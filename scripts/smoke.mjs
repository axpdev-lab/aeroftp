#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// Single pre-release smoke entrypoint: `npm run smoke`.
//
// Runs the deterministic suites (Rust `cargo test` + frontend `vitest run`) that
// must always pass, then attempts the lab-backed `#[ignore]` integration lanes.
// Integration lanes clean-SKIP (never fail the run) when their enabler is absent:
//   - Docker-fixture lanes skip unless the fixture port is listening on 127.0.0.1.
//   - Vault/creds lanes skip unless the relevant env creds are set.
// Prints an aligned PASS/SKIP/FAIL matrix to stdout and exits non-zero IFF a lane
// FAILED (a SKIP never turns the smoke red: that is the whole point).
//
// ASCII only, no external deps.

import { spawnSync } from 'node:child_process';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(__dirname, '..');
const SRC_TAURI = path.join(REPO, 'src-tauri');

// ---- helpers ---------------------------------------------------------------

function probePort(port, host = '127.0.0.1', timeoutMs = 700) {
  return new Promise((resolve) => {
    const sock = new net.Socket();
    let done = false;
    const finish = (ok) => {
      if (done) return;
      done = true;
      sock.destroy();
      resolve(ok);
    };
    sock.setTimeout(timeoutMs);
    sock.once('connect', () => finish(true));
    sock.once('timeout', () => finish(false));
    sock.once('error', () => finish(false));
    sock.connect(port, host);
  });
}

function hasEnv(...keys) {
  return keys.every((k) => {
    const v = process.env[k];
    return typeof v === 'string' && v.trim().length > 0;
  });
}

function runLane(cmd, args, cwd) {
  const started = Date.now();
  const res = spawnSync(cmd, args, {
    cwd,
    stdio: 'inherit',
    env: process.env,
    shell: false,
  });
  const secs = ((Date.now() - started) / 1000).toFixed(1);
  if (res.error) {
    return { ok: false, note: `spawn error: ${res.error.message}`, secs };
  }
  if (res.status !== 0) {
    return { ok: false, note: `exit ${res.status} (${secs}s)`, secs };
  }
  return { ok: true, note: `ok (${secs}s)`, secs };
}

// ---- lane definitions ------------------------------------------------------

const cargoTest = (name) => ['test', '--test', name, '--', '--ignored', '--nocapture'];

// Deterministic lanes: always run; a non-zero exit fails the smoke.
const deterministicLanes = [
  {
    name: 'rust-unit',
    desc: 'cargo test (src-tauri)',
    exec: () => runLane('cargo', ['test'], SRC_TAURI),
  },
  {
    name: 'frontend-unit',
    desc: 'vitest run',
    exec: () => runLane('npm', ['run', 'test:unit'], REPO),
  },
];

// Integration lanes: run only if precheck() resolves true; else clean SKIP.
const SFTP_FIXTURE = { probe: () => probePort(2222), why: 'sftp-rsync fixture not up on 127.0.0.1:2222' };
const FTP_FIXTURE = { probe: () => probePort(2123), why: 'ftp fixture not up on 127.0.0.1:2123' };
const VAULT = {
  probe: async () => hasEnv('AEROFTP_MASTER_PASSWORD'),
  why: 'dev vault locked (set AEROFTP_MASTER_PASSWORD to enable)',
};

const integrationLanes = [
  // Docker-fixture backed (SFTP/rsync on :2222, FTP on :2123).
  { name: 'integration_delta_sync', desc: 'delta-sync over sftp-rsync', gate: SFTP_FIXTURE },
  { name: 'integration_sftp_pool', desc: 'sftp connection pool', gate: SFTP_FIXTURE },
  { name: 'integration_sftp_pipeline', desc: 'sftp read pipeline', gate: SFTP_FIXTURE },
  { name: 'integration_ftp_pool', desc: 'ftp connection pool', gate: FTP_FIXTURE },
  // Vault/creds backed (S3/WebDAV/B2 lab entries, WAN segmented transfer).
  {
    name: 'integration_b2',
    desc: 'b2 native provider',
    gate: {
      probe: async () => hasEnv('AEROFTP_TEST_B2_KEY_ID', 'AEROFTP_TEST_B2_KEY', 'AEROFTP_TEST_B2_BUCKET'),
      why: 'set AEROFTP_TEST_B2_KEY_ID, AEROFTP_TEST_B2_KEY, AEROFTP_TEST_B2_BUCKET to enable',
    },
  },
  { name: 'dag_s3_multipart', desc: 's3 multipart upload dag', gate: VAULT },
  { name: 'dag_s3_server_side_copy', desc: 's3 server-side copy dag', gate: VAULT },
  { name: 'dag_webdav_copy', desc: 'webdav copy dag', gate: VAULT },
  { name: 'dag_b2_multipart', desc: 'b2 multipart dag', gate: VAULT },
  { name: 'integration_gtc_wan_segmented', desc: 'gtc wan segmented transfer', gate: VAULT },
];

// ---- run -------------------------------------------------------------------

async function main() {
  const rows = [];
  let anyFail = false;

  console.log('== AeroFTP pre-release smoke ==');
  console.log('deterministic suites always run; integration lanes skip cleanly without their enabler.\n');

  for (const lane of deterministicLanes) {
    console.log(`--> [deterministic] ${lane.name}: ${lane.desc}`);
    const r = lane.exec();
    if (!r.ok) anyFail = true;
    rows.push({ suite: lane.name, kind: 'deterministic', result: r.ok ? 'PASS' : 'FAIL', note: r.note });
  }

  for (const lane of integrationLanes) {
    const enabled = await lane.gate.probe();
    if (!enabled) {
      rows.push({ suite: lane.name, kind: 'integration', result: 'SKIP', note: lane.gate.why });
      console.log(`--> [integration] ${lane.name}: SKIP (${lane.gate.why})`);
      continue;
    }
    console.log(`--> [integration] ${lane.name}: ${lane.desc}`);
    const r = runLane('cargo', cargoTest(lane.name), SRC_TAURI);
    if (!r.ok) anyFail = true;
    rows.push({ suite: lane.name, kind: 'integration', result: r.ok ? 'PASS' : 'FAIL', note: r.note });
  }

  printMatrix(rows);

  const failed = rows.filter((r) => r.result === 'FAIL').length;
  const skipped = rows.filter((r) => r.result === 'SKIP').length;
  const passed = rows.filter((r) => r.result === 'PASS').length;
  console.log(`\nsummary: ${passed} PASS, ${skipped} SKIP, ${failed} FAIL`);
  console.log(anyFail ? 'SMOKE: FAIL' : 'SMOKE: OK');
  process.exit(anyFail ? 1 : 0);
}

function printMatrix(rows) {
  const header = { suite: 'SUITE', kind: 'KIND', result: 'RESULT', note: 'NOTE' };
  const all = [header, ...rows];
  const w = (key) => Math.max(...all.map((r) => String(r[key]).length));
  const wSuite = w('suite');
  const wKind = w('kind');
  const wResult = w('result');
  const pad = (s, n) => String(s).padEnd(n, ' ');
  const line = (r) =>
    `${pad(r.suite, wSuite)}  ${pad(r.kind, wKind)}  ${pad(r.result, wResult)}  ${r.note}`;
  const rule = '-'.repeat(wSuite + wKind + wResult + 6 + 4);

  console.log('\n== smoke matrix ==');
  console.log(line(header));
  console.log(rule);
  for (const r of rows) console.log(line(r));
}

main().catch((err) => {
  console.error('smoke: unexpected error:', err);
  process.exit(1);
});
