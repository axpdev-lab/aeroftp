// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { readFileSync, readlinkSync, existsSync, realpathSync, openSync, writeFileSync, closeSync, unlinkSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { resolve, join, isAbsolute, dirname, basename } from 'node:path';

// Serialize every adapter process for this inspector. Never remove a live lock automatically.
export function acquireDriverLock(lock, write = writeFileSync) {
    let fd;
    try { fd = openSync(lock, 'wx', 0o600); }
    catch { throw Error(`Another DEV driver owns ${lock}; wait or inspect its PID before cleanup`); }
    // This process created the lock, so a failed PID write must not leave a false live owner.
    try { write(fd, `${process.pid}\n`); }
    catch (error) { closeSync(fd); unlinkSync(lock); throw error; }
    return fd;
}

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
    // Every row must be read, and every listener on the port must be the loopback one: a list of
    // wildcard spellings (0.0.0.0, *, [::]) let `:::PORT`, `0.0.0.0%eth0:PORT` or a LAN address
    // through next to the loopback row. Rows are `State Recv-Q Send-Q Local Peer Process`.
    const lines = [];
    for (const line of listener.split('\n')) {
        const fields = line.trim().split(/\s+/);
        if (line.trim() === '' || fields[0] === 'State') continue;
        if (fields[0] !== 'LISTEN' || fields.length < 5) throw Error(`Development inspector check cannot read this ss line: ${line.trim()}`);
        lines.push({ line, local: fields[3] });
    }
    const other = lines.find(({ local }) => local !== `127.0.0.1:${port}`);
    if (other) throw Error(`Development inspector must listen only on 127.0.0.1:${port}; ss shows ${other.local}`);
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
