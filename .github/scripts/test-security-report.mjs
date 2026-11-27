// The self-hosted security report counts one OSV finding per package version
// and advisory. osv-scanner returns a record per lockfile, so the same crate at
// the same version in two lockfiles must not be counted (or listed) twice.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const { collectOsvFindings, buildMarkdown } = require('../../scripts/security-report.cjs');
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');

const vuln = (id, aliases = []) => ({ id, aliases, summary: `${id} summary` });
const lockfile = (rel, packages) => ({ source: { path: path.join(ROOT, rel) }, packages });
const pkg = (name, version, vulns) => ({ package: { ecosystem: 'crates.io', name, version }, vulnerabilities: vulns });

test('a package version seen in two lockfiles is one finding with both lockfiles', () => {
  const parsed = {
    results: [
      lockfile('src-tauri/Cargo.lock', [pkg('paste', '1.0.15', [vuln('RUSTSEC-2024-0436')])]),
      lockfile('src-tauri/peer-l0/Cargo.lock', [pkg('paste', '1.0.15', [vuln('RUSTSEC-2024-0436')])]),
    ],
  };
  const { open, suppressed } = collectOsvFindings(parsed, new Set(['RUSTSEC-2024-0436']));
  assert.equal(open.length, 0);
  assert.equal(suppressed.length, 1);
  assert.deepEqual(suppressed[0].lockfiles, [path.join('src-tauri', 'Cargo.lock'), path.join('src-tauri', 'peer-l0', 'Cargo.lock')]);
});

test('a different version or a different advisory stays a separate finding', () => {
  const parsed = {
    results: [
      lockfile('a/Cargo.lock', [pkg('mio', '1.2.1', [vuln('RUSTSEC-X'), vuln('RUSTSEC-Y')])]),
      lockfile('b/Cargo.lock', [pkg('mio', '1.2.2', [vuln('RUSTSEC-X')])]),
    ],
  };
  const { open } = collectOsvFindings(parsed, new Set());
  assert.deepEqual(open.map((f) => `${f.version} ${f.id}`).sort(), ['1.2.1 RUSTSEC-X', '1.2.1 RUSTSEC-Y', '1.2.2 RUSTSEC-X']);
});

test('a versionless package is one finding per commit', () => {
  const sub = (commit) => ({ package: { commit }, vulnerabilities: [vuln('OSV-2026-1')] });
  const parsed = {
    results: [
      { source: { path: ROOT }, packages: [sub('aaaa'), sub('bbbb')] },
      { source: { path: ROOT }, packages: [sub('aaaa')] },
    ],
  };
  const { open } = collectOsvFindings(parsed, new Set());
  assert.deepEqual(open.map((f) => f.version).sort(), ['aaaa', 'bbbb']);
});

test('an ignore that matches an alias suppresses the finding', () => {
  const parsed = { results: [lockfile('Cargo.lock', [pkg('glib', '0.18.5', [vuln('GHSA-wrw7-89jp-8q8g', ['RUSTSEC-2024-0429'])])])] };
  const { open, suppressed } = collectOsvFindings(parsed, new Set(['RUSTSEC-2024-0429']));
  assert.equal(open.length, 0);
  assert.equal(suppressed.length, 1);
});

test('a relative lockfile path from the scanner is read against the repository root', () => {
  const parsed = { results: [{ source: { path: path.join('src-tauri', 'Cargo.lock') }, packages: [pkg('paste', '1.0.15', [vuln('RUSTSEC-2024-0436')])] }] };
  const before = process.cwd();
  process.chdir(os.tmpdir());
  try {
    const { open } = collectOsvFindings(parsed, new Set());
    assert.deepEqual(open[0].lockfiles, [path.join('src-tauri', 'Cargo.lock')]);
  } finally {
    process.chdir(before);
  }
});

test('buildMarkdown renders one open and one suppressed finding with GitHub-safe links', () => {
  const finding = (id, summary) => ({
    ecosystem: 'crates.io',
    package: 'demo-crate',
    version: '1.2.3',
    id,
    aliases: [],
    summary,
    severity: 'CVSS:3.1/AV:N/AC:L',
    fixedVersion: '1.2.4',
    lockfiles: [path.join('src-tauri', 'Cargo.lock')],
    suppressed: false,
  });
  const results = [
    { tool: 'cargo audit', skipped: false, counts: { vulnerabilities: 0, warnings: 0 }, vulns: [], warnings: [] },
    { tool: 'npm audit', skipped: false, counts: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 0 }, advisories: [] },
    {
      tool: 'osv-scanner',
      skipped: false,
      counts: { open: 1, suppressed: 1, total: 2 },
      open: [{ ...finding('RUSTSEC-2026-9999', 'open | finding summary'), suppressed: false }],
      suppressed: [{ ...finding('RUSTSEC-2026-8888', 'suppressed finding'), suppressed: true }],
    },
  ];
  const md = buildMarkdown({
    pkg: { version: '4.2.8' },
    cargoVersion: '4.2.8',
    results,
    versions: { 'cargo audit': '0.22.2', 'npm audit': '11.19.0', 'osv-scanner': '2.3.8' },
  });

  // The report lives in docs/security/, so repo-root links need ../../ on GitHub.
  assert.ok(md.includes('](../../src-tauri/.cargo/audit.toml)'), 'audit.toml links must be relative to docs/security/');
  // Open and suppressed findings each get a table row with the osv.dev link.
  assert.ok(md.includes('[RUSTSEC-2026-9999](https://osv.dev/vulnerability/RUSTSEC-2026-9999)'));
  assert.ok(md.includes('[RUSTSEC-2026-8888](https://osv.dev/vulnerability/RUSTSEC-2026-8888)'));
  assert.ok(md.includes('1 open, 1 suppressed'));
  // Pipe characters in tool output must be escaped inside table cells.
  assert.ok(md.includes('open \\| finding summary'));
  // Public Markdown: no em-dash, no local hostname.
  assert.ok(!md.includes('\u2014'), 'no U+2014 in the Markdown report');
  assert.ok(!md.includes(os.hostname()), 'no hostname in the Markdown report');
  assert.ok(md.includes(`Platform: ${process.platform}-${process.arch}`));
});

const cleanTool = (tool) => tool === 'cargo audit'
  ? { tool, skipped: false, counts: { vulnerabilities: 0, warnings: 0 }, vulns: [], warnings: [] }
  : tool === 'npm audit'
    ? { tool, skipped: false, counts: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 0 }, advisories: [] }
    : { tool, skipped: false, counts: { open: 0, suppressed: 0, total: 0 }, open: [], suppressed: [] };

test('buildMarkdown reports an incomplete scan when a tool is skipped or fails to parse', () => {
  const base = { pkg: { version: '4.2.8' }, cargoVersion: '4.2.8', versions: {} };
  for (const broken of [
    { tool: 'osv-scanner', skipped: true, reason: 'osv-scanner not installed' },
    { tool: 'osv-scanner', skipped: false, parseError: true, raw: 'boom' },
  ]) {
    const md = buildMarkdown({ ...base, results: [cleanTool('cargo audit'), cleanTool('npm audit'), broken] });
    assert.ok(md.includes('**Incomplete scan**: 1 of 3 tools did not complete (osv-scanner)'), 'must flag the incomplete scan');
    assert.ok(md.includes('No open vulnerabilities among the tools that ran.'), 'keeps the known clean counts');
    assert.ok(!md.includes('No open vulnerabilities** across'), 'must not claim a full clean scan');
  }
});

test('buildMarkdown claims a full clean scan only when all three tools ran clean', () => {
  const md = buildMarkdown({
    pkg: { version: '4.2.8' },
    cargoVersion: '4.2.8',
    versions: {},
    results: [cleanTool('cargo audit'), cleanTool('npm audit'), cleanTool('osv-scanner')],
  });
  assert.ok(md.includes('**No open vulnerabilities** across cargo audit, npm audit, and osv-scanner.'));
  assert.ok(!md.includes('Incomplete scan'));
});

test('buildMarkdown never emits an em-dash, even from external advisory text', () => {
  const md = buildMarkdown({
    pkg: { version: '4.2.8' },
    cargoVersion: '4.2.8',
    versions: {},
    results: [{
      tool: 'osv-scanner',
      skipped: false,
      counts: { open: 1, suppressed: 0, total: 1 },
      open: [{
        ecosystem: 'crates.io', package: 'demo', version: '1.0.0', id: 'RUSTSEC-2026-7777',
        aliases: [], summary: `title with \u2014 em-dash`, severity: '', fixedVersion: '', lockfiles: [], suppressed: false,
      }],
      suppressed: [],
    }],
  });
  assert.ok(!md.includes('\u2014'), 'no U+2014 in the generated Markdown');
  assert.ok(md.includes('title with  -  em-dash'), 'em-dash normalized to a hyphen separator');
});
