#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * One list of the files that carry the application version, and one comparison.
 *
 * This exists because the list was duplicated. The local pre-push gate compared
 * EIGHT sites; the R10 step in build.yml, which is the one that runs by itself on
 * every pull request and on every tag, compared FOUR and not the lockfile. Two
 * checks sharing a name, with different lists, and the narrower one being the
 * automatic one: v4.1.9 was tagged with `package-lock.json` left a version behind
 * in both of its fields, and main was green with the drift on it. 148 of 166 tags
 * carry the same drift, so it was never a one-off oversight.
 *
 * The fix is not another guard. It is that the list lives in one place and every
 * caller reads THIS file: the pre-push gate, checks.yml on every pull request,
 * and build.yml's R10 step, which adds the tag comparison on tag builds.
 *
 * Why the version is compared against `src-tauri/Cargo.toml` rather than a
 * majority vote: the crate version is what the shipped binary reports, so it is
 * the one value a user can read back from the product. A vote would let two stale
 * files outvote the truth.
 *
 * What this cannot see, stated so nobody reads more into a green than it means:
 *  - It compares declared version strings. It does not verify that the built
 *    artifacts carry them; `cli-smoke.yml` does that for the CLI binary.
 *  - A site that does not exist in the list is not COMPARED, but it can no longer
 *    pass unnoticed: the discovery pass at the end asks the tree which files
 *    declare this version and fails on any that is neither compared nor accepted
 *    with a reason. Stopping at an explicit list would have reproduced, one level
 *    up, the very defect this script exists to fix, since a list cannot report the
 *    member nobody added to it. That pass is what found two such files the first
 *    version of this list did not have.
 *  - `npm ci` does NOT fail on a lock whose version field disagrees. That claim
 *    was in the gate's own comment and does not reproduce: on the v4.1.9 tag it
 *    exits 0 and installs 306 packages, because npm validates the dependency tree
 *    and not this field. The drift is real, the consequence claimed for npm is not,
 *    and this check is the thing that catches it.
 *
 * Usage:
 *   node scripts/check-version-sites.mjs              compare the sites to each other
 *   node scripts/check-version-sites.mjs --tag v4.2.0 also require them to equal the tag
 *
 * Exit 0 when every site agrees, 1 on any disagreement and on any site that
 * cannot be parsed. An unreadable site is a failure, never a skip: a guard that
 * cannot tell "absent" from "equal" is the defect it is supposed to prevent.
 */

import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const inCI = !!process.env.GITHUB_ACTIONS;

const read = (rel) => readFileSync(join(ROOT, rel), 'utf8');

/** The [package] version of a Cargo manifest. Anchored inside the section rather
 *  than taking the first `version =` in the file, so a `[dependencies]` entry
 *  written on its own line can never be read as the crate version. */
const cargoTomlVersion = (rel) => {
  const text = read(rel);
  const section = text.split(/^\[/m).find((s) => s.startsWith('package]'));
  if (!section) throw new Error(`${rel}: no [package] section`);
  const m = section.match(/^\s*version\s*=\s*"([^"]+)"/m);
  if (!m) throw new Error(`${rel}: no version in [package]`);
  return m[1];
};

/** The version of one package inside a Cargo.lock. Parsed by walking the
 *  [[package]] blocks instead of matching name and version as adjacent lines:
 *  the field order inside a block is not guaranteed, and a regex that assumes it
 *  returns empty rather than wrong, which would read as "no drift". */
const cargoLockVersion = (rel, name) => {
  const blocks = read(rel).split(/^\[\[package\]\]$/m).slice(1);
  for (const block of blocks) {
    const n = block.match(/^\s*name\s*=\s*"([^"]+)"/m);
    if (n && n[1] === name) {
      const v = block.match(/^\s*version\s*=\s*"([^"]+)"/m);
      if (!v) throw new Error(`${rel}: package ${name} has no version`);
      return v[1];
    }
  }
  throw new Error(`${rel}: no [[package]] named ${name}`);
};

const jsonVersion = (rel, pick) => {
  const value = pick(JSON.parse(read(rel)));
  if (typeof value !== 'string' || !value) throw new Error(`${rel}: version is not a string`);
  return value;
};

const byRegex = (rel, re, what) => {
  const m = read(rel).match(re);
  if (!m) throw new Error(`${rel}: ${what} not found`);
  return m[1];
};

// The canonical value comes first on purpose: everything else is compared to it.
const SITES = [
  ['src-tauri/Cargo.toml', () => cargoTomlVersion('src-tauri/Cargo.toml')],
  ['package.json', () => jsonVersion('package.json', (j) => j.version)],
  ['src-tauri/tauri.conf.json', () => jsonVersion('src-tauri/tauri.conf.json', (j) => j.version)],
  ['snap/snapcraft.yaml', () => byRegex('snap/snapcraft.yaml', /^version:\s*['"]?([^'"\s]+)['"]?/m, 'version')],
  ['package-lock.json (version)', () => jsonVersion('package-lock.json', (j) => j.version)],
  ['package-lock.json (packages[""])', () => jsonVersion('package-lock.json', (j) => j.packages?.['']?.version)],
  ['src-tauri/Cargo.lock (aeroftp)', () => cargoLockVersion('src-tauri/Cargo.lock', 'aeroftp')],
  // public/splash.html hardcodes it: Tauri IPC is unavailable in the splash
  // window, so nothing resolves it at runtime (missed in v2.2.3).
  ['public/splash.html', () => byRegex('public/splash.html', /class="version">v([0-9][^\s<]*)/, 'version badge')],
];

const tagArg = process.argv.indexOf('--tag');
const tag = tagArg !== -1 ? (process.argv[tagArg + 1] || '').replace(/^v/, '') : null;
if (tagArg !== -1 && !tag) {
  console.error('check-version-sites: --tag given without a value');
  process.exit(1);
}

const readings = [];
const failures = [];
for (const [label, get] of SITES) {
  try {
    readings.push([label, get()]);
  } catch (e) {
    failures.push(`${label}: ${e.message}`);
  }
}

// Always print what was compared. A guard whose log does not name the objects it
// measured cannot be told apart from one that measured something else.
const width = Math.max(...SITES.map(([l]) => l.length));
for (const [label, value] of readings) console.log(`${label.padEnd(width)}  ${value}`);
if (tag) console.log(`${'tag'.padEnd(width)}  ${tag}`);

if (failures.length) {
  for (const f of failures) {
    console.error(`${inCI ? '::error::' : ''}check-version-sites: could not read ${f}`);
  }
  process.exit(1);
}

const [, canonical] = readings[0];
const drifted = readings.filter(([, v]) => v !== canonical);
if (drifted.length) {
  const detail = drifted.map(([l, v]) => `${l}=${v}`).join(', ');
  console.error(`${inCI ? '::error::' : ''}R10 version drift: src-tauri/Cargo.toml=${canonical} but ${detail}`);
  process.exit(1);
}
if (tag && tag !== canonical) {
  console.error(`${inCI ? '::error::' : ''}R10 tag drift: every site says ${canonical} but the tag says ${tag}`);
  process.exit(1);
}

// The metainfo is a version site with a different rule: it holds the history of
// releases, so it must CONTAIN an entry for the current version rather than
// equal it. Ubuntu App Center and GNOME Software read this file, so a bump that
// forgets it ships a release the store cannot describe. The release procedure
// has always said to add the entry by hand; this is the same instruction as a
// checked property.
const metainfo = 'app.aeroftp.AeroFTP.metainfo.xml';
const entry = `<release version="${canonical}"`;
if (!read(metainfo).includes(entry)) {
  console.error(`${inCI ? '::error::' : ''}${metainfo} has no ${entry}...> entry for the current version`);
  process.exit(1);
}
console.log(`${metainfo.padEnd(width)}  contains a <release> entry for ${canonical}`);

// Discovery. Everything above compares a list, and a list cannot report the
// member nobody added to it: that is the defect this whole script exists to fix,
// and it would be reproduced one level up by stopping here. So the tree is asked
// which files declare this version, and anything unaccounted for is a failure.
//
// The match is deliberately on DECLARATION shapes (a JSON or TOML or YAML value,
// an XML attribute, the splash badge) and not on the bare version string. Prose
// mentions a version constantly, in comments and in the changelog: a guard that
// flagged those would need an exception for every sentence someone writes, and a
// guard that cries wolf gets deleted. Measured on this tree: the bare string is
// in 18 tracked files, the declaration shapes in 9.
const ACCEPTED = new Map([
  [
    'docs/COMMAND-INVENTORY.json',
    'regenerated from the binary, and its staleness is already guarded by `aeroftp-cli inventory --check` in cli-smoke.yml',
  ],
]);

// Second sweep, and it exists because the first one asked a leading question.
// The discovery below matches the CANONICAL version, so a file that declares a
// STALE one is not found at all: the case that matters most, a manifest left a
// version behind, was the one case invisible to it. Reviewed and confirmed by
// injecting `version: "4.1.8"` into a tracked file, where the script passed.
//
// The obvious repair, matching declaration shapes with any version, was measured
// before being chosen and rejected: it matches 16 files and 2300 lines, because
// every dependency in `Cargo.lock` (1189), `peer-l0/Cargo.lock` (506) and
// `package-lock.json` (350) declares a version, as do the rsync protocol string
// in `delta_sync_rsync.rs` and the MCP server version in `mcp/server.rs`. An
// accept list for other software's version numbers is noise with no safety in it.
//
// So the sweep is bounded by PURPOSE instead of by value: a file whose NAME says
// it declares a version. Measured on this tree that is seven files, five of them
// already compared above and two that legitimately carry their own lifecycle.
// A manifest left at the previous version is now found because it is a manifest,
// not because it happens to hold the value we expected.
const MANIFEST_RE = /(^|\/)(Cargo\.toml|package\.json|tauri\.conf\.json|snapcraft\.yaml|[^/]*\.metainfo\.xml)$/;
const MANIFEST_ACCEPTED = new Map([
  ['src-tauri/peer-l0/Cargo.toml', 'a spike sub-crate with its own version lifecycle, not shipped as the app'],
  ['tests/portal-chooser/fake-portal/Cargo.toml', 'a test fixture, never shipped'],
]);
const manifestVersion = (rel) => {
  const text = read(rel);
  if (rel.endsWith('Cargo.toml')) {
    const section = text.split(/^\[/m).find((s) => s.startsWith('package]'));
    return section?.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1] ?? null;
  }
  if (rel.endsWith('snapcraft.yaml')) return text.match(/^version:\s*['"]?([^'"\s]+)/m)?.[1] ?? null;
  if (rel.endsWith('.metainfo.xml')) return text.match(/<release version="([^"]+)"/)?.[1] ?? null;
  try {
    return JSON.parse(text).version ?? null;
  } catch {
    return null;
  }
};

const manifestDrift = [];
let manifestsChecked = 0;
try {
  const { execFileSync } = await import('node:child_process');
  const manifests = execFileSync('git', ['ls-files'], { cwd: ROOT, encoding: 'utf8' })
    .split('\n')
    .filter((f) => f && MANIFEST_RE.test(f));
  if (manifests.length < 5) {
    throw new Error(`only ${manifests.length} manifest-shaped files found, the pattern or the root is wrong`);
  }
  for (const rel of manifests) {
    if (MANIFEST_ACCEPTED.has(rel)) continue;
    manifestsChecked += 1;
    const declared = manifestVersion(rel);
    if (declared === null) {
      manifestDrift.push(`${rel}: declares a version this script cannot read`);
    } else if (declared !== canonical) {
      manifestDrift.push(`${rel}: declares ${declared}`);
    }
  }
} catch (e) {
  console.error(`${inCI ? '::error::' : ''}check-version-sites: could not sweep the manifests (${e.message.split('\n')[0]})`);
  process.exit(1);
}
if (manifestDrift.length) {
  console.error(`${inCI ? '::error::' : ''}a file whose name says it declares a version does not say ${canonical}:`);
  for (const d of manifestDrift) console.error(`  ${d}`);
  console.error('Bump it, or add it to MANIFEST_ACCEPTED with the reason it keeps its own version.');
  process.exit(1);
}
console.log(`${'manifests'.padEnd(width)}  ${manifestsChecked} checked, all on ${canonical}`);
let discovered;
try {
  const { execFileSync } = await import('node:child_process');
  const v = canonical.replace(/\./g, '\\.');
  const pattern = `("(app_)?version"[[:space:]]*:[[:space:]]*"${v}"|^[[:space:]]*version[[:space:]]*=[[:space:]]*"${v}"|^[[:space:]]*version:[[:space:]]*['"]?${v}|version="${v}"|class="version">v${v})`;
  discovered = execFileSync('git', ['grep', '-lE', pattern, '--', '.'], {
    cwd: ROOT,
    encoding: 'utf8',
  })
    .split('\n')
    .filter(Boolean);
} catch (e) {
  // A failure to ask is not an answer. `git grep` exits 1 when it matches
  // nothing, and matching nothing here is impossible (package.json declares the
  // version), so an empty result means the probe did not run.
  console.error(`${inCI ? '::error::' : ''}check-version-sites: could not enumerate declaring files (${e.message.split('\n')[0]})`);
  process.exit(1);
}
const known = new Set([
  'src-tauri/Cargo.toml',
  'package.json',
  'src-tauri/tauri.conf.json',
  'snap/snapcraft.yaml',
  'package-lock.json',
  'src-tauri/Cargo.lock',
  'public/splash.html',
  metainfo,
]);
const unaccounted = discovered.filter((f) => !known.has(f) && !ACCEPTED.has(f));
if (!discovered.some((f) => f === 'package.json')) {
  console.error(`${inCI ? '::error::' : ''}check-version-sites: the discovery pattern did not even match package.json, so it is broken`);
  process.exit(1);
}
if (unaccounted.length) {
  console.error(`${inCI ? '::error::' : ''}these files declare version ${canonical} and are not compared by this script:`);
  for (const f of unaccounted) console.error(`  ${f}`);
  console.error('Add each one to SITES if it must stay in lockstep, or to ACCEPTED with the reason it must not.');
  process.exit(1);
}
console.log(
  `check-version-sites: ${readings.length} sites agree on ${canonical}${tag ? ' and match the tag' : ''}, ` +
    `${discovered.length} files declare it and all are accounted for`,
);
