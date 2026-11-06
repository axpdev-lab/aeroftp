// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { parseAst, transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import APP from '../App.tsx?raw';
import INTRO_HUB from '../components/IntroHub/IntroHub.tsx?raw';
import MY_SERVERS from '../components/IntroHub/MyServersPanel.tsx?raw';

/**
 * A connect from the servers screen or the Quick Connect form ends the live
 * connection before it opens the new one. The tab of that connection used to
 * stay active meanwhile, so the in-session readers showed it during the
 * connect, and a Cancel or a failure left it active and marked connected over
 * a backend that was already gone: going back to the file manager found it
 * "already active" and never reconnected it.
 *
 * `parkActiveSession` puts that tab aside (`cached`, nothing active) before the
 * backend goes. These tests pin where it runs, what it does, and that a
 * cancelled connect keeps the tabs that were open before it.
 */

type Node = { type: string; start: number; end: number; [key: string]: unknown };
const isNode = (value: unknown): value is Node => !!value && typeof value === 'object' && typeof (value as Node).type === 'string';

/** The name a node gives to the code below it, if it names a context. */
function contextName(node: Node, parent: Node | null): string | null {
    if (node.type === 'VariableDeclarator' && (node.id as Node).type === 'Identifier' && node.init) {
        return (node.id as unknown as { name: string }).name;
    }
    if (node.type === 'JSXAttribute' && parent?.type === 'JSXOpeningElement') {
        const element = (parent.name as unknown as { name?: string }).name ?? 'element';
        const attribute = (node.name as unknown as { name?: string }).name ?? 'attribute';
        return `<${element} ${attribute}>`;
    }
    return null;
}

const literalArg = (node: Node): unknown => {
    const first = (node.arguments as Node[])[0];
    return first?.type === 'Literal' ? (first as unknown as { value: unknown }).value : undefined;
};
const calleeName = (node: Node): string | null => {
    const callee = node.callee as Node;
    return callee.type === 'Identifier' ? (callee as unknown as { name: string }).name : null;
};

interface Call { line: number; context: string; command: string; start: number; contextStart: number }

/** Every `invoke`/`runConnect` of one of `commands`, with its enclosing named context. */
function calls(commands: ReadonlySet<string>, source: string = APP): Call[] {
    const program = parseAst(source, { lang: 'tsx' }, 'App.tsx') as unknown as Node;
    const found: Call[] = [];
    const visit = (node: Node, parent: Node | null, context: { name: string; start: number } | null) => {
        const name = contextName(node, parent);
        const next = name ? { name, start: node.start } : context;
        if (node.type === 'CallExpression' && next) {
            const callee = calleeName(node);
            const command = literalArg(node);
            if ((callee === 'invoke' || callee === 'runConnect') && typeof command === 'string' && commands.has(command)) {
                found.push({
                    line: source.slice(0, node.start).split('\n').length,
                    context: next.name,
                    command,
                    start: node.start,
                    contextStart: next.start,
                });
            }
        }
        for (const value of Object.values(node)) {
            if (Array.isArray(value)) value.filter(isNode).forEach(child => visit(child, node, next));
            else if (isNode(value)) visit(value, node, next);
        }
    };
    visit(program, null, null);
    return found;
}

const TEARDOWN = new Set(['provider_disconnect', 'disconnect_ftp']);
const CONNECT = new Set(['provider_connect', 'connect_ftp']);
const PARK = 'parkActiveSession();';
const HOST_KEY_CHECK = 'checkSftpHostKey(';

/** Connects that replace the live connection with a new one: they park first. */
const PARKING = ['connectToFtpImpl', '<IntroHub onSavedServerConnect>', 'openMtpSessionFromLiveDevice'];
/** Teardowns that are not a connect over the live tab, and why. */
const NOT_A_CONNECT_OVER_THE_TAB: Record<string, string> = {
    disconnectFromFtp: 'the user ends the session',
    lockAndDropBackendConnection: 'the cleanup a Cancel runs',
    switchSession: 'a tab switch, with its own cancel branch',
    endCurrentConnection: 'the AeroCloud tab, which has no session tab',
    switchGitHubBranch: 'the same GitHub session on another branch',
    handlePortableDeviceClosed: 'the portable device went away',
    loadRemoteFiles: 'the MTP device was unplugged during a listing',
};

/** The source of a context up to a call, without the call itself. */
const pathTo = (call: Call, source: string = APP) => source.slice(call.contextStart, call.start);

describe('a connect parks the live tab before it ends the connection', () => {
    it('knows every teardown in App.tsx', () => {
        const contexts = [...new Set(calls(TEARDOWN).map(c => c.context))].sort();
        expect(contexts).toEqual([...PARKING, ...Object.keys(NOT_A_CONNECT_OVER_THE_TAB)].sort());
    });

    it('parks before every teardown of a connect, after its host-key check', () => {
        // Not just before the first await: the park must sit between the
        // host-key question (a refused key leaves the tab as it was) and the
        // disconnect, on the path of each teardown.
        const misplaced = calls(TEARDOWN)
            .filter(c => PARKING.includes(c.context))
            .filter(c => {
                const path = pathTo(c);
                const park = path.lastIndexOf(PARK);
                return park < 0 || path.lastIndexOf(HOST_KEY_CHECK) > park;
            })
            .map(c => `App.tsx:${c.line} ${c.command} in ${c.context}`);
        expect(calls(TEARDOWN).filter(c => PARKING.includes(c.context)).length).toBeGreaterThanOrEqual(6);
        expect(misplaced).toEqual([]);
    });

    it('parks before every connect of those paths, so a Cancel never leaves a tab connected', () => {
        const unparked = calls(CONNECT)
            .filter(c => PARKING.includes(c.context))
            .filter(c => pathTo(c).lastIndexOf(PARK) < 0)
            .map(c => `App.tsx:${c.line} ${c.command} in ${c.context}`);
        expect(calls(CONNECT).filter(c => PARKING.includes(c.context)).length).toBeGreaterThanOrEqual(4);
        expect(unparked).toEqual([]);
    });

    it('flags a teardown with no park before it', () => {
        const source = "const connectToFtpImpl = async () => { await invoke('provider_disconnect'); parkActiveSession(); };";
        const [call] = calls(TEARDOWN, source);
        expect(pathTo(call, source).lastIndexOf(PARK)).toBe(-1);
    });

    it('flags a park asked before the host-key check', () => {
        const source = "const connectToFtpImpl = async () => { parkActiveSession(); await checkSftpHostKey(h, 22); await invoke('disconnect_ftp'); };";
        const [call] = calls(TEARDOWN, source);
        const path = pathTo(call, source);
        expect(path.lastIndexOf(HOST_KEY_CHECK)).toBeGreaterThan(path.lastIndexOf(PARK));
    });

    it('uses the same helper for the "+" new tab', () => {
        const start = APP.indexOf('  const handleNewTabFromSavedServer = () => {');
        const body = APP.slice(start, APP.indexOf('\n  };', start));
        expect(start).toBeGreaterThan(-1);
        // "+" changes no backend, so a portable device tab stays (going back
        // reuses the live device session).
        expect(body).toContain('parkActiveSession(true);');
        expect(body).not.toContain('setSessions(');
        expect(body).not.toContain('setActiveSessionId(');
    });

    it('shows a clicked parked tab connected once it reconnects, and parked again if it does not', () => {
        // The tab bar calls switchSession directly: with the workspace marked
        // disconnected by the park, the tab reconnected behind a panel that
        // still read as disconnected. Marked connected before the reconnect,
        // its cached files took remote actions while another server held the
        // backend, and the provider capabilities were read from that server.
        // A failed reconnect must not leave a dead tab active either, which a
        // click would no longer retry.
        const start = APP.indexOf('  const switchSession = async');
        const body = APP.slice(start, APP.indexOf('\n  const ', start + 1));
        const success = body.indexOf("setSessions(prev => prev.map(s => s.id === sessionId ? { ...s, status: 'connected' } : s));");
        const connected = body.indexOf('setIsConnected(true);');
        expect(success).toBeGreaterThan(-1);
        expect(connected).toBeGreaterThan(success);
        expect(body.split('setIsConnected(true);').length - 1).toBe(1);
        expect(body.slice(body.indexOf('setActiveSessionId(sessionId);'), success)).toContain('await ');
        expect(body).toContain('const fromParked = activeSessionId === null;');
        // The refused host key, the two cancel branches and the failure branch.
        expect(body.split('backToParked();').length - 1).toBe(4);
        // Back to parked means nothing active and a disconnected workspace, so
        // the next click on the tab passes the "already active" return and
        // reconnects again.
        const back = body.slice(body.indexOf('const backToParked = () => {'), body.indexOf('\n    };', body.indexOf('const backToParked = () => {')));
        expect(back).toContain('if (!fromParked) return;');
        expect(back).toContain('setActiveSessionId(null);');
        expect(back).toContain('setIsConnected(false);');
        expect(body.indexOf('if (activeSessionId === sessionId) {')).toBeLessThan(body.indexOf('const fromParked'));
        expect(APP).toContain('onTabClick={switchSession}');
    });

    it('ends every device tab when a switch to another server replaces the backend', () => {
        // Kept among the others, a device tab outlived the switch that ended
        // its session, and a click on it disconnected the server just reached.
        const start = APP.indexOf('  const switchSession = async');
        const body = APP.slice(start, APP.indexOf('\n  const ', start + 1));
        const drop = body.indexOf("if (targetSession.connectionParams?.protocol !== 'mtp'");
        expect(drop).toBeGreaterThan(body.indexOf('checkSftpHostKey('));
        expect(drop).toBeLessThan(body.indexOf('setActiveSessionId(sessionId);'));
        const branch = body.slice(drop, body.indexOf('\n    }\n', drop));
        expect(branch).toContain("setSessions(prev => prev.filter(s => s.connectionParams?.protocol !== 'mtp'));");
        expect(branch).toContain('setActivePortableDeviceId(null);');
    });

    it('keeps a stalled reconnect listing of a parked tab cancellable with the panel hidden', () => {
        // The workspace reads as connected only once the reconnect succeeds,
        // so the remote panel and its spinner Cancel are hidden meanwhile; a
        // stalled listing holds the provider, so this Cancel is the way out.
        const at = APP.indexOf('{remoteListLoading && !isConnected && !showConnectionScreen && (');
        expect(at).toBeGreaterThan(-1);
        const overlay = APP.slice(at, APP.indexOf('{stillConnecting && (', at));
        expect(overlay).toContain('onClick={cancelRemoteNavigation}');
        // No rows to act on while another server may hold the backend.
        expect(overlay).not.toContain('remoteFiles');
    });

    it('keeps the disconnect cleanup off the overlay of a parked tab being reconnected', () => {
        // A parked tab reads as disconnected until its reconnect succeeds, and
        // switchSession restores its crypt overlay before that: the cleanup
        // effects keyed on the disconnected state would clear it underneath.
        for (const vault of ['rcloneCryptVaultId', 'aeroCryptVaultId']) {
            const guard = `if (isConnected || !${vault} || activeSessionId) return;`;
            const at = APP.indexOf(guard);
            expect(at, vault).toBeGreaterThan(-1);
            const effect = APP.slice(at, APP.indexOf('}, [', at) + 80);
            expect(effect, vault).toContain("invoke('provider_clear_crypt_overlay', { full: true })");
            expect(effect, vault).toContain(`}, [isConnected, ${vault}, activeSessionId]);`);
        }
    });

    it('keeps a parked tab parked when its host key is refused', () => {
        // The card and Active Sessions mark the workspace connected before they
        // call switchSession: a refused key must not leave it connected with
        // nothing active.
        const start = APP.indexOf('  const switchSession = async');
        const body = APP.slice(start, APP.indexOf('\n  const ', start + 1));
        expect(body).toContain('if (!accepted) { backToParked(); return; }');
        expect(body.indexOf('const backToParked')).toBeLessThan(body.indexOf('checkSftpHostKey('));
    });

    it('keeps the parked tabs when a portable device opens, closes or is unplugged', () => {
        // The device open parks the live tab first: replacing the whole list,
        // then emptying it on close, threw every parked tab away.
        const open = APP.slice(APP.indexOf('  const openMtpSessionFromLiveDevice = useCallback('), APP.indexOf('  /** PLACES portable row'));
        expect(open).not.toContain('setSessions([newSession])');
        expect(open).toContain("setSessions(prev => [...prev.filter(s => s.connectionParams?.protocol !== 'mtp'), newSession]);");
        const closed = APP.slice(APP.indexOf('  const handlePortableDeviceClosed = useCallback('), APP.indexOf('  // Unplug while a device session is live.'));
        const unplugged = APP.slice(APP.indexOf('      // Gvfs-backed MTP: mount vanishes on unplug'), APP.indexOf("try { await invoke('mtp_close_device', { deviceId: null }); }"));
        // An unplug while another server's tab is live leaves its provider alone.
        expect(closed).toContain("const deviceOwnsProvider = !activeSessionId || liveConnectionParams.protocol === 'mtp';");
        expect(closed).toMatch(/if \(deviceOwnsProvider\) \{\s*try \{ await invoke\('provider_disconnect'\); \}/);
        expect(closed.indexOf("setSessions(prev => prev.filter(s => s.connectionParams?.protocol !== 'mtp'));"))
            .toBeLessThan(closed.indexOf("if (liveConnectionParams.protocol === 'mtp') {"));
        for (const [name, path] of [['close', closed], ['unplug during a listing', unplugged]]) {
            expect(path.length, name).toBeGreaterThan(0);
            expect(path, name).not.toContain('setSessions([])');
            expect(path, name).toContain("setSessions(prev => prev.filter(s => s.connectionParams?.protocol !== 'mtp'));");
        }
    });

    it('lets the park keep the old tab\'s Navigation Sync', () => {
        // Reset before the host-key question, the park would store the reset
        // values in the tab: parkActiveSession resets them itself, afterwards.
        const early = calls(TEARDOWN)
            .filter(c => PARKING.includes(c.context))
            .filter(c => {
                const path = pathTo(c);
                const park = path.lastIndexOf(PARK);
                const branch = path.lastIndexOf('setLoading(true);', park);
                const between = path.slice(Math.max(branch, 0), park);
                return between.includes('setIsSyncNavigation(false)') || between.includes('setSyncBasePaths(null)');
            })
            .map(c => `App.tsx:${c.line} ${c.command} in ${c.context}`);
        expect(early).toEqual([]);
    });

    it('parks when the connection form hands over an OAuth session it already connected', () => {
        const start = APP.indexOf('  const connectToFtpImpl = async (');
        const at = APP.indexOf('    if (isOAuth) {\n', start);
        const branch = APP.slice(at, APP.indexOf('setIsConnected(true);', at));
        expect(at).toBeGreaterThan(start);
        expect(branch).toContain(PARK);
    });

    it('parks before an OAuth or 4shared connect replaces the backend in My Servers', () => {
        for (const run of ['scopedCancellable(doOAuthConnect)', 'scopedCancellable(doFourSharedConnect)']) {
            const at = MY_SERVERS.indexOf(run);
            const branch = MY_SERVERS.slice(MY_SERVERS.lastIndexOf('setOauthConnecting(server.id);', at), at);
            expect(at, run).toBeGreaterThan(-1);
            expect(branch, run).toContain('onParkActiveSession?.();');
        }
        expect(MY_SERVERS).toContain('    onParkActiveSession,\n}: MyServersPanelProps)');
        expect(INTRO_HUB).toContain('onParkActiveSession={onParkActiveSession}');
        expect(APP).toContain('onParkActiveSession={parkActiveSession}');
    });
});

/** Runs one slice of App.tsx as a function, with the given scope. */
async function runSlice<T>(startMarker: string, endMarker: string, name: string, ctx: Record<string, unknown>): Promise<T> {
    const start = APP.indexOf(startMarker);
    const end = APP.indexOf(endMarker, start);
    if (start < 0 || end < 0) throw new Error(`Missing ${name} in App.tsx`);
    const source = APP.slice(start, end);
    for (const setter of new Set(source.match(/\bset[A-Z]\w+/g))) ctx[setter] ??= vi.fn();
    const { code } = await transformWithOxc(source, `${name}.ts`);
    return new Function(...Object.keys(ctx), `${code}; return ${name};`)(...Object.values(ctx)) as T;
}

describe('parkActiveSession', () => {
    const state = {
        remoteFiles: [{ name: 'r' }],
        localFiles: [{ name: 'l' }],
        currentRemotePath: '/remote',
        currentLocalPath: '/home/user',
        isSyncNavigation: true,
        syncBasePaths: { remote: '/remote', local: '/home/user' },
        aeroVaultOverlaySession: { sessionId: 'vault' },
    };
    const park = async (activeSessionId: string | null, keepDeviceTab?: boolean) => {
        const ctx: Record<string, unknown> = {
            ...state, activeSessionId, useLatestCallback: (fn: unknown) => fn, mtpGvfsMountPathRef: { current: '/run/user/gvfs/mtp' },
            quotaVersionRef: { current: 4 }, quotaConnectionRef: { current: 9 },
        };
        const fn = await runSlice<(keep?: boolean) => void>('  const parkActiveSession = useLatestCallback(', '  /** Shared open path', 'parkActiveSession', ctx);
        fn(keepDeviceTab);
        return ctx as Record<string, ReturnType<typeof vi.fn>> & {
            mtpGvfsMountPathRef: { current: string | null }; quotaVersionRef: { current: number }; quotaConnectionRef: { current: number };
        };
    };
    const applyAll = (setSessions: ReturnType<typeof vi.fn>, tabs: { id: string; status: string; connectionParams?: { protocol: string } }[]) =>
        setSessions.mock.calls.reduce((prev, [update]) => (update as (p: typeof tabs) => typeof tabs)(prev), tabs);

    it('turns the active tab into a cached one with its state, and leaves nothing active', async () => {
        const ctx = await park('a');
        const update = ctx.setSessions.mock.calls[0][0] as (prev: object[]) => object[];
        const [a, b] = update([{ id: 'a', status: 'connected' }, { id: 'b', status: 'cached' }]);
        expect(a).toEqual({
            id: 'a',
            status: 'cached',
            remoteFiles: state.remoteFiles,
            localFiles: state.localFiles,
            remotePath: '/remote',
            localPath: '/home/user',
            isSyncNavigation: true,
            syncBasePaths: state.syncBasePaths,
            aeroVaultOverlaySession: state.aeroVaultOverlaySession,
        });
        expect(b).toEqual({ id: 'b', status: 'cached' });
        expect(ctx.setActiveSessionId).toHaveBeenCalledWith(null);
        expect(ctx.setIsConnected).toHaveBeenCalledWith(false);
        expect(ctx.setAeroVaultOverlaySession).toHaveBeenCalledWith(null);
        // The remote panel no longer lists the parked session's files while the
        // next connection lists; the cached tab keeps them (asserted above).
        expect(ctx.setRemoteFiles).toHaveBeenCalledWith([]);
        expect(ctx.setSelectedRemoteFiles).toHaveBeenCalledWith(new Set());
        // Search results of the parked session would list over the next one,
        // and its query would filter the next listing.
        expect(ctx.setRemoteSearchResults).toHaveBeenCalledWith(null);
        expect(ctx.setRemoteSearchQuery).toHaveBeenCalledWith('');
        // A quota fetch or scan of the parked session cannot land on the next.
        expect(ctx.quotaVersionRef.current).toBe(5);
        expect(ctx.quotaConnectionRef.current).toBe(10);
        expect(ctx.setStorageQuota).toHaveBeenCalledWith(null);
    });

    it('drops a portable device tab and its unplug watch when a connect replaces the backend', async () => {
        const tabs = [
            { id: 'a', status: 'connected', connectionParams: { protocol: 'ftp' } },
            { id: 'phone', status: 'cached', connectionParams: { protocol: 'mtp' } },
        ];
        const ctx = await park('a');
        expect(applyAll(ctx.setSessions, tabs).map(t => `${t.id}:${t.status}`)).toEqual(['a:cached']);
        expect(ctx.setActivePortableDeviceId).toHaveBeenCalledWith(null);
        expect(ctx.mtpGvfsMountPathRef.current).toBeNull();
    });

    it('keeps the device tab for the "+" new tab, which changes no backend', async () => {
        const tabs = [{ id: 'phone', status: 'connected', connectionParams: { protocol: 'mtp' } }];
        const ctx = await park('phone', true);
        expect(applyAll(ctx.setSessions, tabs).map(t => `${t.id}:${t.status}`)).toEqual(['phone:cached']);
        expect(ctx.setActivePortableDeviceId).not.toHaveBeenCalled();
        expect(ctx.mtpGvfsMountPathRef.current).toBe('/run/user/gvfs/mtp');
    });

    it('parks no tab when none is active', async () => {
        const tabs = [{ id: 'b', status: 'cached', connectionParams: { protocol: 'sftp' } }];
        const ctx = await park(null);
        expect(applyAll(ctx.setSessions, tabs)).toEqual(tabs);
        expect(ctx.setActiveSessionId).toHaveBeenCalledWith(null);
        expect((await park(null, true)).setSessions).not.toHaveBeenCalled();
    });
});

describe('a cancelled connect keeps the tabs that were open before it', () => {
    const abort = async (recorded: { phaseVersion: number; ids: ReadonlySet<string> } | null, phaseVersion: number) => {
        const ctx: Record<string, unknown> = {
            remoteConnectPhaseRef: { current: 'connect' },
            connectStartSessionsRef: { current: recorded },
            lockAndDropBackendConnection: vi.fn(async () => true),
            clearCryptOverlayState: vi.fn(),
            humanLog: { logRaw: vi.fn() },
            showToastNotifications: false,
            toast: { info: vi.fn() },
            t: (key: string) => key,
        };
        const fn = await runSlice<(owns: () => boolean, version: number) => Promise<void>>(
            '  const abortConnectAfterCancel = async', '  // V2: a cancelled tab SWITCH', 'abortConnectAfterCancel', ctx);
        await fn(() => true, phaseVersion);
        const setSessions = ctx.setSessions as ReturnType<typeof vi.fn>;
        const update = setSessions.mock.calls[0][0] as (prev: { id: string; status: string }[]) => { id: string; status: string }[];
        const kept = update([{ id: 'a', status: 'connected' }, { id: 'b', status: 'cached' }, { id: 'added', status: 'connected' }]);
        return { ctx: ctx as Record<string, ReturnType<typeof vi.fn>>, tabs: kept.map(s => s.id), statuses: kept.map(s => s.status) };
    };

    it('removes only the tab the cancelled connect added, and parks the ones it keeps', async () => {
        const { ctx, tabs, statuses } = await abort({ phaseVersion: 3, ids: new Set(['a', 'b']) }, 3);
        expect(tabs).toEqual(['a', 'b']);
        // The backend is gone: no kept tab may read as connected.
        expect(statuses).toEqual(['cached', 'cached']);
        expect(ctx.setActiveSessionId).toHaveBeenCalledWith(null);
        expect(ctx.setShowConnectionScreen).toHaveBeenCalledWith(true);
    });

    it('clears every tab when the connect started with none', async () => {
        expect((await abort({ phaseVersion: 3, ids: new Set() }, 3)).tabs).toEqual([]);
    });

    it('keeps every tab, parked, for a connect it has no record of', async () => {
        // Another entry point (the AeroCloud tab, a GitHub branch switch) bumped
        // the phase version without recording the tabs. It added none, so a
        // Cancel there no longer costs the user every open tab.
        for (const recorded of [{ phaseVersion: 3, ids: new Set(['a', 'b']) }, null]) {
            const { tabs, statuses } = await abort(recorded, 4);
            expect(tabs).toEqual(['a', 'b', 'added']);
            expect(statuses).toEqual(['cached', 'cached', 'cached']);
        }
    });

    it('records the tabs when a connect phase starts, and hands its version to the abort', () => {
        const start = APP.indexOf('  const asConnectPhase = ');
        const wrapper = APP.slice(start, APP.indexOf('\n    };', start));
        const bump = wrapper.indexOf('const phaseVersion = ++remoteConnectPhaseVersionRef.current;');
        const record = wrapper.indexOf('connectStartSessionsRef.current = { phaseVersion, ids: new Set(sessions.map(s => s.id)) };');
        expect(bump).toBeGreaterThan(-1);
        expect(record).toBeGreaterThan(bump);
        expect(APP).toContain('else if (kind) await abortConnectAfterCancel(owns, phaseVersion);');
    });
});
