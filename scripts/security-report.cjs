#!/usr/bin/env node
/* eslint-disable no-console */
/**
 * AeroFTP self-hosted security audit report.
 *
 * Aggregates output from:
 *   - cargo audit         (Rust crates, RustSec advisory DB)
 *   - npm audit           (Node deps)
 *   - osv-scanner         (Google OSV DB, optional)
 *
 * Produces a single self-contained HTML in docs/security/security-report-<date>.html
 *
 * Usage:
 *   node scripts/security-report.cjs
 *   node scripts/security-report.cjs --open      # open in default browser
 *   node scripts/security-report.cjs --json      # also emit machine-readable JSON
 */

const { execFileSync, spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');

const ROOT = path.resolve(__dirname, '..');
const OUT_DIR = path.join(ROOT, 'docs', 'security');
const TIMESTAMP = new Date();
const DATE_TAG = TIMESTAMP.toISOString().slice(0, 10);
const ARGS = new Set(process.argv.slice(2));

fs.mkdirSync(OUT_DIR, { recursive: true });

function readPkg() {
  return JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8'));
}

function readCargoVersion() {
  const toml = fs.readFileSync(path.join(ROOT, 'src-tauri', 'Cargo.toml'), 'utf8');
  const m = toml.match(/^\s*version\s*=\s*"([^"]+)"/m);
  return m ? m[1] : 'unknown';
}

function readAuditIgnoreList() {
  const auditPath = path.join(ROOT, 'src-tauri', '.cargo', 'audit.toml');
  if (!fs.existsSync(auditPath)) return { ignored: new Set(), path: null };
  const content = fs.readFileSync(auditPath, 'utf8');
  const ids = new Set();
  const re = /"(RUSTSEC-\d{4}-\d{4}|GHSA-[a-z0-9-]+|CVE-\d{4}-\d+)"/gi;
  let m;
  while ((m = re.exec(content)) !== null) ids.add(m[1]);
  return { ignored: ids, path: path.relative(ROOT, auditPath) };
}

function tryRun(cmd, args, opts = {}) {
  const res = spawnSync(cmd, args, {
    cwd: opts.cwd || ROOT,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    env: { ...process.env, FORCE_COLOR: '0', NO_COLOR: '1' },
  });
  return {
    ok: res.status === 0,
    status: res.status,
    stdout: res.stdout || '',
    stderr: res.stderr || '',
    spawnError: res.error ? res.error.message : null,
  };
}

function commandExists(cmd) {
  const r = spawnSync(process.platform === 'win32' ? 'where' : 'which', [cmd], { encoding: 'utf8' });
  return r.status === 0;
}

function runCargoAudit() {
  const tool = 'cargo-audit';
  if (!commandExists(tool) && !commandExists('cargo')) {
    return { tool: 'cargo audit', skipped: true, reason: 'cargo-audit not installed (`cargo install cargo-audit`)' };
  }
  const r = tryRun('cargo', ['audit', '--json'], { cwd: path.join(ROOT, 'src-tauri') });
  if (r.spawnError) {
    return { tool: 'cargo audit', skipped: true, reason: r.spawnError };
  }
  let parsed = null;
  try {
    parsed = JSON.parse(r.stdout);
  } catch {
    return { tool: 'cargo audit', skipped: false, parseError: true, raw: r.stdout || r.stderr };
  }
  const vulns = parsed?.vulnerabilities?.list || [];
  const warnings = (parsed?.warnings && Object.values(parsed.warnings).flat()) || [];
  return {
    tool: 'cargo audit',
    skipped: false,
    counts: {
      vulnerabilities: vulns.length,
      warnings: warnings.length,
    },
    vulns: vulns.map((v) => ({
      id: v.advisory?.id,
      package: v.package?.name,
      version: v.package?.version,
      title: v.advisory?.title,
      url: v.advisory?.url,
      severity: v.advisory?.severity || 'unknown',
      patched: v.versions?.patched || [],
    })),
    warnings: warnings.map((w) => ({
      kind: w.kind,
      package: w.package?.name,
      version: w.package?.version,
      message: w.advisory?.title || w.message || '',
      url: w.advisory?.url || '',
    })),
  };
}

function runNpmAudit() {
  if (!commandExists('npm')) {
    return { tool: 'npm audit', skipped: true, reason: 'npm not installed' };
  }
  const r = tryRun('npm', ['audit', '--json', '--omit=dev']);
  let parsed = null;
  try {
    parsed = JSON.parse(r.stdout);
  } catch {
    return { tool: 'npm audit', skipped: false, parseError: true, raw: r.stdout || r.stderr };
  }
  const meta = parsed?.metadata?.vulnerabilities || {};
  const advisories = [];
  for (const [name, info] of Object.entries(parsed?.vulnerabilities || {})) {
    advisories.push({
      package: name,
      severity: info.severity,
      via: Array.isArray(info.via) ? info.via.map((v) => (typeof v === 'string' ? v : v.title)).filter(Boolean) : [],
      range: info.range,
      fixAvailable: !!info.fixAvailable,
    });
  }
  return {
    tool: 'npm audit',
    skipped: false,
    counts: {
      info: meta.info || 0,
      low: meta.low || 0,
      moderate: meta.moderate || 0,
      high: meta.high || 0,
      critical: meta.critical || 0,
      total: meta.total || 0,
    },
    advisories,
  };
}

function runOsvScanner(ignoreInfo) {
  if (!commandExists('osv-scanner')) {
    return {
      tool: 'osv-scanner',
      skipped: true,
      reason: 'osv-scanner not installed. Install via `go install github.com/google/osv-scanner/v2/cmd/osv-scanner@latest` or download from https://github.com/google/osv-scanner/releases',
    };
  }
  const r = tryRun('osv-scanner', ['scan', 'source', '--format=json', '--recursive', '.']);
  let parsed = null;
  try {
    parsed = JSON.parse(r.stdout);
  } catch {
    return { tool: 'osv-scanner', skipped: false, parseError: true, raw: r.stdout || r.stderr };
  }
  const ignored = ignoreInfo?.ignored || new Set();
  const open = [];
  const suppressed = [];

  for (const result of parsed?.results || []) {
    for (const pkg of result.packages || []) {
      for (const vuln of pkg.vulnerabilities || []) {
        const aliases = new Set([vuln.id, ...(vuln.aliases || [])]);
        const isSuppressed = [...aliases].some((id) => ignored.has(id));
        const cvssScore = vuln.severity?.find((s) => s.type?.startsWith('CVSS'))?.score || '';
        const finding = {
          ecosystem: pkg.package?.ecosystem,
          package: pkg.package?.name,
          version: pkg.package?.version,
          id: vuln.id,
          aliases: vuln.aliases || [],
          summary: vuln.summary,
          severity: cvssScore || 'unknown',
          fixedVersion: extractFixedVersion(vuln),
        };
        (isSuppressed ? suppressed : open).push(finding);
      }
    }
  }

  return {
    tool: 'osv-scanner',
    skipped: false,
    counts: {
      open: open.length,
      suppressed: suppressed.length,
      total: open.length + suppressed.length,
    },
    open,
    suppressed,
    auditTomlPath: ignoreInfo?.path || null,
  };
}

function extractFixedVersion(vuln) {
  for (const aff of vuln.affected || []) {
    for (const range of aff.ranges || []) {
      for (const event of range.events || []) {
        if (event.fixed) return event.fixed;
      }
    }
  }
  return '';
}

function escapeHtml(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function severityClass(s) {
  const v = String(s || '').toLowerCase();
  if (v.includes('critical')) return 'sev-critical';
  if (v.includes('high')) return 'sev-high';
  if (v.includes('moderate') || v.includes('medium')) return 'sev-medium';
  if (v.includes('low')) return 'sev-low';
  return 'sev-info';
}

function renderSection(result) {
  if (result.skipped) {
    return `
      <section class="card skipped">
        <header><h2>${escapeHtml(result.tool)}</h2><span class="badge badge-skip">skipped</span></header>
        <p class="muted">${escapeHtml(result.reason)}</p>
      </section>`;
  }
  if (result.parseError) {
    return `
      <section class="card warn">
        <header><h2>${escapeHtml(result.tool)}</h2><span class="badge badge-warn">parse error</span></header>
        <pre>${escapeHtml((result.raw || '').slice(0, 4000))}</pre>
      </section>`;
  }

  if (result.tool === 'cargo audit') {
    const vulnRows = result.vulns.map((v) => `
      <tr class="${severityClass(v.severity)}">
        <td>${escapeHtml(v.id || '')}</td>
        <td>${escapeHtml(v.package)} <span class="muted">${escapeHtml(v.version)}</span></td>
        <td>${escapeHtml(v.severity)}</td>
        <td>${escapeHtml(v.title || '')}</td>
        <td>${v.url ? `<a href="${escapeHtml(v.url)}" target="_blank" rel="noopener">advisory</a>` : ''}</td>
      </tr>`).join('');
    const warnRows = result.warnings.map((w) => `
      <tr>
        <td>${escapeHtml(w.kind)}</td>
        <td>${escapeHtml(w.package)} <span class="muted">${escapeHtml(w.version)}</span></td>
        <td>${escapeHtml(w.message)}</td>
      </tr>`).join('');
    const isClean = result.counts.vulnerabilities === 0;
    return `
      <section class="card ${isClean ? 'clean' : 'has-issues'}">
        <header>
          <h2>cargo audit <span class="muted">(RustSec advisory DB)</span></h2>
          <span class="badge ${isClean ? 'badge-ok' : 'badge-bad'}">
            ${result.counts.vulnerabilities} vuln · ${result.counts.warnings} warn
          </span>
        </header>
        ${isClean
          ? '<p class="ok">No vulnerabilities found in Rust dependency tree.</p>'
          : `<table><thead><tr><th>ID</th><th>Package</th><th>Severity</th><th>Title</th><th>Link</th></tr></thead><tbody>${vulnRows}</tbody></table>`}
        ${result.warnings.length
          ? `<details><summary>Warnings (${result.warnings.length}) - unmaintained / yanked / deprecated crates</summary>
              <table><thead><tr><th>Kind</th><th>Package</th><th>Note</th></tr></thead><tbody>${warnRows}</tbody></table>
            </details>`
          : ''}
      </section>`;
  }

  if (result.tool === 'npm audit') {
    const c = result.counts;
    const isClean = (c.critical + c.high + c.moderate + c.low) === 0;
    const advRows = result.advisories.map((a) => `
      <tr class="${severityClass(a.severity)}">
        <td>${escapeHtml(a.package)}</td>
        <td>${escapeHtml(a.severity)}</td>
        <td>${escapeHtml(a.range || '')}</td>
        <td>${escapeHtml((a.via || []).join(', '))}</td>
        <td>${a.fixAvailable ? 'yes' : 'no'}</td>
      </tr>`).join('');
    return `
      <section class="card ${isClean ? 'clean' : 'has-issues'}">
        <header>
          <h2>npm audit <span class="muted">(production deps)</span></h2>
          <span class="badge ${isClean ? 'badge-ok' : 'badge-bad'}">
            ${c.critical} crit · ${c.high} high · ${c.moderate} mod · ${c.low} low
          </span>
        </header>
        ${isClean
          ? '<p class="ok">No vulnerabilities found in production npm dependencies.</p>'
          : `<table><thead><tr><th>Package</th><th>Severity</th><th>Range</th><th>Via</th><th>Fix?</th></tr></thead><tbody>${advRows}</tbody></table>`}
      </section>`;
  }

  if (result.tool === 'osv-scanner') {
    const c = result.counts;
    const isClean = c.open === 0;
    const renderRow = (f) => `
      <tr class="${severityClass(f.severity)}">
        <td>${escapeHtml(f.ecosystem)}</td>
        <td>${escapeHtml(f.package)} <span class="muted">${escapeHtml(f.version)}</span></td>
        <td><a href="https://osv.dev/vulnerability/${escapeHtml(f.id)}" target="_blank" rel="noopener">${escapeHtml(f.id)}</a></td>
        <td>${escapeHtml(f.severity || '')}</td>
        <td>${escapeHtml(f.fixedVersion || '')}</td>
        <td>${escapeHtml(f.summary || '')}</td>
      </tr>`;
    const openRows = result.open.map(renderRow).join('');
    const suppRows = result.suppressed.map(renderRow).join('');
    const auditLink = result.auditTomlPath
      ? `<a href="../../${escapeHtml(result.auditTomlPath)}">${escapeHtml(result.auditTomlPath)}</a>`
      : '<code>src-tauri/.cargo/audit.toml</code>';
    return `
      <section class="card ${isClean ? 'clean' : 'has-issues'}">
        <header>
          <h2>osv-scanner <span class="muted">(Google OSV DB - includes RustSec, GHSA, CVE)</span></h2>
          <span class="badge ${isClean ? 'badge-ok' : 'badge-bad'}">
            ${c.open} open · ${c.suppressed} suppressed
          </span>
        </header>
        ${isClean
          ? '<p class="ok">No open findings. All known advisories in scope are documented in the audit.toml suppression list with written rationales.</p>'
          : `<p class="muted">Open findings are advisories not yet present in ${auditLink}. Review and either bump the dependency or add a justified suppression.</p>
             <table><thead><tr><th>Ecosystem</th><th>Package</th><th>ID</th><th>CVSS</th><th>Fixed in</th><th>Summary</th></tr></thead><tbody>${openRows}</tbody></table>`}
        ${result.suppressed.length
          ? `<details><summary>Suppressed via ${auditLink} (${result.suppressed.length}) - documented rationale required</summary>
              <table><thead><tr><th>Ecosystem</th><th>Package</th><th>ID</th><th>CVSS</th><th>Fixed in</th><th>Summary</th></tr></thead><tbody>${suppRows}</tbody></table>
            </details>`
          : ''}
      </section>`;
  }

  return '';
}

function buildHtml({ pkg, cargoVersion, results }) {
  const totalIssues = results.reduce((acc, r) => {
    if (r.skipped || r.parseError) return acc;
    if (r.tool === 'cargo audit') return acc + (r.counts?.vulnerabilities || 0);
    if (r.tool === 'npm audit') {
      const c = r.counts || {};
      return acc + (c.critical || 0) + (c.high || 0) + (c.moderate || 0) + (c.low || 0);
    }
    if (r.tool === 'osv-scanner') return acc + (r.counts?.open || 0);
    return acc;
  }, 0);

  const totalSuppressed = results.reduce((acc, r) => {
    if (r.skipped || r.parseError) return acc;
    if (r.tool === 'osv-scanner') return acc + (r.counts?.suppressed || 0);
    return acc;
  }, 0);

  const overallClass = totalIssues === 0 ? 'all-clean' : 'has-findings';
  const overallLabel = totalIssues === 0
    ? 'No open vulnerabilities'
    : `${totalIssues} open finding(s)`;
  const subLabel = totalSuppressed > 0
    ? `${totalSuppressed} additional advisory(ies) suppressed via documented audit.toml rationale`
    : 'aggregated across cargo audit, npm audit, and osv-scanner';

  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>AeroFTP Security Report - ${escapeHtml(DATE_TAG)}</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root {
    --bg: #0f172a; --panel: #1e293b; --text: #e2e8f0; --muted: #94a3b8;
    --ok: #22c55e; --bad: #ef4444; --warn: #f59e0b; --info: #3b82f6;
    --border: #334155;
  }
  @media (prefers-color-scheme: light) {
    :root { --bg: #f8fafc; --panel: #fff; --text: #0f172a; --muted: #64748b; --border: #e2e8f0; }
  }
  * { box-sizing: border-box; }
  body { margin: 0; font: 14px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;
         background: var(--bg); color: var(--text); }
  .wrap { max-width: 1100px; margin: 0 auto; padding: 32px 20px 64px; }
  h1 { margin: 0 0 4px; font-size: 28px; }
  h2 { margin: 0; font-size: 18px; }
  .meta { color: var(--muted); font-size: 13px; margin-bottom: 24px; }
  .summary { display: flex; gap: 16px; align-items: center; padding: 20px;
             border-radius: 12px; margin-bottom: 24px; border: 1px solid var(--border); }
  .summary.all-clean { background: rgba(34,197,94,.10); border-color: rgba(34,197,94,.35); }
  .summary.has-findings { background: rgba(239,68,68,.10); border-color: rgba(239,68,68,.35); }
  .summary .label { font-size: 22px; font-weight: 700; }
  .summary.all-clean .label::before { content: "OK "; color: var(--ok); }
  .summary.has-findings .label::before { content: "WARN "; color: var(--bad); }
  .card { background: var(--panel); border: 1px solid var(--border); border-radius: 12px;
          padding: 18px 20px; margin-bottom: 16px; }
  .card.clean { border-left: 4px solid var(--ok); }
  .card.has-issues { border-left: 4px solid var(--bad); }
  .card.skipped { border-left: 4px solid var(--muted); opacity: .8; }
  .card.warn { border-left: 4px solid var(--warn); }
  .card header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 12px; }
  .badge { display: inline-block; padding: 4px 10px; border-radius: 999px; font-size: 12px; font-weight: 600; }
  .badge-ok { background: rgba(34,197,94,.18); color: var(--ok); }
  .badge-bad { background: rgba(239,68,68,.18); color: var(--bad); }
  .badge-skip { background: rgba(148,163,184,.20); color: var(--muted); }
  .badge-warn { background: rgba(245,158,11,.18); color: var(--warn); }
  .muted { color: var(--muted); font-weight: 400; font-size: 13px; }
  .ok { color: var(--ok); margin: 6px 0 0; font-weight: 500; }
  table { width: 100%; border-collapse: collapse; margin-top: 8px; font-size: 13px; }
  th, td { text-align: left; padding: 8px 10px; border-bottom: 1px solid var(--border); vertical-align: top; }
  th { color: var(--muted); font-weight: 600; }
  tr.sev-critical td:nth-child(3), tr.sev-critical td:nth-child(2) { color: var(--bad); font-weight: 600; }
  tr.sev-high td:nth-child(3) { color: var(--bad); }
  tr.sev-medium td:nth-child(3) { color: var(--warn); }
  tr.sev-low td:nth-child(3) { color: var(--info); }
  details { margin-top: 12px; }
  summary { cursor: pointer; color: var(--muted); }
  pre { background: var(--bg); padding: 12px; border-radius: 8px; overflow-x: auto; font-size: 12px; }
  footer { margin-top: 32px; color: var(--muted); font-size: 12px; text-align: center; }
  a { color: var(--info); }
</style>
</head>
<body>
<div class="wrap">
  <h1>AeroFTP Security Report</h1>
  <div class="meta">
    Frontend version: <strong>${escapeHtml(pkg.version)}</strong> ·
    Backend (src-tauri) version: <strong>${escapeHtml(cargoVersion)}</strong> ·
    Generated: ${escapeHtml(TIMESTAMP.toISOString())} ·
    Host: ${escapeHtml(os.hostname())} ${escapeHtml(process.platform)}-${escapeHtml(process.arch)}
  </div>

  <div class="summary ${overallClass}">
    <div>
      <div class="label">${escapeHtml(overallLabel)}</div>
      <div class="muted">${escapeHtml(subLabel)}</div>
    </div>
  </div>

  ${results.map(renderSection).join('\n')}

  <footer>
    Generated by <code>scripts/security-report.cjs</code> - self-hosted, no third-party dependency.
    Data sources: <a href="https://rustsec.org/">RustSec</a>,
    <a href="https://docs.npmjs.com/cli/v10/commands/npm-audit">npm registry</a>,
    <a href="https://osv.dev/">OSV</a>.
  </footer>
</div>
</body>
</html>`;
}

(function main() {
  const pkg = readPkg();
  const cargoVersion = readCargoVersion();
  const ignoreInfo = readAuditIgnoreList();

  console.log(`Running security audit for AeroFTP ${pkg.version} (backend ${cargoVersion})...`);
  if (ignoreInfo.path) {
    console.log(`  Suppression list: ${ignoreInfo.path} (${ignoreInfo.ignored.size} entries)`);
  }

  const results = [];
  console.log('  - cargo audit');
  results.push(runCargoAudit());
  console.log('  - npm audit');
  results.push(runNpmAudit());
  console.log('  - osv-scanner');
  results.push(runOsvScanner(ignoreInfo));

  const html = buildHtml({ pkg, cargoVersion, results });
  const htmlPath = path.join(OUT_DIR, `security-report-${DATE_TAG}.html`);
  fs.writeFileSync(htmlPath, html, 'utf8');
  const latestPath = path.join(OUT_DIR, 'security-report-latest.html');
  fs.copyFileSync(htmlPath, latestPath);
  console.log(`\nHTML report written: ${path.relative(ROOT, htmlPath)}`);
  console.log(`Latest symlink:      ${path.relative(ROOT, latestPath)}`);

  if (ARGS.has('--json')) {
    const jsonPath = path.join(OUT_DIR, `security-report-${DATE_TAG}.json`);
    fs.writeFileSync(jsonPath, JSON.stringify({
      version: pkg.version,
      cargoVersion,
      generatedAt: TIMESTAMP.toISOString(),
      results,
    }, null, 2));
    console.log(`JSON report written: ${path.relative(ROOT, jsonPath)}`);
  }

  if (ARGS.has('--open')) {
    const opener = process.platform === 'darwin' ? 'open'
                 : process.platform === 'win32' ? 'start'
                 : 'xdg-open';
    try { execFileSync(opener, [htmlPath], { stdio: 'ignore' }); } catch { /* ignore */ }
  }

  const totalIssues = results.reduce((acc, r) => {
    if (r.skipped || r.parseError) return acc;
    if (r.tool === 'cargo audit') return acc + (r.counts?.vulnerabilities || 0);
    if (r.tool === 'npm audit') {
      const c = r.counts || {};
      return acc + (c.critical || 0) + (c.high || 0) + (c.moderate || 0) + (c.low || 0);
    }
    if (r.tool === 'osv-scanner') return acc + (r.counts?.open || 0);
    return acc;
  }, 0);

  const totalSuppressed = results.reduce((acc, r) => {
    if (r.tool === 'osv-scanner' && !r.skipped && !r.parseError) {
      return acc + (r.counts?.suppressed || 0);
    }
    return acc;
  }, 0);

  console.log(`\nOpen findings:       ${totalIssues}`);
  console.log(`Suppressed (justified): ${totalSuppressed}`);
  process.exit(0);
})();
