#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// Live Linux DEV check for Requested item 23 of tracker #1202. See fixture README.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream, openSync, closeSync } from 'node:fs';
import { chmod, mkdir, mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { fixtureRoot, inspectFixture } from './lib/gui-dev-runtime.mjs';
import { inspectorPort } from './lib/gui-inspector.mjs';

const exec = promisify(execFile);
const checkout = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const root = fixtureRoot(process.env.AEROFTP_GUI_TEST_ROOT);
const fixture = inspectFixture(root, inspectorPort(process.env.INSPECTOR_PORT));
assert.equal(fixture.cwd, checkout, 'Launch the GUI from the checkout serving this test');
assert.notEqual(process.getuid(), 0, 'Run unprivileged: root bypasses the permission fault');
const run = await mkdtemp(join(root, 'mirror-kept-'));
const session = join(run, 'controller-session.json');
const profileId = `srv_mirror_${Date.now()}`;
const source = join(run, 'source');
const destination = join(run, 'destination');
const ftpRoot = join(run, 'ftp');
const protectedDir = join(destination, 'unfinished/deep');
const receipts = { date: new Date().toISOString(), fixture: run };
let server, began = false, seeded = false;

const pause = ms => new Promise(r => setTimeout(r, ms));
async function driver(...args) {
    const { stdout } = await exec(process.execPath, [join(checkout, 'scripts/gui-controller-dev.mjs'), ...args],
        { cwd: checkout, env: process.env, timeout: 60000, maxBuffer: 2 * 1024 * 1024 });
    return JSON.parse(stdout).value;
}
async function drive(body) {
    const path = join(run, 'step.js');
    await writeFile(path, helpers + body);
    return driver('--js', path);
}
async function request(name, args) {
    const result = await driver('--request', JSON.stringify({ name, args, timeout_ms: 30000 }), '--session-file', session);
    assert.equal(result.ok, true, JSON.stringify(result));
    return result.snapshot;
}
async function seed(path, text) {
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, text);
}
async function exists(path) {
    try { await readdir(path); return true; } catch (error) { if (error.code === 'ENOENT') return false; throw error; }
}
async function digest(path) {
    const hash = createHash('sha256');
    for await (const chunk of createReadStream(path)) hash.update(chunk);
    return hash.digest('hex');
}

// All actions below use rendered controls and real native commands. React props
// are observed only to capture the report actually passed to the result dialog.
const helpers = `
const wait = async (predicate, label) => {
    const end = Date.now() + 45000;
    while (Date.now() < end) { const value = predicate(); if (value) return value;
        await new Promise(resolve => setTimeout(resolve, 50)); }
    throw Error('Timeout: ' + label);
};
const dialog = () => document.querySelector('[role=dialog]');
const button = text => [...document.querySelectorAll('button')].find(e => e.innerText.trim() === text && !e.disabled);
const click = async text => (await wait(() => button(text), text)).click();
const paint = () => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
function propsWhere(predicate) {
    for (const element of document.querySelectorAll('[role=dialog] *')) {
        const key = Object.keys(element).find(k => k.startsWith('__reactFiber$'));
        for (let fiber = key && element[key], i = 0; fiber && i < 12; fiber = fiber.return, i++) {
            if (predicate(fiber.memoizedProps)) return fiber.memoizedProps;
        }
    }
    throw Error('Expected rendered component props');
}
`;

async function plan(remote) {
    return drive(`
await click('AeroSync'); await click('Plan'); await click('Start compare');
await wait(() => document.querySelector('textarea'), 'compare complete');
const mirror = [...dialog().querySelectorAll('button')].find(e => e.innerText.startsWith('Mirror\\n'));
if (!mirror) throw Error('Mirror preset missing'); mirror.click(); await paint();
const input = dialog().querySelector('textarea');
Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(input, '*.keep');
input.dispatchEvent(new Event('input', { bubbles: true })); await paint();
${remote ? `await click('Local → Remote'); await paint();
const backup = dialog().querySelector('input[type=checkbox]'); if (!backup.checked) backup.click(); await paint();` : ''}
await click('Rescan');
await wait(() => document.querySelector('textarea')?.value === '*.keep'
    && !dialog().innerText.includes('Rescan before running')
    && [...dialog().querySelectorAll('button')].some(e => e.innerText.startsWith('Execute Mirror')), 'fresh compare');
return { text: dialog().innerText,
    comparison: propsWhere(p => p?.result && p?.onExecute).result };
`);
}
async function execute() {
    return drive(`
const confirm = [...dialog().querySelectorAll('input[type=checkbox]')]
    .find(e => e.closest('label')?.innerText.includes('reviewed the destructive'));
if (!confirm) throw Error('Destructive confirmation missing'); if (!confirm.checked) confirm.click();
(await wait(() => [...dialog().querySelectorAll('button')]
    .find(e => e.innerText.startsWith('Execute Mirror') && !e.disabled), 'Execute enabled')).click();
await wait(() => dialog()?.innerText.includes('Folders kept'), 'Mirror result'); await paint();
return { report: propsWhere(p => p?.report?.keptDirs).report, text: dialog().innerText };
`);
}
function checkReport(result, kept, errorPath) {
    assert.equal(result.report.deleted, 3);
    assert.equal(result.report.skipped, kept.length);
    assert.equal(result.report.cancelled, false);
    assert.equal(result.report.errors.length, 1);
    assert.equal(result.report.errors[0].file_path, errorPath);
    assert.deepEqual(result.report.keptDirs, kept.map(([file_path, reason]) => ({ file_path, reason })));
    for (const [path, reason] of kept) {
        const label = { entries: 'holds excluded or unlisted entries',
            unfinished_rows: 'holds files that were not synced', server: 'not empty on the server' }[reason];
        assert.ok(result.text.includes(`${path}/: ${label}`), `Rendered reason missing: ${path}`);
    }
}

try {
    const state = await drive(`
const state = window.__aeroftpController.state();
if (state.locked || state.blocked || state.connected || state.sessions.length) throw Error('Use an idle, unlocked, fresh GUI fixture');
if (!document.querySelector('button[aria-label="Language: English"]')) throw Error('Select English in this test fixture');
const scoped = await window.__TAURI_INTERNALS__.invoke('user_partitions_load_active_server_profiles_scoped');
if (scoped.profiles.length) throw Error('Use a fresh fixture with no saved profiles');
return { ...state, userAgent: navigator.userAgent };
`);
    receipts.app_version = state.version;
    receipts.locale = 'en'; receipts.user_agent = state.userAgent;
    receipts.frontend_sha = (await exec('git', ['rev-parse', 'HEAD'], { cwd: checkout })).stdout.trim();
    receipts.binary_sha256 = await digest(`/proc/${fixture.pid}/exe`);
    await mkdir(source);
    await seed(join(destination, 'excluded/deep/sentinel.keep'), 'excluded local sentinel\n');
    await seed(join(protectedDir, 'sentinel.txt'), 'failed local sentinel\n');
    await seed(join(destination, 'removed/deep/removable.txt'), 'local removal control\n');
    await chmod(protectedDir, 0o500);
    await driver('--begin', '--actor', 'Mirror live check', '--session-file', session); began = true;
    const shown = await request('show_view', { view: 'files' });
    if (!shown.panels.local2) await drive(`
const toggle = document.querySelector('button[aria-label="Dual Panel"]');
if (!toggle) throw Error('Dual Panel control missing'); toggle.click();
await wait(() => window.__aeroftpController.state().panels.local2, 'second local panel'); return true;`);
    await request('navigate', { panel: 'local', path: source });
    await request('navigate', { panel: 'local2', path: destination });
    receipts.local_plan = await plan(false);
    assert.ok(!JSON.stringify(receipts.local_plan.comparison).includes('sentinel.keep'));
    receipts.local = await execute();
    checkReport(receipts.local, [['excluded/deep', 'entries'], ['unfinished/deep', 'unfinished_rows'],
        ['excluded', 'unfinished_rows'], ['unfinished', 'unfinished_rows']], 'unfinished/deep/sentinel.txt');
    assert.equal(await readFile(join(destination, 'excluded/deep/sentinel.keep'), 'utf8'), 'excluded local sentinel\n');
    assert.equal(await readFile(join(protectedDir, 'sentinel.txt'), 'utf8'), 'failed local sentinel\n');
    assert.equal(await exists(join(destination, 'removed')), false);
    await drive(`await click('Close'); return true;`);

    for (const [path, content] of Object.entries({
        'excluded/deep/sentinel.keep': 'excluded remote sentinel\n',
        'hidden/deep/invisible.keep': 'hidden remote sentinel\n',
        'failed/deep/only-copy.txt': 'backup failed sentinel\n',
        'removed/deep/removable.txt': 'backup succeeds sentinel\n',
    })) await seed(join(ftpRoot, 'remote', path), content);
    const events = join(run, 'ftp-commands.jsonl'), ready = join(run, 'ftp-port');
    const log = openSync(join(run, 'ftp.log'), 'wx', 0o600);
    try {
        server = spawn(process.env.AEROFTP_TEST_PYTHON || 'python3', [
            join(checkout, 'src-tauri/tests/fixtures/gui-mirror/server.py'),
            '--root', ftpRoot, '--events', events, '--ready', ready,
        ], { stdio: ['ignore', log, log] });
    } finally { closeSync(log); }
    let spawnError;
    server.on('error', error => { spawnError = error; });
    const deadline = Date.now() + 10000;
    let port;
    while (!port) {
        if (spawnError) throw spawnError;
        if (server.exitCode !== null || Date.now() > deadline) throw Error(`FTP fixture unavailable; see ${run}/ftp.log`);
        try { port = Number(await readFile(ready, 'utf8')); } catch (error) { if (error.code !== 'ENOENT') throw error; }
        if (!port) await pause(50);
    }
    seeded = true;
    await drive(`
const invoke = window.__TAURI_INTERNALS__.invoke;
const scoped = await invoke('user_partitions_load_active_server_profiles_scoped');
if (scoped.profiles.length) throw Error('Fixture profiles changed');
await invoke('store_credential', { account: 'server_${profileId}', password: 'mirror-artificial-password' });
await invoke('user_partitions_save_active_server_profiles', { expectedUserId: scoped.userId, profiles: [{
    id: '${profileId}', name: 'Mirror live fixture', host: '127.0.0.1', port: ${port}, username: 'mirror-test',
    protocol: 'ftp', initialPath: '/remote', localInitialPath: ${JSON.stringify(source)},
    hasStoredCredential: true, options: { tlsMode: 'none' },
}] }); window.dispatchEvent(new Event('aeroftp-profiles-changed')); return true;
`);
    await request('connect', { profile_id: profileId });
    receipts.remote_plan = await plan(true);
    assert.ok(!JSON.stringify(receipts.remote_plan.comparison).includes('sentinel.keep'));
    assert.ok(!JSON.stringify(receipts.remote_plan.comparison).includes('invisible.keep'));
    receipts.remote = await execute();
    checkReport(receipts.remote, [['excluded/deep', 'entries'], ['failed/deep', 'unfinished_rows'],
        ['hidden/deep', 'server'], ['excluded', 'unfinished_rows'], ['failed', 'unfinished_rows'],
        ['hidden', 'unfinished_rows']], 'failed/deep/only-copy.txt');
    for (const [path, content] of Object.entries({
        'excluded/deep/sentinel.keep': 'excluded remote sentinel\n',
        'hidden/deep/invisible.keep': 'hidden remote sentinel\n',
        'failed/deep/only-copy.txt': 'backup failed sentinel\n',
    })) assert.equal(await readFile(join(ftpRoot, 'remote', path), 'utf8'), content);
    assert.equal(await exists(join(ftpRoot, 'remote/removed')), false);
    const backups = join(ftpRoot, 'remote/.aeroftp-versions');
    const stamps = await readdir(backups);
    assert.equal(stamps.length, 1);
    assert.equal(await readFile(join(backups, stamps[0], 'removed/deep/removable.txt'), 'utf8'), 'backup succeeds sentinel\n');
    receipts.ftp_commands = (await readFile(events, 'utf8')).trim().split('\n').map(line => JSON.parse(line));
    assert.ok(receipts.ftp_commands.some(e => e.cmd === 'RNTO' && e.code === 550 && e.path.endsWith('/only-copy.txt')));
    assert.ok(receipts.ftp_commands.some(e => e.cmd === 'RMD' && e.path === 'remote/hidden/deep' && e.code === 550));
    assert.ok(!receipts.ftp_commands.some(e => e.cmd === 'RMD' && /^remote\/failed(?:\/|$)/.test(e.path)));
    assert.ok(!receipts.ftp_commands.some(e => e.cmd === 'DELE'));
    receipts.status = 'PASS';
    await drive(`await click('Close'); return true;`);
} catch (error) {
    receipts.status = 'FAIL'; receipts.error = String(error.stack || error);
    throw error;
} finally {
    await chmod(protectedDir, 0o700).catch(() => {});
    if (seeded) {
        await request('disconnect', {}).catch(error => { receipts.cleanup_error = String(error); });
        await drive(`
const invoke = window.__TAURI_INTERNALS__.invoke;
const scoped = await invoke('user_partitions_load_active_server_profiles_scoped');
await invoke('user_partitions_save_active_server_profiles', { expectedUserId: scoped.userId,
    profiles: scoped.profiles.filter(p => p.id !== '${profileId}') });
await invoke('delete_credential', { account: 'server_${profileId}' });
window.dispatchEvent(new Event('aeroftp-profiles-changed')); return true;
`).catch(error => { receipts.cleanup_error = String(error); });
    }
    if (began) await driver('--finish', '--session-file', session).catch(error => { receipts.cleanup_error = String(error); });
    if (server && server.exitCode === null && server.signalCode === null) {
        server.kill('SIGTERM');
        const deadline = Date.now() + 2000;
        while (server.exitCode === null && server.signalCode === null && Date.now() < deadline) await pause(50);
        if (server.exitCode === null && server.signalCode === null) server.kill('SIGKILL');
    }
    if (receipts.cleanup_error) { receipts.status = 'FAIL'; process.exitCode = 1; }
    await writeFile(join(run, 'receipt.json'), JSON.stringify(receipts, null, 2) + '\n');
    console.log(`${receipts.status}: local and FTP Mirror. Evidence: ${run}/receipt.json`);
}
