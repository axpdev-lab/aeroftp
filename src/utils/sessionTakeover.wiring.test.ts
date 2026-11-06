// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import APP from '../App.tsx?raw';

/**
 * Two follow-ups of the park of the live tab.
 *
 * A takeover outside a connect phase (the "+" new tab, a portable device
 * open, the OAuth or 4shared handshake of My Servers) parked the tab but left
 * its remote navigation running: a listing still in flight landed on the new
 * workspace, and its latch and spinner stayed held.
 *
 * A tab switch from a live tab that was cancelled or failed left the target
 * active over a backend the switch had already dropped, so a click on it
 * found it "already active" and never retried. The tab it left also kept
 * reading as connected.
 */

type Spy = ReturnType<typeof vi.fn>;

function slice(start: string, end: string, from = 0): string {
    const a = APP.indexOf(start, from);
    const b = APP.indexOf(end, a + start.length);
    if (a < 0 || b < 0) throw new Error(`Missing App.tsx block: ${start}`);
    return APP.slice(a, b);
}

/** Runs App.tsx source with the given scope; setters it names default to spies. */
async function run<T>(source: string, ctx: Record<string, unknown>, suffix: string): Promise<T> {
    for (const setter of new Set(source.match(/\bset[A-Z]\w+/g))) ctx[setter] ??= vi.fn();
    const { code } = await transformWithOxc(source, 'session-takeover.ts');
    return new Function(...Object.keys(ctx), `return (async () => { ${code}\n${suffix} })();`)(...Object.values(ctx)) as Promise<T>;
}

const SWITCH_AT = APP.indexOf('  const switchSession = async');

describe('a takeover outside a connect phase releases the remote navigation', () => {
    it('discards the old listing and frees its latch, spinner and decrypting state', async () => {
        const ctx: Record<string, unknown> = {
            useLatestCallback: (fn: unknown) => fn,
            remoteNavCounter: { current: 7 },
            remoteNavInFlightRef: { current: 7 },
        };
        const release = await run<() => void>(
            slice('  const releaseRemoteNavigation = useLatestCallback(', '  /** Puts the active session tab aside'),
            ctx, 'return releaseRemoteNavigation;');
        release();
        // A listing that started under generation 7 is dropped when it lands.
        expect(ctx.remoteNavCounter).toEqual({ current: 8 });
        expect(ctx.remoteNavInFlightRef).toEqual({ current: 0 });
        expect(ctx.setRemoteListLoading).toHaveBeenCalledWith(false);
        expect(ctx.setRemoteListReason).toHaveBeenCalledWith(null);
        expect(ctx.setOverlayDecrypting).toHaveBeenCalledWith(false);
    });

    it('releases before it parks', async () => {
        const order: string[] = [];
        const ctx: Record<string, unknown> = {
            useLatestCallback: (fn: unknown) => fn,
            releaseRemoteNavigation: () => order.push('release'),
            parkActiveSession: (keep: boolean) => order.push(`park:${keep}`),
        };
        const takeOver = await run<(keep?: boolean) => void>(
            slice('  const takeOverActiveSession = useLatestCallback(', '  /** Shared open path'),
            ctx, 'return takeOverActiveSession;');
        takeOver(true);
        takeOver();
        expect(order).toEqual(['release', 'park:true', 'release', 'park:false']);
    });

    it('takes over through it for the "+" new tab, keeping a device tab', async () => {
        const ctx: Record<string, unknown> = { takeOverActiveSession: vi.fn(), parkActiveSession: vi.fn() };
        const newTab = await run<() => void>(
            slice('  const handleNewTabFromSavedServer = () => {', '\n  };') + '\n  };',
            ctx, 'return handleNewTabFromSavedServer;');
        newTab();
        expect(ctx.takeOverActiveSession).toHaveBeenCalledWith(true);
        expect(ctx.parkActiveSession).not.toHaveBeenCalled();
        expect(ctx.setShowConnectionScreen).toHaveBeenCalledWith(true);
    });

    it('takes over through it when a portable device opens, before any await', () => {
        const open = slice('  const openMtpSessionFromLiveDevice = useCallback(', '  /** PLACES portable row');
        const takeOver = open.indexOf('takeOverActiveSession();');
        expect(takeOver).toBeGreaterThan(-1);
        expect(takeOver).toBeLessThan(open.indexOf('await '));
        expect(open).not.toMatch(/\bparkActiveSession\(/);
        expect(open).toMatch(/\}, \[[^\]]*\btakeOverActiveSession\b[^\]]*\]\);/);
    });

    it('takes over through it for the OAuth and 4shared handshake of My Servers', () => {
        expect(APP).toContain('onParkActiveSession={takeOverActiveSession}');
    });

    it('leaves the connect phases on their own release', () => {
        // A connect phase released the navigation when it started. A second
        // release after its host-key await would move the generation past a
        // Cancel already pressed, and the abort that Cancel owes would no
        // longer run.
        const phase = slice('  const asConnectPhase = ', '  const withRemoteListSpinner =');
        expect(phase.indexOf('releaseRemoteNavigation();')).toBeGreaterThan(-1);
        expect(phase.indexOf('releaseRemoteNavigation();')).toBeLessThan(phase.indexOf('    try {'));
        const connect = slice('  const connectToFtpImpl = async (', '  const connectToFtp = useLatestCallback(');
        expect(connect).not.toContain('takeOverActiveSession(');
        expect(connect).toContain('parkActiveSession();');
    });
});

describe('a tab switch from a live tab that is cancelled or fails leaves nothing active', () => {
    const LIVE_TAB = 'a';
    const TARGET = 'b';
    const CANCELLED = new Error('CONNECT_CANCELLED');

    const failSwitch = async (opts: { scopeCancelled?: boolean; superseded?: boolean; listingCancelled?: boolean; connectCancelled?: boolean }) => {
        const helpers = slice('    const fromParked = activeSessionId === null;', '\n    };', SWITCH_AT) + '\n    };\n';
        const handlers = slice('try { connectScope.assert(); } catch (cancelled) {', '      tryShowTwoFactorPrompt(', SWITCH_AT);
        const ctx: Record<string, unknown> = {
            activeSessionId: LIVE_TAB,
            sessionId: TARGET,
            targetSession: { id: TARGET, localPath: '/home/user/b', serverName: 'b' },
            phaseVersion: 5,
            remoteConnectPhaseVersionRef: { current: opts.superseded ? 6 : 5 },
            connectScope: { assert: () => { if (opts.scopeCancelled) throw CANCELLED; } },
            isListingCancelled: () => !!opts.listingCancelled,
            isConnectCancelledError: () => !!opts.connectCancelled,
            activityLog: { updateEntry: vi.fn() },
            reconnectLogId: 'log',
            logger: { error: vi.fn() },
            t: (key: string) => key,
        };
        const outcome = await run<string>(
            `${helpers}try { throw new Error('switch failed'); } catch (e) { ${handlers} }`,
            ctx, "return 'returned';").catch((error: unknown) => error);
        const setSessions = ctx.setSessions as Spy;
        const tabs = setSessions.mock.calls.reduce(
            (prev, [update]) => (update as (p: typeof prev) => typeof prev)(prev),
            [{ id: LIVE_TAB, status: 'cached' }, { id: TARGET, status: 'connecting' }]);
        return { ctx: ctx as Record<string, Spy>, outcome, tabs: tabs.map(s => `${s.id}:${s.status}`) };
    };

    it('on the card Cancel or the controller Stop, while the switch owns the phase', async () => {
        const { ctx, outcome, tabs } = await failSwitch({ scopeCancelled: true });
        expect(outcome).toBe(CANCELLED);
        expect(tabs).toEqual(['a:cached', 'b:cached']);
        expect(ctx.setActiveSessionId).toHaveBeenCalledWith(null);
        expect(ctx.setIsConnected).toHaveBeenCalledWith(false);
        expect(ctx.setCurrentLocalPath).toHaveBeenCalledWith('/home/user/b');
    });

    it('but touches nothing once a newer connect owns the phase', async () => {
        const { ctx, outcome, tabs } = await failSwitch({ scopeCancelled: true, superseded: true });
        expect(outcome).toBe(CANCELLED);
        expect(tabs).toEqual(['a:cached', 'b:connecting']);
        expect(ctx.setActiveSessionId).not.toHaveBeenCalled();
        expect(ctx.setIsConnected).not.toHaveBeenCalled();
    });

    it.each([
        ['the listing Cancel', { listingCancelled: true }],
        ['a cancelled reconnect', { connectCancelled: true }],
        ['a failed reconnect', {}],
    ])('on %s', async (_name, opts) => {
        const { ctx, outcome, tabs } = await failSwitch(opts);
        // Handled here, not rethrown.
        expect(outcome).not.toBeInstanceOf(Error);
        expect(tabs).toEqual(['a:cached', 'b:cached']);
        expect(ctx.setActiveSessionId).toHaveBeenCalledWith(null);
        expect(ctx.setIsConnected).toHaveBeenCalledWith(false);
    });

    it('keeps the live tab as it was when the host key is refused', () => {
        // Asked before anything of the current tab changes, so a refusal from
        // a live tab leaves it active and connected.
        const body = slice('  const switchSession = async', '\n  const ', SWITCH_AT);
        expect(body).toContain('if (!accepted) { if (fromParked) leaveNothingActive(); return; }');
        expect(body.indexOf('if (!accepted)')).toBeLessThan(body.indexOf('// Capture current state values'));
    });
});

describe('the tab a switch leaves', () => {
    it('reads as cached, since the backend goes to the target', async () => {
        const ctx: Record<string, unknown> = {
            activeSessionId: 'a',
            remoteFiles: [{ name: 'r' }], localFiles: [{ name: 'l' }],
            currentRemotePath: '/remote', currentLocalPath: '/home/user',
            isSyncNavigation: false, syncBasePaths: null, aeroVaultOverlaySession: null,
        };
        await run(slice('    // Capture current state values before any async operations', '    // Set active session immediately', SWITCH_AT), ctx, '');
        const [update] = (ctx.setSessions as Spy).mock.calls[0] as [(prev: { id: string; status: string }[]) => { id: string; status: string; remotePath?: string }[]];
        const [left, other] = update([{ id: 'a', status: 'connected' }, { id: 'c', status: 'cached' }]);
        expect(left).toMatchObject({ id: 'a', status: 'cached', remotePath: '/remote', localPath: '/home/user' });
        expect(other).toEqual({ id: 'c', status: 'cached' });
        // A tab already off the backend keeps its own status.
        const [disconnected] = update([{ id: 'a', status: 'disconnected' }]);
        expect(disconnected.status).toBe('disconnected');
    });
});
