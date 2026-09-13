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
 *  - A site that does not exist in this list is not checked. Adding a new file
 *    that hardcodes the version means adding it HERE, and the list is deliberately
 *    explicit rather than discovered by pattern, because a pattern would silently
 *    stop matching when a file is reformatted.
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

console.log(`check-version-sites: ${readings.length} sites agree on ${canonical}${tag ? ` and match the tag` : ''}`);
