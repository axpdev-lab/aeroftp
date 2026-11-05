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
        expect(body).toContain(PARK);
        expect(body).not.toContain('setSessions(');
        expect(body).not.toContain('setActiveSessionId(');
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
    const park = async (activeSessionId: string | null) => {
        const ctx: Record<string, unknown> = { ...state, activeSessionId, useLatestCallback: (fn: unknown) => fn };
        const fn = await runSlice<() => void>('  const parkActiveSession = useLatestCallback(', '  /** Shared open path', 'parkActiveSession', ctx);
        fn();
        return ctx as Record<string, ReturnType<typeof vi.fn>>;
    };

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
    });

    it('touches no tab when none is active', async () => {
        const ctx = await park(null);
        expect(ctx.setSessions).not.toHaveBeenCalled();
        expect(ctx.setActiveSessionId).toHaveBeenCalledWith(null);
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
        const update = setSessions.mock.calls[0][0] as (prev: { id: string }[]) => { id: string }[];
        return { ctx: ctx as Record<string, ReturnType<typeof vi.fn>>, tabs: update([{ id: 'a' }, { id: 'b' }, { id: 'added' }]).map(s => s.id) };
    };

    it('removes only the tab the cancelled connect added', async () => {
        const { ctx, tabs } = await abort({ phaseVersion: 3, ids: new Set(['a', 'b']) }, 3);
        expect(tabs).toEqual(['a', 'b']);
        expect(ctx.setActiveSessionId).toHaveBeenCalledWith(null);
        expect(ctx.setShowConnectionScreen).toHaveBeenCalledWith(true);
    });

    it('clears every tab when the connect started with none', async () => {
        expect((await abort({ phaseVersion: 3, ids: new Set() }, 3)).tabs).toEqual([]);
    });

    it('clears every tab for a connect it has no record of', async () => {
        // Another entry point (the AeroCloud tab, a GitHub branch) bumped the
        // phase version without recording the tabs: the old behavior stands.
        expect((await abort({ phaseVersion: 3, ids: new Set(['a', 'b']) }, 4)).tabs).toEqual([]);
        expect((await abort(null, 4)).tabs).toEqual([]);
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
