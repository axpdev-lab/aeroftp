// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Pins scripts/check-appstream.mjs against the two report formats that
// `appstreamcli validate --no-net --format yaml` produces: block YAML from
// 1.0.2 (Ubuntu 24.04, the ubuntu-latest runner today) and a JSON-shaped
// document from 1.1.2 (Ubuntu 26.04 LTS). The gate itself is run with a
// stand-in appstreamcli on PATH, so what is tested is the gate and not a copy
// of its parser.
//
// The reports in fixtures/appstream are captured, not written by hand, and
// each pair comes from the same file through the two real binaries:
//   <name>-1.1.2.txt  /usr/bin/appstreamcli 1.1.2-1 on Ubuntu 26.04.1 LTS
//   <name>-1.0.2.txt  the 1.0.2 appstreamcli of the gnome-46-2404 snap
//                     (Ubuntu 24.04 userland), run under the core24 loader
// on accepted.xml and blocking.xml, saved as app.aeroftp.AeroFTP.metainfo.xml.
// The xml-invalid pair comes from a component whose first paragraph holds a
// BEL, a no-break space, U+2028 and an emoji: the validator quotes that line
// in its hint, which is where 1.1.2 writes escapes JSON does not have.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readValidateReport, yamlDoubleQuotedToJson } from '../../scripts/lib/appstream-report.mjs';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const GATE = join(ROOT, 'scripts', 'check-appstream.mjs');
const FIXTURES = join(ROOT, '.github', 'scripts', 'fixtures', 'appstream');
const capture = (name) => readFileSync(join(FIXTURES, name), 'utf8');

// Runs the real gate in a scratch directory holding `component` as the
// metainfo, with stand-ins for both validators that print what they are given.
function gate(t, component, report, { status = 3, desktopOut = '', desktopStatus = 0 } = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'check-appstream-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const bin = join(dir, 'bin');
  mkdirSync(bin);
  copyFileSync(join(FIXTURES, component), join(dir, 'app.aeroftp.AeroFTP.metainfo.xml'));
  writeFileSync(join(dir, 'app.aeroftp.AeroFTP.desktop'), '');
  writeFileSync(join(dir, 'report.txt'), report);
  writeFileSync(join(dir, 'desktop.txt'), desktopOut);
  for (const [tool, out, code] of [
    ['appstreamcli', 'report.txt', status],
    ['desktop-file-validate', 'desktop.txt', desktopStatus],
  ]) {
    writeFileSync(join(bin, tool), `#!/bin/sh\ncat '${join(dir, out)}'\nexit ${code}\n`);
    chmodSync(join(bin, tool), 0o755);
  }
  const env = { ...process.env, PATH: `${bin}:${process.env.PATH}` };
  delete env.GITHUB_ACTIONS;
  const r = spawnSync(process.execPath, [GATE], { cwd: dir, env, encoding: 'utf8' });
  return { status: r.status, stdout: r.stdout, stderr: r.stderr };
}

test('the 1.0.2 block report and the 1.1.2 JSON report give the same findings', () => {
  for (const name of ['accepted', 'blocking']) {
    const block = readValidateReport(capture(`${name}-1.0.2.txt`), 3);
    const json = readValidateReport(capture(`${name}-1.1.2.txt`), 3);
    assert.equal(block.format, 'block', name);
    assert.equal(json.format, 'json', name);
    assert.deepEqual(json.findings, block.findings, name);
  }
  // Spelled out once, so that "the same" cannot mean "both empty".
  assert.deepEqual(readValidateReport(capture('blocking-1.1.2.txt'), 3).findings, [
    { tag: 'cid-contains-uppercase-letter', severity: 'pedantic', line: 4 },
    { tag: 'description-first-para-too-short', severity: 'info', line: 10 },
    { tag: 'description-has-plaintext-url', severity: 'warning', line: 21 },
    { tag: 'description-has-plaintext-url', severity: 'warning', line: 26 },
    { tag: 'description-has-plaintext-url', severity: 'warning', line: 31 },
    { tag: 'description-has-plaintext-url', severity: 'warning', line: 36 },
  ]);
});

test('the 1.1.2 report is read although it is not valid JSON', () => {
  // The capture wraps strings with bare and escaped line breaks, and the XML
  // error quotes text with YAML-only escapes. If JSON.parse alone could read
  // these, the decoding step would be untested by them.
  for (const name of ['blocking-1.1.2.txt', 'xml-invalid-1.1.2.txt']) {
    assert.throws(() => JSON.parse(capture(name).replace(/^---\n/, '')), SyntaxError, name);
  }
  const error = [{ tag: 'xml-markup-invalid', severity: 'error' }];
  assert.deepEqual(readValidateReport(capture('xml-invalid-1.0.2.txt'), 3).findings, error);
  assert.deepEqual(readValidateReport(capture('xml-invalid-1.1.2.txt'), 3).findings, error);
});

test('YAML double-quoted strings decode by the YAML rules', () => {
  // Expected values are what PyYAML 6.0.3 reads for the same scalars.
  const cases = [
    ['a\\\n   b', 'ab'],
    ['a \\\n   b', 'a b'],
    ['a\n  b', 'a b'],
    ['a  \t\n\n\n  b', 'a\n\nb'],
    ['a\\\n\n  b', 'a\nb'],
    ['x\\\\\n  y', 'x\\ y'],
    ['t\\ta\tb', 't\ta\tb'],
    ['lead\\\n  \\ \tkept', 'lead \tkept'],
    ['\\_\\L\\P\\N\\U0001F600\\x41\\u00e9\\e\\0\\a\\v\\ \\/\\"', '   \u0085\u{1F600}Aé\u001b\u0000\u0007\u000b /"'],
  ];
  for (const [yaml, value] of cases) {
    assert.equal(JSON.parse(`"${yamlDoubleQuotedToJson(yaml)}"`), value, JSON.stringify(yaml));
  }
  // An escape neither language has is refused, never passed through.
  for (const bad of ['\\q', '\\x4', '\\u12', '\\U110000AA', '\\\r']) {
    assert.throws(() => JSON.parse(`"${yamlDoubleQuotedToJson(bad)}"`), undefined, JSON.stringify(bad));
  }
});

test('a file with no finding at all is an empty report that exits 0', () => {
  // Measured on both versions with a component that has nothing to report.
  assert.deepEqual(readValidateReport('', 0), { findings: [], format: 'empty' });
  assert.ok(readValidateReport('', 3).error);
  assert.ok(readValidateReport('\n', 1).error);
});

test('a report this reader cannot read is refused, not read as clean', () => {
  const json = capture('blocking-1.1.2.txt');
  const block = capture('blocking-1.0.2.txt');
  const ok = '{ "tag": "description-has-plaintext-url", "severity": "warning", "line": 21 }';
  const unreadable = {
    'flow-style record': '---\nIssues:\n- { tag: metainfo-unknown-tag, severity: error, line: 7 }\nPassed: no\n',
    'block next to flow': `${block.replace(/Passed: no\n$/, '')}- { tag: metainfo-unknown-tag, severity: error }\nPassed: no\n`,
    'block report cut short': block.split('\n').slice(0, 20).join('\n'),
    'block, no record, not passed': '---\nFile: x\nIssues: []\nPassed: no\n',
    'JSON cut short': json.slice(0, Math.floor(json.length / 2)),
    // Each malformed record sits next to a readable one: alone, skipping it
    // would leave zero findings and be refused by the generic path anyway.
    'JSON issue without severity': `---\n{ "Issues": [ ${ok}, { "tag": "x", "line": 3 } ], "Passed": "no" }\n`,
    'JSON issue without tag': `---\n{ "Issues": [ ${ok}, { "severity": "error", "line": 3 } ], "Passed": "no" }\n`,
    'JSON issue with a line that is not a line': `---\n{ "Issues": [ ${ok}, { "tag": "x", "severity": "error", "line": "3" } ], "Passed": "no" }\n`,
    'JSON Issues not a list': '---\n{ "Issues": { "tag": "x", "severity": "error" }, "Passed": "no" }\n',
    'JSON without a verdict': '---\n{ "Issues": [ { "tag": "x", "severity": "error" } ] }\n',
    'JSON, no issue, not passed': '---\n{ "Issues": [], "Passed": "no" }\n',
    'JSON with an unknown escape': '---\n{ "Issues": [ { "tag": "x", "severity": "error", "hint": "\\q" } ], "Passed": "no" }\n',
    'JSON top level not an object': '---\n[ { "tag": "x", "severity": "error" } ]\n',
    'neither shape': 'Validation failed: errors: 1\n',
  };
  for (const [name, text] of Object.entries(unreadable)) {
    const r = readValidateReport(text, 3);
    assert.ok(r.error, `${name}: ${JSON.stringify(r)}`);
    assert.equal(r.findings, undefined, name);
  }
  assert.match(readValidateReport(unreadable['block next to flow'], 3).error, /1 flow-style record/);
});

test('the gate gives the same verdict on the same file from either version', (t) => {
  const v102 = gate(t, 'accepted.xml', capture('accepted-1.0.2.txt'));
  const v112 = gate(t, 'accepted.xml', capture('accepted-1.1.2.txt'));
  assert.equal(v102.status, 0, v102.stderr);
  assert.equal(v112.status, 0, v112.stderr);
  const accepted = (out) => out.split('\n').filter((l) => l.startsWith('accepted'));
  assert.deepEqual(accepted(v112.stdout), accepted(v102.stdout));
  assert.deepEqual(accepted(v112.stdout), [
    'accepted  app.aeroftp.AeroFTP.metainfo.xml:21 description-has-plaintext-url (aeroftp://)',
    'accepted  app.aeroftp.AeroFTP.metainfo.xml:26 description-has-plaintext-url (tauri://, http://127.0.0.1)',
  ]);
  assert.match(v102.stdout, /block report\)/);
  assert.match(v112.stdout, /json report\)/);
});

test('the gate names the same real findings from either version', (t) => {
  const v102 = gate(t, 'blocking.xml', capture('blocking-1.0.2.txt'));
  const v112 = gate(t, 'blocking.xml', capture('blocking-1.1.2.txt'));
  assert.equal(v102.status, 1);
  assert.equal(v112.status, 1);
  assert.equal(v112.stdout, v102.stdout);
  assert.equal(v112.stderr, v102.stderr);
  for (const line of [
    'app.aeroftp.AeroFTP.metainfo.xml:26 warning description-has-plaintext-url: https://example.com/docs',
    'app.aeroftp.AeroFTP.metainfo.xml:31 warning description-has-plaintext-url: https://example.com/profiles',
    'app.aeroftp.AeroFTP.metainfo.xml:36 warning description-has-plaintext-url: http://127.0.0.1.example.invalid/',
  ]) {
    assert.ok(v112.stderr.includes(line), `${line}\n--- stderr ---\n${v112.stderr}`);
  }
  assert.match(v112.stdout, /^accepted {2}app\.aeroftp\.AeroFTP\.metainfo\.xml:21 /m);
});

test('the gate fails on a report it cannot read and on an empty one that is not an answer', (t) => {
  const flow = gate(t, 'accepted.xml', '---\nIssues:\n- { tag: x, severity: error }\nPassed: no\n');
  assert.equal(flow.status, 1);
  assert.match(flow.stderr, /flow-style record/);
  const cut = capture('accepted-1.1.2.txt');
  const truncated = gate(t, 'accepted.xml', cut.slice(0, cut.length - 40));
  assert.equal(truncated.status, 1);
  assert.match(truncated.stderr, /does not parse/);
  assert.equal(gate(t, 'accepted.xml', '', { status: 3 }).status, 1);
  assert.equal(gate(t, 'accepted.xml', '', { status: 0 }).status, 0);
});

test('desktop-file-validate failing without an error: line still fails the gate', (t) => {
  // Measured on 0.28: a missing file prints "file does not exist" and exits 1.
  const r = gate(t, 'accepted.xml', capture('accepted-1.1.2.txt'), {
    desktopOut: 'app.aeroftp.AeroFTP.desktop: file does not exist\n',
    desktopStatus: 1,
  });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /desktop-file-validate exited 1: app\.aeroftp\.AeroFTP\.desktop: file does not exist/);
  const named = gate(t, 'accepted.xml', capture('accepted-1.1.2.txt'), {
    desktopOut: 'app.aeroftp.AeroFTP.desktop: error: required key "Type" in group "Desktop Entry" is not present\n',
    desktopStatus: 1,
  });
  assert.equal(named.status, 1);
  assert.match(named.stderr, /error: required key "Type"/);
  assert.doesNotMatch(named.stderr, /exited 1/);
});
