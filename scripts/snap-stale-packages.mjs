#!/usr/bin/env node
/**
 * Audit a published snap revision against the live Ubuntu archive.
 *
 * A snap freezes every `stage-packages` dependency at build time, so a snap that
 * is only rebuilt on release tags slowly accumulates known-vulnerable libraries
 * between releases. That is what the Snap Store "contains outdated Ubuntu
 * packages" mails are about (e.g. USN-8584-1 on r216, 2026-07-22).
 *
 * This script answers the question those mails ask, before they are sent:
 * for every entry of `primed-stage-packages` in the snap's own
 * `snap/manifest.yaml`, is a newer version published in the archive for the
 * suite the snap's base pins (core22 -> jammy)?
 *
 * Usage:
 *   node scripts/snap-stale-packages.mjs                       # audit latest/stable
 *   node scripts/snap-stale-packages.mjs --snap ./aeroftp.snap
 *   node scripts/snap-stale-packages.mjs --name aeroftp --channel candidate
 *   node scripts/snap-stale-packages.mjs --json
 *   node scripts/snap-stale-packages.mjs --self-test
 *
 * Exit code is 0 whether or not packages are stale (staleness is data, not an
 * error); it is non-zero only when the audit itself could not be performed.
 * When $GITHUB_OUTPUT is set it writes `stale=true|false` and `count=<n>`.
 */

import { execFileSync } from 'node:child_process';
import { gunzipSync } from 'node:zlib';
import { mkdtempSync, rmSync, readFileSync, readdirSync, appendFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const MIRROR = process.env.UBUNTU_MIRROR ?? 'http://archive.ubuntu.com/ubuntu';
const COMPONENTS = ['main', 'universe', 'restricted', 'multiverse'];
const POCKETS = ['', '-updates', '-security'];
const BASE_SUITE = {
  core: 'xenial',
  core18: 'bionic',
  core20: 'focal',
  core22: 'jammy',
  core24: 'noble',
};

// ---------------------------------------------------------------------------
// dpkg version comparison (deb-version(7)), faithful to dpkg's verrevcmp().
// Implemented here rather than shelling out to `dpkg --compare-versions`: the
// archive indexes hold ~1.5 M entries, and one process per comparison would
// turn a 20-second audit into a coffee break.
// ---------------------------------------------------------------------------
const isDigit = (ch) => ch >= '0' && ch <= '9';

/** dpkg's character ordering: '~' first, then digits, letters, everything else. */
function order(ch) {
  if (ch === undefined) return 0; // end of string
  if (isDigit(ch)) return 0;
  if (ch === '~') return -1;
  const code = ch.charCodeAt(0);
  const isAlpha = (ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z');
  return isAlpha ? code : code + 256;
}

function verrevcmp(a, b) {
  let i = 0;
  let j = 0;
  while (i < a.length || j < b.length) {
    let firstDiff = 0;

    while ((i < a.length && !isDigit(a[i])) || (j < b.length && !isDigit(b[j]))) {
      const ac = order(a[i]);
      const bc = order(b[j]);
      if (ac !== bc) return ac - bc;
      i += 1;
      j += 1;
    }

    while (a[i] === '0') i += 1;
    while (b[j] === '0') j += 1;

    while (isDigit(a[i]) && isDigit(b[j])) {
      if (firstDiff === 0) firstDiff = a.charCodeAt(i) - b.charCodeAt(j);
      i += 1;
      j += 1;
    }

    if (isDigit(a[i])) return 1;
    if (isDigit(b[j])) return -1;
    if (firstDiff !== 0) return firstDiff;
  }
  return 0;
}

function splitVersion(version) {
  let rest = version;
  let epoch = 0;
  const colon = rest.indexOf(':');
  if (colon !== -1 && /^\d+$/.test(rest.slice(0, colon))) {
    epoch = Number.parseInt(rest.slice(0, colon), 10);
    rest = rest.slice(colon + 1);
  }
  const dash = rest.lastIndexOf('-');
  const upstream = dash === -1 ? rest : rest.slice(0, dash);
  const revision = dash === -1 ? '' : rest.slice(dash + 1);
  return { epoch, upstream, revision };
}

export function compareDebVersions(v1, v2) {
  const a = splitVersion(v1);
  const b = splitVersion(v2);
  if (a.epoch !== b.epoch) return a.epoch - b.epoch;
  const up = verrevcmp(a.upstream, b.upstream);
  if (up !== 0) return up;
  return verrevcmp(a.revision, b.revision);
}

// ---------------------------------------------------------------------------
// Archive indexes
// ---------------------------------------------------------------------------

/** Parse a Packages file into the newest version seen per binary package. */
function absorbPackages(text, suiteLabel, best) {
  let pos = 0;
  let name = null;
  let version = null;
  const len = text.length;

  while (pos < len) {
    let eol = text.indexOf('\n', pos);
    if (eol === -1) eol = len;

    if (text.startsWith('Package: ', pos)) {
      name = text.slice(pos + 9, eol).trim();
    } else if (text.startsWith('Version: ', pos)) {
      version = text.slice(pos + 9, eol).trim();
    } else if (eol === pos) {
      // blank line: end of stanza
      if (name && version) {
        const current = best.get(name);
        if (!current || compareDebVersions(version, current.version) > 0) {
          best.set(name, { version, suite: suiteLabel });
        }
      }
      name = null;
      version = null;
    }
    pos = eol + 1;
  }

  if (name && version) {
    const current = best.get(name);
    if (!current || compareDebVersions(version, current.version) > 0) {
      best.set(name, { version, suite: suiteLabel });
    }
  }
}

async function fetchArchiveIndex(suite, arch) {
  const best = new Map();
  const jobs = [];
  for (const pocket of POCKETS) {
    for (const component of COMPONENTS) {
      const label = `${suite}${pocket}/${component}`;
      const url = `${MIRROR}/dists/${suite}${pocket}/${component}/binary-${arch}/Packages.gz`;
      jobs.push(
        (async () => {
          const res = await fetch(url);
          if (!res.ok) {
            // A pocket/component can legitimately be absent (e.g. no
            // -security/multiverse for some suites); only a total miss matters.
            if (res.status === 404) return null;
            throw new Error(`GET ${url} -> HTTP ${res.status}`);
          }
          const gz = Buffer.from(await res.arrayBuffer());
          return { label, text: gunzipSync(gz).toString('utf8') };
        })(),
      );
    }
  }

  const results = await Promise.all(jobs);
  let loaded = 0;
  for (const result of results) {
    if (!result) continue;
    absorbPackages(result.text, result.label, best);
    loaded += 1;
  }
  if (loaded === 0) throw new Error(`no archive index could be fetched for ${suite}/${arch}`);
  return { best, loaded };
}

// ---------------------------------------------------------------------------
// The snap under audit
// ---------------------------------------------------------------------------

function sh(cmd, args, opts = {}) {
  return execFileSync(cmd, args, { encoding: 'utf8', ...opts });
}

function readSnapMetadata(snapFile, workDir) {
  sh('unsquashfs', ['-q', '-n', '-f', '-d', join(workDir, 'x'), snapFile, 'snap/manifest.yaml', 'meta/snap.yaml']);
  const manifest = readFileSync(join(workDir, 'x', 'snap', 'manifest.yaml'), 'utf8');
  let snapYaml = '';
  try {
    snapYaml = readFileSync(join(workDir, 'x', 'meta', 'snap.yaml'), 'utf8');
  } catch {
    /* optional */
  }

  const base = (manifest.match(/^base:\s*(\S+)/m) ?? snapYaml.match(/^base:\s*(\S+)/m) ?? [])[1];
  const version = (manifest.match(/^version:\s*['"]?([^'"\n]+)/m) ?? [])[1];
  const arch =
    (snapYaml.match(/^architectures:\s*\n\s*-\s*(\S+)/m) ?? [])[1] ??
    (manifest.match(/^architectures:\s*\n\s*-\s*(\S+)/m) ?? [])[1] ??
    'amd64';

  // `primed-stage-packages` is the flat, deduplicated list of everything that
  // actually made it into the squashfs - the same list the Store scanner reads.
  const block = manifest.match(/^primed-stage-packages:\n((?:-\s.*\n)+)/m);
  if (!block) throw new Error('no primed-stage-packages block in snap/manifest.yaml');
  const packages = block[1]
    .split('\n')
    .filter((line) => line.startsWith('- '))
    .map((line) => {
      const entry = line.slice(2).trim();
      const eq = entry.lastIndexOf('=');
      return { name: entry.slice(0, eq), version: entry.slice(eq + 1) };
    });

  return { base, version, arch, packages };
}

// ---------------------------------------------------------------------------
// Self-test: dpkg's own documented ordering plus the versions this audit hinges
// on. Cheap insurance for a comparator that is easy to get subtly wrong.
// ---------------------------------------------------------------------------
function selfTest() {
  const cases = [
    ['1.0', '1.0', 0],
    ['1.0', '1.1', -1],
    ['1.10', '1.9', 1],
    ['1.0~beta', '1.0', -1],
    ['1.0~beta1', '1.0~beta2', -1],
    ['1:1.0', '2.0', 1],
    ['1.0-1', '1.0-2', -1],
    ['1.20.3-0ubuntu1.7', '1.20.3-0ubuntu1.1', 1],
    ['1.20.3-0ubuntu1.7', '1.20.3-0ubuntu1.7', 0],
    ['1.20.3-0ubuntu1.10', '1.20.3-0ubuntu1.9', 1],
    ['2.37-2build1', '2.37-2build2', -1],
    ['1.24.2-1ubuntu1.5', '1.20.3-0ubuntu1.7', 1],
    ['41.0-1ubuntu1', '41.0-1ubuntu1', 0],
  ];
  let failed = 0;
  for (const [a, b, expected] of cases) {
    const got = Math.sign(compareDebVersions(a, b));
    if (got !== expected) {
      console.error(`FAIL  ${a} vs ${b}: expected ${expected}, got ${got}`);
      failed += 1;
    }
  }
  if (failed > 0) {
    console.error(`${failed}/${cases.length} version-comparison cases failed`);
    process.exit(1);
  }
  console.log(`self-test OK (${cases.length} version-comparison cases)`);
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------
async function main() {
  const argv = process.argv.slice(2);
  const flag = (name, fallback = undefined) => {
    const i = argv.indexOf(name);
    return i === -1 ? fallback : argv[i + 1];
  };

  if (argv.includes('--self-test')) {
    selfTest();
    return;
  }
  selfTest();

  const asJson = argv.includes('--json');
  const snapName = flag('--name', 'aeroftp');
  const channel = flag('--channel', 'stable');
  let snapFile = flag('--snap');

  const workDir = mkdtempSync(join(tmpdir(), 'snap-audit-'));
  try {
    if (!snapFile) {
      console.log(`Downloading ${snapName} from latest/${channel}...`);
      sh('snap', ['download', snapName, `--channel=${channel}`], { cwd: workDir, stdio: 'inherit' });
      const found = readdirSync(workDir).find((f) => f.endsWith('.snap'));
      if (!found) throw new Error(`snap download produced no .snap for ${snapName}`);
      snapFile = join(workDir, found);
    }

    const { base, version, arch, packages } = readSnapMetadata(snapFile, workDir);
    const suite = BASE_SUITE[base];
    if (!suite) throw new Error(`unknown snap base '${base}' - extend BASE_SUITE`);

    console.log(`Snap:      ${snapFile}`);
    console.log(`Version:   ${version}  (base ${base} -> ${suite}, ${arch})`);
    console.log(`Packages:  ${packages.length} primed stage packages`);

    const { best, loaded } = await fetchArchiveIndex(suite, arch);
    console.log(`Archive:   ${best.size} binary packages from ${loaded} index files`);

    const stale = [];
    const unknown = [];
    for (const pkg of packages) {
      const candidate = best.get(pkg.name);
      if (!candidate) {
        unknown.push(pkg);
        continue;
      }
      if (compareDebVersions(candidate.version, pkg.version) > 0) {
        stale.push({ ...pkg, archive: candidate.version, suite: candidate.suite });
      }
    }

    stale.sort((a, b) => a.name.localeCompare(b.name));

    if (asJson) {
      console.log(JSON.stringify({ snap: snapName, version, base, suite, stale, unknown }, null, 2));
    } else {
      console.log('');
      if (stale.length === 0) {
        console.log(`OK: all ${packages.length} staged packages are at the newest archive version.`);
      } else {
        console.log(`STALE: ${stale.length} of ${packages.length} staged packages have newer archive versions`);
        for (const s of stale) {
          console.log(`  ${s.name}: snap=${s.version}  archive=${s.archive}  (${s.suite})`);
        }
      }
      if (unknown.length > 0) {
        console.log(`\nNot found in the archive indexes (${unknown.length}):`);
        for (const u of unknown) console.log(`  ${u.name}=${u.version}`);
      }
    }

    if (process.env.GITHUB_OUTPUT) {
      appendFileSync(
        process.env.GITHUB_OUTPUT,
        `stale=${stale.length > 0}\ncount=${stale.length}\nversion=${version}\n`,
      );
    }
    if (process.env.GITHUB_STEP_SUMMARY) {
      const lines = [
        `### Snap package audit - ${snapName} ${version} (base ${base}/${suite})`,
        '',
        stale.length === 0
          ? `All **${packages.length}** staged packages are current.`
          : `**${stale.length}** stale package(s) out of ${packages.length}:`,
        ...stale.map((s) => `- \`${s.name}\` snap \`${s.version}\` -> archive \`${s.archive}\` (${s.suite})`),
      ];
      appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${lines.join('\n')}\n`);
    }
  } finally {
    rmSync(workDir, { recursive: true, force: true });
  }
}

// Only audit when run directly, so `compareDebVersions` can be imported by a
// differential test against dpkg itself.
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((err) => {
    console.error(`::error::snap package audit failed: ${err.message}`);
    process.exit(2);
  });
}
