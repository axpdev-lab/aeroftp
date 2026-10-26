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
import { connectLandsOutsideOverlay, isWithinCryptScope, normCryptScope, resolveUsernameTemplate } from './cryptScope';
import { refusalFor } from './overlayRefusal';

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

describe('crypt overlay scope-entry recovery', () => {
    async function enterScope(kind: string | null, reactivated: boolean, refusalProfile = 'srv_1') {
        const start = app.indexOf('const reactivated = await maybeAutoUnlockProfileOverlay(activeSession.savedServerId);');
        const marker = 'reactivatedOverlayForNav = true;';
        const end = app.indexOf(marker, start);
        if (start < 0 || end < 0) throw new Error('scope-entry unlock block not found in App.tsx');
        const setLockedOverlayProfile = vi.fn();
        const run = execute('let reactivatedOverlayForNav = false;\n' + app.slice(start, end + marker.length), {
            activeSession: { savedServerId: 'srv_1', cryptOverlayKind: kind },
            overlayBinding: { vaultId: 'live_vault', prefix: 'aerocrypt_provider' },
            maybeAutoUnlockProfileOverlay: vi.fn().mockResolvedValue(reactivated),
            setOverlayDecrypting: vi.fn(),
            setPendingOverlayUnlock: vi.fn(),
            setLockedOverlayProfile,
            overlayRefusalRef: { current: { savedServerId: refusalProfile, kind: kind ?? 'aerocrypt', reason: 'Supply the missing salt.' } },
            refusalFor,
        }, '');
        // The successful branch assigns the surrounding navigation flag.
        return { run, setLockedOverlayProfile };
    }

    it.each(['aerocrypt', 'rclone-crypt', null])('offers manual recovery after a failed %s scope-entry unlock', async (kind) => {
        const { run, setLockedOverlayProfile } = await enterScope(kind, false);
        await expect(run).rejects.toThrow('Failed to re-apply crypt overlay for this scope.');
        expect(setLockedOverlayProfile).toHaveBeenCalledWith({
            savedServerId: 'srv_1', kind: kind ?? 'aerocrypt', reason: kind === 'rclone-crypt' ? 'Supply the missing salt.' : undefined,
        });
    });

    it('does not show a refusal belonging to another profile', async () => {
        const { run, setLockedOverlayProfile } = await enterScope('rclone-crypt', false, 'srv_other');
        await expect(run).rejects.toThrow('Failed to re-apply crypt overlay for this scope.');
        expect(setLockedOverlayProfile).toHaveBeenCalledWith({ savedServerId: 'srv_1', kind: 'rclone-crypt', reason: undefined });
    });

    it('keeps the recovery banner closed after a successful scope-entry unlock', async () => {
        const { run, setLockedOverlayProfile } = await enterScope('aerocrypt', true);
        await expect(run).resolves.toBeUndefined();
        expect(setLockedOverlayProfile).not.toHaveBeenCalled();
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

    it('classifies the Quick Connect landing instead of the saved profile default', async () => {
        const start = app.indexOf('const getProfileOverlayHint = async (');
        const end = app.indexOf('// Open and Create from', start);
        if (start < 0 || end < 0) throw new Error('profile overlay hint block not found in App.tsx');
        const hintFor = (landingPath?: string, profilePath = '/', overlayScope: string | null = '/Crypt') => execute(app.slice(start, end), {
            landingPath,
            loadSavedServerProfiles: async () => [{
                id: 'srv_1',
                initialPath: profilePath,
                username: 'alice',
                hasStoredAeroCryptPassword: true,
                aeroCryptOverlay: { enabled: true, kind: 'aerocrypt', remoteScope: overlayScope },
            }],
            resolveUsernameTemplate,
            connectLandsOutsideOverlay,
        }, "return getProfileOverlayHint('srv_1', landingPath);");

        await expect(hintFor()).resolves.toMatchObject({ opensOutside: true });
        await expect(hintFor('/Crypt')).resolves.toMatchObject({ opensOutside: false });
        await expect(hintFor('/', '/Crypt', null)).resolves.toMatchObject({ anchor: '/Crypt', opensOutside: true });
        expect(app).toContain('getProfileOverlayHint(overlaySavedId, oauthInitialPath)');
        expect(app).toContain('getProfileOverlayHint(overlaySavedId, resolvedRemoteDir)');
        expect(app).toContain('serverInitialPath: initialPathOverride ?? cachedInitialPath');
        expect(app).toContain('savedOverlayHint ? savedOauthInitialPath : undefined');
        expect(app).toContain("overlayHint ? resolvedSavedInitialPath || '/' : undefined");
    });
});


describe('new overlay connection replaces stale frontend vault mirrors', () => {
    const starts = [
        ['Quick Connect OAuth', "const overlayAnchor = overlayHint.anchor || oauthResponse?.current_path || '/';", 'overlaySavedId'],
        ['Quick Connect provider', 'const overlayHint = overlaySavedId ? await getProfileOverlayHint(overlaySavedId, resolvedRemoteDir) : null;', 'overlaySavedId'],
        ['saved OAuth', "const savedOverlayAnchor = savedOverlayHint.anchor || savedOauthResp?.current_path || '/';", 'savedOverlaySavedId'],
        ['saved provider', 'const overlayHint = await connectScope.step(() => getProfileOverlayHint(overlaySavedId));', 'overlaySavedId'],
    ] as const;

    for (const [index, [label, marker, ownerKey]] of starts.entries()) {
        for (const previousKind of ['aerocrypt', 'rclone-crypt']) {
            it(`${label} clears a previous ${previousKind} mirror before arming the new owner`, async () => {
                const first = app.indexOf(marker);
                const start = index === 1 || index === 3
                    ? app.indexOf('if (overlayHint && overlaySavedId) {', first)
                    : first;
                const end = app.indexOf('markSessionOverlayKind(', start);
                if (start < 0 || end < 0) throw new Error(`${label} activation block missing`);
                // Drop the opening conditional so this runs the actual activation
                // statements without needing the remainder of the connect handler.
                const source = app.slice(start, end).replace(/^if \(overlayHint && overlaySavedId\) \{/, '');
                const vaultId = `provider-overlay:${previousKind}:new-profile`;
                const overlayReloadedVaultRef = { current: vaultId as string | null };
                let aeroId: string | null = previousKind === 'aerocrypt' ? 'previous-aero-vault' : null;
                let rcloneId: string | null = previousKind === 'rclone-crypt' ? 'previous-rclone-vault' : null;
                const setCryptOverlayOwner = vi.fn((owner: unknown) => {
                    expect(aeroId).toBeNull();
                    expect(rcloneId).toBeNull();
                    expect(overlayReloadedVaultRef.current).toBeNull();
                    expect(owner).toEqual({ savedServerId: 'new-profile', sessionId: null });
                });
                await execute(source, {
                    [ownerKey]: 'new-profile',
                    overlayHint: { anchor: '/Crypt' },
                    savedOverlayHint: { anchor: '/Crypt' },
                    oauthResponse: null,
                    savedOauthResp: null,
                    setAeroCryptVaultId: (id: string | null) => { aeroId = id; },
                    setRcloneCryptVaultId: (id: string | null) => { rcloneId = id; },
                    setCryptOverlayOwner,
                    overlayReloadedVaultRef,
                }, '');
                expect(setCryptOverlayOwner).toHaveBeenCalledOnce();
                // Re-unlocking the same profile reuses its sentinel. Its first
                // phase-2 reload must run, while a StrictMode repeat stays gated.
                const guardStart = app.indexOf('const activeVault = aeroCryptVaultId || rcloneCryptVaultId;', app.indexOf('// P3.3 (phase 2)'));
                const guardEnd = app.indexOf('let cancelled = false;', guardStart);
                if (guardStart < 0 || guardEnd < 0) throw new Error('phase-2 reload guard missing');
                const guardContext = {
                    aeroCryptVaultId: previousKind === 'aerocrypt' ? vaultId : null,
                    rcloneCryptVaultId: previousKind === 'rclone-crypt' ? vaultId : null,
                    pendingOverlayUnlock: 'new-profile',
                    overlayReloadedVaultRef,
                };
                await expect(execute(app.slice(guardStart, guardEnd), guardContext, 'return true;')).resolves.toBe(true);
                await expect(execute(app.slice(guardStart, guardEnd), guardContext, 'return true;')).resolves.toBeUndefined();
            });
        }
    }
});


describe('outside-scope provider landing survives Refresh', () => {
    for (const saved of [false, true]) {
        for (const hint of [{ opensOutside: true }, { opensOutside: false }, null]) {
            it(`${saved ? 'saved' : 'Quick Connect'} ${hint === null ? 'plain' : hint.opensOutside ? 'outside-scope' : 'in-scope'} listing keeps its directory contract`, async () => {
                const marker = saved
                    ? "const response = await connectScope.step(() => withRemoteListSpinner(() => invoke<{ files: any[]; current_path: string }>"
                    : "logger.debug('[connectToFtp] Calling provider_list_files for:', protocol);";
                const start = app.indexOf(marker);
                const end = app.indexOf('const files = response.files.map', start);
                if (start < 0 || end < 0) throw new Error('provider landing listing block missing');
                // Filen list(path) can read a folder without moving its cwd.
                // Refresh uses path:null and therefore reads that stored cwd.
                let cwd = '/';
                const invoke = vi.fn(async (command: string, args: { path: string | null }) => {
                    if (command === 'provider_change_dir') cwd = args.path || '/';
                    return { current_path: args.path || cwd, files: [{ name: args.path || cwd }] };
                });
                const response = await execute<{ current_path: string; files: unknown[] }>(app.slice(start, end), {
                    overlayHint: hint,
                    resolvedRemoteDir: '/Parent',
                    resolvedSavedInitialPath: '/Parent',
                    connectScope: { step: (op: () => Promise<unknown>) => op() },
                    withRemoteListSpinner: (op: () => Promise<unknown>) => op(),
                    listingReason: () => undefined,
                    logger: { debug: vi.fn() },
                    protocol: 'filen',
                    invoke,
                }, 'return response;');
                expect(response.current_path).toBe('/Parent');
                if (hint?.opensOutside) {
                    expect(invoke).toHaveBeenCalledWith('provider_change_dir', { path: '/Parent' });
                    const refreshed = await invoke('provider_list_files', { path: null });
                    expect(refreshed).toEqual(response);
                } else {
                    expect(invoke).toHaveBeenCalledWith('provider_list_files', { path: '/Parent' });
                    expect(cwd).toBe('/');
                }
            });
        }
    }
});
