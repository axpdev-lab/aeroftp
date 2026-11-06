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
const { collectOsvFindings } = require('../../scripts/security-report.cjs');
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
