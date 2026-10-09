#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// Complete Linux DEV driver; never bundled into the application.
import { readFileSync, writeFileSync, unlinkSync, existsSync, openSync, closeSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { evaluate, inspectorPort, scriptTimeout } from './lib/gui-inspector.mjs';
import { fixtureRoot, inspectFixture, readSession, requestBody } from './lib/gui-dev-runtime.mjs';

const usage = `Owned AeroFTP DEV controller (Node 22+, Linux)
  --begin --actor <name> [--session-file <path>]
  --request <json> [--session-file <path>]
  --finish [--session-file <path>]
  --js <file>          Full DEV YOLO inspector/native script
  --unlock            Uses AEROFTP_GUI_TEST_MASTER, never prints it
  --help
Required: AEROFTP_GUI_TEST_ROOT. Optional: INSPECTOR_PORT (9222).
Full scripts: AEROFTP_GUI_SCRIPT_TIMEOUT_MS (55000, 100..3600000); broker deadlines are unchanged.
Request: {"name":"show_view","args":{"view":"servers"},"timeout_ms":10000}
This is the DEV envelope, not public gui_run's {"intent":...}.
No automatic retries. begin/request/finish share one session; other scripts are serialized.`;

async function main() {
    const argv = process.argv.slice(2);
    if (argv.length === 1 && argv[0] === '--help') { console.log(usage); return; }
    let mode, value, actor = process.env.AEROFTP_GUI_ACTOR, sessionPath, actorOption = false;
    for (let i = 0; i < argv.length; i++) {
        const arg = argv[i];
        if (arg === '--actor' || arg === '--session-file') {
            const next = argv[++i];
            if (!next || next.startsWith('--')) throw Error(`Missing ${arg} value`);
            if (arg === '--actor') { actor = next; actorOption = true; } else sessionPath = resolve(next);
        } else if (['--begin', '--request', '--finish', '--js', '--unlock'].includes(arg) && !mode) {
            mode = arg;
            if (arg === '--request' || arg === '--js') {
                value = argv[++i]; if (!value || value.startsWith('--')) throw Error(`Missing ${arg} value`);
            }
        } else throw Error(`Unknown or duplicate option: ${arg}. Use --help`);
    }
    if (!mode) throw Error('Select one mode. Use --help');
    if (actorOption && mode !== '--begin') throw Error('--actor is set only at --begin');
    // Give envelope mistakes a useful error before contacting a process or approving a tool.
    let request;
    if (mode === '--request') {
        try { request = JSON.parse(value); }
        catch { throw Error('Invalid DEV request JSON. Use --help'); }
        if (!request || typeof request !== 'object' || Array.isArray(request) || typeof request.name !== 'string') {
            throw Error('DEV request requires name, not intent. Use --help');
        }
        const allowed = ['name', 'args', 'timeout_ms', 'if_revision'];
        const unknown = Object.keys(request).find(key => !allowed.includes(key));
        if (unknown) throw Error(`Unknown DEV request field: ${unknown}; expected ${allowed.join(', ')}`);
    }
    const root = fixtureRoot(process.env.AEROFTP_GUI_TEST_ROOT);
    const port = inspectorPort(process.env.INSPECTOR_PORT);
    const fixture = inspectFixture(root, port);
    sessionPath ??= join(root, 'controller-session.json');
    // Serialize every adapter process for this inspector. Never remove a live lock automatically.
    const lock = join(root, 'controller-driver.lock');
    let fd;
    try { fd = openSync(lock, 'wx', 0o600); writeFileSync(fd, `${process.pid}\n`); }
    catch { throw Error(`Another DEV driver owns ${lock}; wait or inspect its PID before cleanup`); }
    try {
        let body;
        if (mode === '--begin') {
            if (!actor?.trim()) throw Error('--begin requires --actor or AEROFTP_GUI_ACTOR');
            if (existsSync(sessionPath)) throw Error('Session file already exists; finish it or choose another --session-file');
            body = `return await window.__TAURI_INTERNALS__.invoke('gui_dev_session_begin', { label: ${JSON.stringify(actor)} });`;
        } else if (mode === '--request' || mode === '--finish') {
            const session = readSession(sessionPath, fixture);
            body = mode === '--request' ? requestBody(request, session.session_id) :
                `await window.__TAURI_INTERNALS__.invoke('gui_dev_session_end', { sessionId: ${JSON.stringify(session.session_id)} }); return { finished: true };`;
        } else if (mode === '--js') body = readFileSync(resolve(value), 'utf8');
        else {
            const password = process.env.AEROFTP_GUI_TEST_MASTER;
            if (!password) throw Error('Set AEROFTP_GUI_TEST_MASTER for the owned DEV master');
            body = `const controller = window.__aeroftpController;
                if (!controller.state().locked) return { unlocked: true, already_unlocked: true };
                const input = document.querySelector('input[type=password]');
                if (!input?.closest('form')) throw Error('DEV unlock form unavailable');
                Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, ${JSON.stringify(password)});
                input.dispatchEvent(new Event('input', { bubbles: true }));
                await new Promise(resolve => setTimeout(resolve, 50)); input.closest('form').requestSubmit();
                const deadline = Date.now() + 20000;
                while (controller.state().locked) {
                    if (Date.now() >= deadline) throw Error('DEV unlock failed');
                    await new Promise(resolve => setTimeout(resolve, 100));
                }
                return { unlocked: true };`;
        }
        const result = await evaluate(body, { port, timeoutMs: mode === '--js' ? scriptTimeout(process.env.AEROFTP_GUI_SCRIPT_TIMEOUT_MS) : 55000 });
        if (mode === '--begin') {
            try {
                writeFileSync(sessionPath, JSON.stringify({ schema_version: 1, ...fixture, ...result }) + '\n', { flag: 'wx', mode: 0o600 });
            } catch (error) {
                // A failed local receipt must not orphan an active backend session.
                try {
                    await evaluate(`await window.__TAURI_INTERNALS__.invoke('gui_dev_session_end', { sessionId: ${JSON.stringify(result.session_id)} }); return true;`, { port });
                } catch {
                    // Preserve the local failure and say explicitly that cleanup is uncertain.
                    throw new Error(`Session receipt was not written (${error.code ?? 'write failed'}); backend cleanup also failed. Restart the owned fixture before beginning again.`);
                }
                throw error;
            }
        } else if (mode === '--finish') unlinkSync(sessionPath);
        console.log(JSON.stringify({ value: result, ...(mode === '--begin' ? { session_file: sessionPath } : {}) }));
        // execute_ai_tool serializes a tool outcome even on refusal. It is not a successful CLI run.
        if (result?.ok === false || result?.success === false) process.exitCode = 1;
    } finally { closeSync(fd); unlinkSync(lock); }
}
main().catch(error => { console.error(error.message); process.exitCode = 1; });
