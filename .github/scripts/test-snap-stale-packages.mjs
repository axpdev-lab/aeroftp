// Runs scripts/snap-stale-packages.mjs for real, with a stand-in `unsquashfs`
// on PATH that hands it a chosen snap/manifest.yaml and a local mirror that
// serves chosen Packages.gz indexes. The audit decides whether snap-refresh
// rebuilds the snap, so "not stale" must mean every staged package was found
// and compared, never that the audit could not read some of them.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { gzipSync } from 'node:zlib';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const AUDIT = join(ROOT, 'scripts', 'snap-stale-packages.mjs');

const POCKETS = ['jammy', 'jammy-updates', 'jammy-security'];
const COMPONENTS = ['main', 'universe', 'restricted', 'multiverse'];

// The shape snapcraft 9.1.4 writes, read from the 4.2.3 snap (188 entries,
// every one `name=version`).
const manifest = (entries) =>
  [
    'snapcraft-version: 9.1.4',
    'name: aeroftp',
    'version: 4.2.3',
    'base: core22',
    'architectures:',
    '- amd64',
    'build-packages: []',
    'build-snaps: []',
    'primed-stage-packages:',
    ...entries.map((e) => `- ${e}`),
    'primed-stage-snaps: []',
    '',
  ].join('\n');

const SNAP_YAML = 'name: aeroftp\nversion: 4.2.3\nbase: core22\narchitectures:\n- amd64\n';

const stanzas = (pkgs) => pkgs.map(([name, version]) => `Package: ${name}\nArchitecture: amd64\nVersion: ${version}\n`).join('\n');

// jammy/main holds the release versions, jammy-security/main the newer ones.
const ARCHIVE = {
  'jammy/main': [
    ['bubblewrap', '0.6.1-1'],
    ['libssl3', '3.0.2-0ubuntu1'],
    ['zlib1g', '1:1.2.11.dfsg-2ubuntu9'],
  ],
  'jammy-security/main': [
    ['bubblewrap', '0.6.1-1ubuntu0.3'],
    ['libssl3', '3.0.2-0ubuntu1.18'],
    ['zlib1g', '1:1.2.11.dfsg-2ubuntu9.2'],
  ],
};

const CURRENT = ['bubblewrap=0.6.1-1ubuntu0.3', 'libssl3=3.0.2-0ubuntu1.18', 'zlib1g=1:1.2.11.dfsg-2ubuntu9.2'];

async function audit(t, entries, { missing = [] } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'snap-stale-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));

  const indexes = new Map();
  for (const pocket of POCKETS) {
    for (const component of COMPONENTS) {
      const key = `${pocket}/${component}`;
      if (missing.includes(key)) continue;
      indexes.set(`/dists/${key}/binary-amd64/Packages.gz`, gzipSync(stanzas(ARCHIVE[key] ?? [])));
    }
  }
  const server = createServer((req, res) => {
    const body = indexes.get(req.url);
    res.writeHead(body ? 200 : 404).end(body);
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  t.after(() => server.close());

  // The audit runs `unsquashfs -q -n -f -d <dir> <snap> snap/manifest.yaml meta/snap.yaml`.
  writeFileSync(join(dir, 'manifest.yaml'), manifest(entries));
  writeFileSync(join(dir, 'snap.yaml'), SNAP_YAML);
  const bin = join(dir, 'bin');
  mkdirSync(bin);
  writeFileSync(
    join(bin, 'unsquashfs'),
    [
      '#!/bin/sh',
      'while [ "$#" -gt 0 ]; do [ "$1" = -d ] && out="$2"; shift; done',
      'mkdir -p "$out/snap" "$out/meta"',
      `cp '${join(dir, 'manifest.yaml')}' "$out/snap/manifest.yaml"`,
      `cp '${join(dir, 'snap.yaml')}' "$out/meta/snap.yaml"`,
      '',
    ].join('\n'),
  );
  chmodSync(join(bin, 'unsquashfs'), 0o755);

  const output = join(dir, 'github-output');
  writeFileSync(output, '');
  const env = {
    ...process.env,
    PATH: `${bin}:${process.env.PATH}`,
    UBUNTU_MIRROR: `http://127.0.0.1:${server.address().port}`,
    GITHUB_OUTPUT: output,
  };
  delete env.GITHUB_STEP_SUMMARY;

  const child = spawn(process.execPath, [AUDIT, '--snap', join(dir, 'aeroftp.snap')], { env });
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (d) => (stdout += d));
  child.stderr.on('data', (d) => (stderr += d));
  const status = await new Promise((resolve) => child.on('close', resolve));
  return { status, stdout, stderr, outputs: readFileSync(output, 'utf8') };
}

test('current packages read as current', async (t) => {
  const r = await audit(t, CURRENT);
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stdout, /OK: all 3 staged packages are at the newest archive version/);
  assert.match(r.outputs, /^stale=false$/m);
});

test('an older package reads as stale', async (t) => {
  const r = await audit(t, ['bubblewrap=0.6.1-1ubuntu0.3', 'libssl3=3.0.2-0ubuntu1.10', 'zlib1g=1:1.2.11.dfsg-2ubuntu9.2']);
  assert.equal(r.status, 0, r.stderr);
  assert.match(r.stdout, /libssl3: snap=3\.0\.2-0ubuntu1\.10 {2}archive=3\.0\.2-0ubuntu1\.18/);
  assert.match(r.outputs, /^stale=true$/m);
  assert.match(r.outputs, /^count=1$/m);
});

// A stale libssl3 written in a shape the reader does not split into a name
// and a version used to be "not found in the archive" and then "OK": the
// rebuild was skipped with a stale package in the snap.
test('a manifest entry not shaped name=version is unverified, not current', async (t) => {
  for (const entry of [
    'libssl3:amd64=3.0.2-0ubuntu1.10',
    'libssl3 3.0.2-0ubuntu1.10',
    'libssl3: 3.0.2-0ubuntu1.10',
    'libssl3==3.0.2-0ubuntu1.10',
    'libssl3=',
    '=3.0.2-0ubuntu1.10',
  ]) {
    await t.test(entry, async (t) => {
      const r = await audit(t, ['bubblewrap=0.6.1-1ubuntu0.3', entry, 'zlib1g=1:1.2.11.dfsg-2ubuntu9.2']);
      assert.equal(r.status, 2, r.stdout);
      assert.match(r.stderr, /::error::/);
      assert.ok(r.stderr.includes(entry), r.stderr);
      assert.doesNotMatch(r.stdout, /^OK:/m);
      assert.doesNotMatch(r.outputs, /^stale=/m);
    });
  }
});

// No snapcraft.yaml part adds a package repository, so every staged package
// comes from the Ubuntu archive: one the indexes do not list was not compared.
test('a staged package missing from the archive indexes is unverified', async (t) => {
  const r = await audit(t, [...CURRENT, 'libfoo1=1.0-1']);
  assert.equal(r.status, 2, r.stdout);
  assert.match(r.stderr, /::error::.*1 staged package/);
  assert.match(r.stderr, /libfoo1=1\.0-1/);
  assert.doesNotMatch(r.stdout, /^OK:/m);
  assert.doesNotMatch(r.outputs, /^stale=/m);
});

// Every pocket and component exists for every base in BASE_SUITE (all 60
// amd64 indexes answered 200 on 2026-10-17). A 404 on -security used to be
// skipped, and the audit then compared against the release versions only.
test('an index the mirror does not serve fails the audit', async (t) => {
  for (const key of ['jammy-security/main', 'jammy-updates/universe']) {
    await t.test(key, async (t) => {
      const r = await audit(t, ['bubblewrap=0.6.1-1', 'libssl3=3.0.2-0ubuntu1', 'zlib1g=1:1.2.11.dfsg-2ubuntu9'], {
        missing: [key],
      });
      assert.equal(r.status, 2, r.stdout);
      assert.match(r.stderr, new RegExp(`::error::.*${key.replace('/', '/')}.*404`));
      assert.doesNotMatch(r.stdout, /^OK:/m);
      assert.doesNotMatch(r.outputs, /^stale=/m);
    });
  }
});

test('a manifest without the primed-stage-packages block fails the audit', async (t) => {
  const r = await audit(t, []);
  assert.equal(r.status, 2, r.stdout);
  assert.match(r.stderr, /::error::.*primed-stage-packages/);
});
