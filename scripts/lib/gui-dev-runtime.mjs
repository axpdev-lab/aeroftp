// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { readFileSync, readlinkSync, existsSync, realpathSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { resolve, join, isAbsolute, dirname, basename } from 'node:path';

export function fixtureRoot(value) {
    if (!value || !isAbsolute(value)) throw Error('Set AEROFTP_GUI_TEST_ROOT to the absolute owned fixture directory');
    const root = resolve(value);
    if (root === '/' || root === '/tmp' || root === '/home') throw Error('Expected a dedicated development fixture directory');
    return root;
}

export function validateFixtureEnvironment(root, env, port) {
    const home = env.HOME;
    const paths = { CONFIG: '.config', CACHE: '.cache', DATA: '.local/share' };
    for (const [key, suffix] of Object.entries(paths)) {
        const actual = env[`XDG_${key}_HOME`] || (home && join(home, suffix));
        const candidates = [join(root, key.toLowerCase()), join(root, suffix), join(root, 'home', suffix)];
        // HOME layout is isolated only if HOME itself is under the owned root.
        if (!actual || !candidates.includes(resolve(actual))) throw Error(`Inspector ${key.toLowerCase()} storage does not belong to the fixture`);
        if (realpathSync(actual) !== resolve(actual)) throw Error('Fixture storage must not redirect through symlinks');
    }
    if (env.WEBKIT_INSPECTOR_HTTP_SERVER !== `127.0.0.1:${port}`) throw Error('Inspector address does not match the owned loopback fixture');
}

/** ss columns distinguish the listening address from the wildcard peer address. */
export function inspectorPid(listener, port) {
    const lines = listener.split('\n').map(line => ({ line, local: line.trim().split(/\s+/)[3] }));
    if (lines.some(({ local }) => local === `0.0.0.0:${port}` || local === `*:${port}` || local === `[::]:${port}`)) {
        throw Error('Development inspector must listen only on loopback');
    }
    const owned = lines.filter(({ local }) => local === `127.0.0.1:${port}`);
    if (owned.length !== 1 || !/"aeroftp",pid=(\d+)/.test(owned[0].line)) throw Error('Owned loopback AeroFTP inspector unavailable');
    return Number(owned[0].line.match(/"aeroftp",pid=(\d+)/)[1]);
}

export function inspectFixture(root, port) {
    const listener = execFileSync('ss', ['-ltnp', `sport = :${port}`], { encoding: 'utf8' });
    const pid = inspectorPid(listener, port);
    const env = Object.fromEntries(readFileSync(`/proc/${pid}/environ`, 'utf8').split('\0')
        .filter(item => item.includes('=')).map(item => [item.slice(0, item.indexOf('=')), item.slice(item.indexOf('=') + 1)]));
    validateFixtureEnvironment(root, env, port);
    const cwd = developmentCheckout(readlinkSync(`/proc/${pid}/cwd`));
    return { pid, cwd, root, port };
}

export function developmentCheckout(cwd) {
    const root = basename(cwd) === 'src-tauri' ? dirname(cwd) : cwd;
    if (!existsSync(join(root, 'src/gui/controller.ts')) || !existsSync(join(root, 'src-tauri/Cargo.toml'))) {
        throw Error('Expected AeroFTP development checkout');
    }
    return root;
}

export function readSession(path, fixture) {
    const session = JSON.parse(readFileSync(path, 'utf8'));
    if (!session || typeof session !== 'object' || Array.isArray(session) || session.schema_version !== 1 || session.pid !== fixture.pid || session.port !== fixture.port || session.root !== fixture.root ||
        !/^gui-dev-[0-9a-f-]{36}$/.test(session.session_id) || session.actor?.id !== session.session_id || session.actor?.kind !== 'dev') {
        throw Error('Session does not belong to this running fixture; begin a new session');
    }
    return session;
}

export function requestBody(request, sessionId) {
    return `const request = ${JSON.stringify(request)};
        const { validateGuiRequest } = await import('/src/gui/controller.ts'); validateGuiRequest(request);
        const invoke = window.__TAURI_INTERNALS__.invoke;
        const name = request.name;
        const toolName = name === 'state' ? 'gui_state' : name === 'wait' ? 'gui_wait' : 'gui_run';
        const args = name === 'state' ? {} : { ...request.args,
            ...(name === 'wait' ? {} : { intent: name }),
            ...(request.timeout_ms === undefined ? {} : { timeout_ms: request.timeout_ms }),
            ...(request.if_revision === undefined ? {} : { if_revision: request.if_revision }),
            ...(request.speed_percent === undefined ? {} : { speed_percent: request.speed_percent }) };
        const sessionId = ${JSON.stringify(sessionId)};
        const prepared = await invoke('prepare_ai_tool_approval', { toolName, args, sessionId });
        let approvalGrantId;
        if (prepared.approvalRequired) {
            const grant = await invoke('grant_ai_tool_approval', { requestId: prepared.requestId,
                rememberForSession: false, skipNativeDialog: true });
            if (!grant.approved || !grant.grantId) throw Error('Native approval refused');
            approvalGrantId = grant.grantId;
        }
        return await invoke('execute_ai_tool', { toolName, args, sessionId, approvalGrantId });`;
}
