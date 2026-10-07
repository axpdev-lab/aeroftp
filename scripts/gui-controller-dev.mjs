#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// Linux development fixture driver. Never bundled into the application.
import { readFileSync, readlinkSync, existsSync } from 'node:fs';
import { execFileSync, spawnSync } from 'node:child_process';
import { resolve, join } from 'node:path';
import { homedir } from 'node:os';

const [mode, value] = process.argv.slice(2);
if (!['--request', '--js', '--unlock'].includes(mode) || (mode !== '--unlock' && !value)) {
    console.error('Usage: node scripts/gui-controller-dev.mjs --request <request-json> | --js <test-file> | --unlock');
    process.exit(2);
}
try {
    const fixture = resolve(process.env.AEROFTP_GUI_TEST_ROOT || '/tmp/aeroftp-gui-native-01a110c5');
    if (!fixture.startsWith('/tmp/aeroftp-gui-')) throw Error('Expected an isolated /tmp/aeroftp-gui-* fixture');
    const listener = execFileSync('ss', ['-ltnp', 'sport = :9222'], { encoding: 'utf8' });
    const pid = listener.match(/"aeroftp",pid=(\d+)/)?.[1];
    if (!pid || !listener.includes('127.0.0.1:9222')) throw Error('Owned loopback dev inspector unavailable');
    const env = Object.fromEntries(readFileSync(`/proc/${pid}/environ`, 'utf8').split('\0')
        .filter(item => item.includes('=')).map(item => [item.slice(0, item.indexOf('=')), item.slice(item.indexOf('=') + 1)]));
    if (['CONFIG', 'CACHE', 'DATA'].some(key => env[`XDG_${key}_HOME`] !== join(fixture, key.toLowerCase())) ||
        env.WEBKIT_INSPECTOR_HTTP_SERVER !== '127.0.0.1:9222') throw Error('Inspector is not the isolated test fixture');
    const cwd = readlinkSync(`/proc/${pid}/cwd`);
    if (!existsSync(join(cwd, 'src/gui/controller.ts'))) throw Error('Expected AeroFTP development checkout');
    const driver = join(homedir(), '.codex/skills/gui-drive/scripts/drive.mjs');
    if (!existsSync(driver)) throw Error('Install the gui-drive development skill first');
    let body;
    if (mode === '--js') body = readFileSync(resolve(value), 'utf8');
    else if (mode === '--unlock') {
        const password = process.env.AEROFTP_GUI_TEST_MASTER;
        if (!password) throw Error('Set AEROFTP_GUI_TEST_MASTER to the artificial fixture password');
        body = `const controller = window.__aeroftpController;
            if (!controller) throw Error('Development controller unavailable');
            if (!controller.state().locked) return { unlocked: true, already_unlocked: true };
            const input = document.querySelector('input[type=password]');
            if (!input?.closest('form')) throw Error('Fixture unlock form unavailable');
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, ${JSON.stringify(password)});
            input.dispatchEvent(new Event('input', { bubbles: true }));
            await new Promise(resolve => setTimeout(resolve, 50)); input.closest('form').requestSubmit();
            const deadline = Date.now() + 20000;
            while (controller.state().locked) {
                if (Date.now() >= deadline) throw Error('Fixture unlock failed');
                await new Promise(resolve => setTimeout(resolve, 100));
            }
            return { unlocked: true };`;
    } else {
        const request = JSON.parse(value);
        body = `const request = ${JSON.stringify(request)};
            const { validateGuiRequest } = await import('/src/gui/controller.ts'); validateGuiRequest(request);
            const invoke = window.__TAURI_INTERNALS__.invoke;
            const name = request.name;
            const toolName = name === 'state' ? 'gui_state' : name === 'wait' ? 'gui_wait' : 'gui_run';
            const args = name === 'state' ? {} : { ...request.args,
                ...(name === 'wait' ? {} : { intent: name }),
                ...(request.timeout_ms === undefined ? {} : { timeout_ms: request.timeout_ms }),
                ...(request.if_revision === undefined ? {} : { if_revision: request.if_revision }) };
            const sessionId = 'gui-dev-driver';
            const prepared = await invoke('prepare_ai_tool_approval', { toolName, args, sessionId });
            let approvalGrantId;
            if (prepared.approvalRequired) {
                const grant = await invoke('grant_ai_tool_approval', { requestId: prepared.requestId,
                    rememberForSession: false, skipNativeDialog: true });
                if (!grant.approved || !grant.grantId) throw Error('Fixture expert-mode authorization unavailable');
                approvalGrantId = grant.grantId;
            }
            return await invoke('execute_ai_tool', { toolName, args, sessionId, approvalGrantId });`;
    }
    const result = spawnSync(process.execPath, [driver, body, '55000'], { encoding: 'utf8', timeout: 60000 });
    if (result.stdout) process.stdout.write(result.stdout);
    if (result.stderr) process.stderr.write(result.stderr);
    process.exit(result.status ?? 1);
} catch (error) { console.error(error.message); process.exit(1); }
