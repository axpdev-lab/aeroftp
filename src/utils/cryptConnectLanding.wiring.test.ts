// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Regression wiring for issue #1211: a profile with a crypt overlay and an
// EMPTY Remote Path must land on the on-wire root at connect, not inside the
// Overlays Path. The phase-2 overlay effect in App.tsx computes the landing
// from the session's serverInitialPath, and smartReanchorOverlay maps an empty
// landing to '/'. Both halves of that contract are exercised here by slicing
// the production blocks out of App.tsx and running them.

import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import app from '../App.tsx?raw';
import { isWithinCryptScope, normCryptScope } from './cryptScope';

async function execute<T>(source: string, context: Record<string, unknown>, suffix: string): Promise<T> {
    const { code } = await transformWithOxc(source, 'crypt-connect-landing.ts');
    return new Function(...Object.keys(context), `return (async () => { ${code}\n${suffix} })();`)(...Object.values(context));
}

// The landing-decision block of the phase-2 overlay effect.
const LANDING_START = 'const activeSess = sessions.find((s) => s.id === activeSessionId);';
const LANDING_END = 'void (async () => {';

function landingBlock(): string {
    const start = app.indexOf(LANDING_START);
    const end = app.indexOf(LANDING_END, start);
    if (start < 0 || end < 0) throw new Error('phase-2 landing block not found in App.tsx');
    return app.slice(start, end);
}

interface FakeSession {
    id: string;
    cryptOverlay?: { remoteScope?: string } | null;
    serverInitialPath?: string;
}

async function landingFor(session: FakeSession): Promise<{ initialLandingPath?: string | null } | undefined> {
    return execute(landingBlock(), {
        sessions: [session],
        activeSessionId: session.id,
        normCryptScope,
    }, 'return initialLandingOpts;');
}

// The initial-landing branch of smartReanchorOverlay (runs before the
// cwd-in-view legacy re-anchor; an empty landing is the on-wire root).
const REANCHOR_START = 'if (opts && opts.initialLandingPath !== undefined) {';
const REANCHOR_END = 'const cwdInView = await';

async function reanchorWith(opts: { initialLandingPath?: string | null } | undefined) {
    const start = app.indexOf(REANCHOR_START);
    const end = app.indexOf(REANCHOR_END, start);
    if (start < 0 || end < 0) throw new Error('smartReanchorOverlay initial-landing branch not found in App.tsx');
    const invoke = vi.fn().mockResolvedValue(undefined);
    const positionWrappedProviderAtScope = vi.fn().mockResolvedValue(undefined);
    await execute(app.slice(start, end), {
        opts,
        invoke,
        normCryptScope,
        positionWrappedProviderAtScope,
    }, '');
    return { invoke, positionWrappedProviderAtScope };
}

describe('crypt overlay connect landing (#1211)', () => {
    it('empty Remote Path with a non-empty scope lands on the on-wire root', async () => {
        const opts = await landingFor({ id: 's1', cryptOverlay: { remoteScope: '/Vault' }, serverInitialPath: undefined });
        expect(opts).toEqual({ initialLandingPath: '' });
    });

    it('blank Remote Path behaves like an empty one', async () => {
        const opts = await landingFor({ id: 's1', cryptOverlay: { remoteScope: '/Vault' }, serverInitialPath: '   ' });
        expect(opts).toEqual({ initialLandingPath: '' });
    });

    it('a configured Remote Path remains the landing', async () => {
        const opts = await landingFor({ id: 's1', cryptOverlay: { remoteScope: '/Vault' }, serverInitialPath: '/docs' });
        expect(opts).toEqual({ initialLandingPath: '/docs' });
    });

    it('a whole-remote overlay (empty scope) keeps the legacy re-anchor', async () => {
        const opts = await landingFor({ id: 's1', cryptOverlay: { remoteScope: '' }, serverInitialPath: '' });
        expect(opts).toBeUndefined();
    });

    it('smartReanchorOverlay maps an empty landing to provider_change_dir("/")', async () => {
        const { invoke, positionWrappedProviderAtScope } = await reanchorWith({ initialLandingPath: '' });
        expect(invoke).toHaveBeenCalledWith('provider_change_dir', { path: '/' });
        expect(positionWrappedProviderAtScope).not.toHaveBeenCalled();
    });

    it('smartReanchorOverlay moves to a non-empty landing path', async () => {
        const { invoke, positionWrappedProviderAtScope } = await reanchorWith({ initialLandingPath: '/docs' });
        expect(positionWrappedProviderAtScope).toHaveBeenCalledWith('/docs');
        expect(invoke).not.toHaveBeenCalled();
    });
});

describe('crypt overlay deferred connect unlock', () => {
    const DECISION_START = 'const overlayBinding = isProvider ? rawActiveCryptOverlay() : null;';
    const DECISION_END = 'let reactivatedOverlayForNav = false;';

    async function navigationDecisionFor(activeSession: { savedServerId?: string; cryptOverlayKind?: string | null }) {
        const start = app.indexOf(DECISION_START);
        const end = app.indexOf(DECISION_END, start);
        if (start < 0 || end < 0) throw new Error('overlay navigation decision block not found in App.tsx');
        return execute(app.slice(start, end), {
            isProvider: true,
            rawActiveCryptOverlay: () => null,
            activeSession,
            resolveTargetDisplayPath: (_current: string, path: string) => path,
            currentRemoteDisplayPath: '/',
            path: '/Crypt',
            plainPath: true,
            activeBoundRemoteScope: '/Crypt',
            isWithinCryptScope,
        }, 'return { crossingIn, crossingOut, providerNavPath };');
    }

    it('enters the scope through a saved capability before any vault exists', async () => {
        await expect(navigationDecisionFor({ savedServerId: 'srv_1', cryptOverlayKind: 'aerocrypt' })).resolves.toEqual({
            crossingIn: true,
            crossingOut: false,
            providerNavPath: '/Crypt',
        });
    });

    it('does not route a plain session through the overlay', async () => {
        await expect(navigationDecisionFor({ savedServerId: 'srv_1', cryptOverlayKind: null })).resolves.toEqual({
            crossingIn: false,
            crossingOut: false,
            providerNavPath: '/Crypt',
        });
    });

    it('gates every connect-time pending unlock on the landing policy', () => {
        expect(app.match(/shouldAutoUnlockOverlayOnConnect\(/g)).toHaveLength(4);
    });

    it('lists OAuth at the configured Remote Path on both connect paths', () => {
        expect(app).toContain("invoke<FileListResponse>('provider_change_dir', { path: oauthInitialPath })");
        expect(app).toContain("invoke<FileListResponse>('provider_change_dir', { path: savedOauthInitialPath })");
    });
});
