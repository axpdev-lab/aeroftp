// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import app from '../App.tsx?raw';

async function run<T>(source: string, context: Record<string, unknown>, suffix = ''): Promise<T> {
    const { code } = await transformWithOxc(source, 'remote-navigation-takeover.ts');
    return new Function(...Object.keys(context), `return (async () => { ${code}\n${suffix} })();`)(...Object.values(context));
}

function between(start: string, end: string, from = 0): string {
    const a = app.indexOf(start, from);
    const b = app.indexOf(end, a);
    if (a < 0 || b < 0) throw new Error(`Production navigation block missing: ${start}`);
    return app.slice(a, b);
}

function context() {
    const state = { loading: true, reason: 'old /Crypt listing' as string | null };
    const ctx = {
        remoteNavCounter: { current: 7 }, remoteNavInFlightRef: { current: 7 },
        remoteConnectPhaseVersionRef: { current: 0 }, remoteConnectPhaseRef: { current: false as false | string },
        setRemoteListLoading: vi.fn((value: boolean) => { state.loading = value; }),
        setRemoteListReason: vi.fn((value: string | null) => { state.reason = value; }),
        setActiveSessionId: vi.fn(), setShowRemotePanel: vi.fn(), sessionId: 'new-tab',
        ConnectScope: class {}, cancelRemoteNavigation: vi.fn(),
    };
    return { state, ctx };
}

async function connect(ctx: ReturnType<typeof context>['ctx']): Promise<void> {
    const source = between('const asConnectPhase =', 'const withRemoteListSpinner =');
    await expect(run(source, ctx, 'return asConnectPhase(async () => { throw new Error("missing credentials before listing"); })();')).rejects.toThrow('missing credentials before listing');
}

describe('remote navigation takeover', () => {
    it.each(['switch', 'connect'] as const)('%s clears the old spinner even if no new listing runs', async (kind) => {
        const { state, ctx } = context();
        if (kind === 'connect') await connect(ctx);
        else await run(between('// Set active session immediately', 'quotaVersionRef.current++;', app.indexOf('const switchSession = async')), ctx);
        expect(ctx.remoteNavCounter.current).toBe(8);
        expect(ctx.remoteNavInFlightRef.current).toBe(0);
        expect(state.loading).toBe(false);
        expect(state.reason).toBeNull();
    });

    it.each([false, true])('a new connection discards the pending scope-entry result (%s)', async (activated) => {
        const { ctx } = context();
        let finish!: (value: boolean) => void;
        const unlock = new Promise<boolean>((resolve) => { finish = resolve; });
        const setOverlayDecrypting = vi.fn();
        const setLockedOverlayProfile = vi.fn();
        const source = between('const reactivated = await maybeAutoUnlockProfileOverlay(activeSession.savedServerId,', 'reactivatedOverlayForNav = true;') + 'reactivatedOverlayForNav = true;';
        const pending = run('let reactivatedOverlayForNav = false; let reactivatedOverlayGeneration;\n' + source, {
            ...ctx, navId: 7, activeSession: { id: 'old-tab', savedServerId: 'old-profile', cryptOverlayKind: 'aerocrypt' },
            maybeAutoUnlockProfileOverlay: () => unlock,
            setOverlayDecrypting, setPendingOverlayUnlock: vi.fn(), setLockedOverlayProfile,
            setAeroCryptVaultId: vi.fn(), setRcloneCryptVaultId: vi.fn(), setCryptOverlayOwner: vi.fn(),
            lockSessionCryptOverlay: vi.fn(), overlayReloadedVaultRef: { current: null },
            overlayRefusalRef: { current: null }, overlayBinding: null, refusalFor: () => null,
        }, "return 'completed';").catch(() => 'failed');
        await connect(ctx);
        finish(activated);
        expect(await pending).toBeNull();
        expect(setOverlayDecrypting).not.toHaveBeenCalled();
        expect(setLockedOverlayProfile).not.toHaveBeenCalled();
    });
});
