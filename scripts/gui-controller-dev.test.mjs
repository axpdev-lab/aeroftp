// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { inspectorPort, scriptTimeout } from './lib/gui-inspector.mjs';
import { fixtureRoot, inspectorPid, developmentCheckout, validateFixtureEnvironment, readSession, requestBody } from './lib/gui-dev-runtime.mjs';

test('ports stay bounded, integral and loopback-only', () => {
    assert.equal(inspectorPort(), 9222);
    assert.equal(inspectorPort('9333'), 9333);
    for (const port of ['0', '1023', '65536', '9222/x', '::1:9222', '1e4', 'NaN']) assert.throws(() => inspectorPort(port));
});
test('requires explicit dedicated absolute fixture storage', () => {
    for (const value of [undefined, '', './fixture', '/', '/tmp', '/home']) assert.throws(() => fixtureRoot(value));
    assert.equal(fixtureRoot('/tmp/team-dev/fixture'), '/tmp/team-dev/fixture');
});
test('long DEV scripts preserve legacy timeouts without unbounded or malformed waits', () => {
    assert.equal(scriptTimeout(), 55000);
    assert.equal(scriptTimeout('90000'), 90000);
    for (const value of ['0', '99', '3600001', '-1', '90000x', 'NaN']) assert.throws(() => scriptTimeout(value));
});
test('both XDG and isolated HOME recipes pass; owner-home and symlink redirection fail', () => {
    const root = mkdtempSync(join(tmpdir(), 'gui-runtime-test-'));
    try {
        const env = { WEBKIT_INSPECTOR_HTTP_SERVER: '127.0.0.1:9333' };
        for (const key of ['CONFIG', 'CACHE', 'DATA']) {
            const path = join(root, key.toLowerCase()); mkdirSync(path);
            env[`XDG_${key}_HOME`] = path;
        }
        validateFixtureEnvironment(root, env, 9333);
        assert.throws(() => validateFixtureEnvironment(root, { ...env, XDG_DATA_HOME: '/home/owner/.local/share' }, 9333));
        assert.throws(() => validateFixtureEnvironment(root, { ...env, WEBKIT_INSPECTOR_HTTP_SERVER: '0.0.0.0:9333' }, 9333));
        assert.throws(() => validateFixtureEnvironment(root, env, 9222));
        mkdirSync(join(root, 'home/.config'), { recursive: true }); mkdirSync(join(root, 'home/.cache'), { recursive: true });
        mkdirSync(join(root, 'home/.local/share'), { recursive: true });
        validateFixtureEnvironment(root, { HOME: join(root, 'home'), WEBKIT_INSPECTOR_HTTP_SERVER: '127.0.0.1:9333' }, 9333);
        assert.throws(() => validateFixtureEnvironment(root, { HOME: '/home/owner', WEBKIT_INSPECTOR_HTTP_SERVER: '127.0.0.1:9333' }, 9333));
        rmSync(join(root, 'data'), { recursive: true }); symlinkSync(join(root, 'home/.local/share'), join(root, 'data'));
        assert.throws(() => validateFixtureEnvironment(root, env, 9333));
    } finally { rmSync(root, { recursive: true, force: true }); }
});
test('session receipts cannot be reused against another process, port or fixture', () => {
    const root = mkdtempSync(join(tmpdir(), 'gui-session-test-'));
    try {
        const fixture = { pid: 123, port: 9333, root };
        const session_id = 'gui-dev-11111111-1111-1111-1111-111111111111';
        const path = join(root, 'session.json');
        writeFileSync(path, JSON.stringify({ schema_version: 1, ...fixture, session_id, actor: { id: session_id, kind: 'dev', label: 'Codex' } }));
        assert.equal(readSession(path, fixture).session_id, session_id);
        for (const foreign of [{ ...fixture, pid: 124 }, { ...fixture, port: 9334 }, { ...fixture, root: '/other' }]) {
            assert.throws(() => readSession(path, foreign));
        }
    } finally { rmSync(root, { recursive: true, force: true }); }
});
test('malformed receipt values produce actionable session recovery errors', () => {
    const root = mkdtempSync(join(tmpdir(), 'gui-receipt-test-'));
    try {
        const path = join(root, 'receipt.json');
        for (const value of [null, [], 1, 'bad']) {
            writeFileSync(path, JSON.stringify(value));
            assert.throws(() => readSession(path, {}), /begin a new session/);
        }
    } finally { rmSync(root, { recursive: true, force: true }); }
});
test('real request pipeline preserves immutable session and does not force fast presentation', async () => {
    const calls = [];
    const body = requestBody({ name: 'show_view', args: { view: 'servers' }, timeout_ms: 1234 }, 'session-one');
    // Strip the Vite-only validator import; execute the actual approval/dispatch body.
    const executable = body.replace("const { validateGuiRequest } = await import('/src/gui/controller.ts'); validateGuiRequest(request);", '');
    const result = await new Function('window', `return (async()=>{${executable}})()` )({ __TAURI_INTERNALS__: { invoke: async (name, args) => {
        calls.push([name, args]);
        if (name === 'prepare_ai_tool_approval') return { approvalRequired: true, requestId: 'request-one' };
        if (name === 'grant_ai_tool_approval') return { approved: true, grantId: 'grant-one' };
        return { ok: true };
    } } });
    assert.deepEqual(result, { ok: true });
    assert.equal(calls[0][1].sessionId, 'session-one');
    assert.equal(calls[2][1].sessionId, 'session-one');
    assert.deepEqual(calls[2][1].args, { intent: 'show_view', view: 'servers', timeout_ms: 1234 });
    assert.equal(calls[2][1].approvalGrantId, 'grant-one');
    assert.equal(calls[1][1].rememberForSession, false);
});
test('help and malformed envelopes need no personal driver, fixture or running GUI', () => {
    const cli = new URL('./gui-controller-dev.mjs', import.meta.url);
    const run = args => spawnSync(process.execPath, [cli.pathname, ...args], { encoding: 'utf8', env: { PATH: process.env.PATH } });
    const help = run(['--help']); assert.equal(help.status, 0); assert.match(help.stdout, /--begin/);
    const bad = run(['--request', '{"intent":"disconnect"}']); assert.equal(bad.status, 1); assert.match(bad.stderr, /requires name/);
    const unknown = run(['--request', '{"name":"state","password":"never-echo-this"}']);
    assert.equal(unknown.status, 1); assert.match(unknown.stderr, /Unknown DEV request field: password/); assert.doesNotMatch(unknown.stderr, /never-echo-this/);
    const pace = run(['--request', '{"name":"state","pace":"fast"}']); assert.match(pace.stderr, /Unknown DEV request field: pace/);
    const malformed = run(['--request', '{"password":"never-echo-this",']);
    assert.equal(malformed.status, 1); assert.match(malformed.stderr, /Invalid DEV request JSON/); assert.doesNotMatch(malformed.stderr, /never-echo-this/);
});

test('listener ownership checks the local column, not ss wildcard peer addresses', () => {
    const loopback = 'LISTEN 0 10 127.0.0.1:9333 0.0.0.0:* users:(("aeroftp",pid=123,fd=44))';
    assert.equal(inspectorPid(loopback, 9333), 123);
    assert.throws(() => inspectorPid(loopback.replace('127.0.0.1:9333', '0.0.0.0:9333'), 9333));
    assert.throws(() => inspectorPid(loopback.replace('"aeroftp"', '"other"'), 9333));
    assert.throws(() => inspectorPid(loopback + '\n' + loopback, 9333));
});

test('normal Tauri launch in src-tauri and direct launch in checkout are both owned', () => {
    const root = mkdtempSync(join(tmpdir(), 'gui-checkout-test-'));
    try {
        mkdirSync(join(root, 'src/gui'), { recursive: true }); mkdirSync(join(root, 'src-tauri'));
        writeFileSync(join(root, 'src/gui/controller.ts'), ''); writeFileSync(join(root, 'src-tauri/Cargo.toml'), '');
        assert.equal(developmentCheckout(root), root);
        assert.equal(developmentCheckout(join(root, 'src-tauri')), root);
        assert.throws(() => developmentCheckout(join(root, 'other')));
    } finally { rmSync(root, { recursive: true, force: true }); }
});
