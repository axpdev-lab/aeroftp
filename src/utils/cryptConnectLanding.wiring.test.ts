// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Connect landing for a crypt overlay. A blank Remote Path and a Remote Path
// of "/" are the account root: the connect opens on the Overlays Path, and the
// plaintext root stays reachable by walking up. A Remote Path that names a
// folder stays that folder. smartReanchorOverlay still maps an empty landing
// to '/'. Both halves are exercised by slicing the production blocks out of
// App.tsx and running them.

import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import app from '../App.tsx?raw';
import { normCryptScope } from './cryptScope';

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

describe('crypt overlay connect landing', () => {
    it('empty Remote Path with a non-empty scope opens the Overlays Path', async () => {
        const opts = await landingFor({ id: 's1', cryptOverlay: { remoteScope: '/Vault' }, serverInitialPath: undefined });
        expect(opts).toEqual({ initialLandingPath: '/Vault' });
    });

    it('blank Remote Path opens the Overlays Path', async () => {
        const opts = await landingFor({ id: 's1', cryptOverlay: { remoteScope: '/Vault' }, serverInitialPath: '   ' });
        expect(opts).toEqual({ initialLandingPath: '/Vault' });
    });

    it('a Remote Path of "/" opens the Overlays Path', async () => {
        const opts = await landingFor({ id: 's1', cryptOverlay: { remoteScope: '/Vault' }, serverInitialPath: '/' });
        expect(opts).toEqual({ initialLandingPath: '/Vault' });
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
