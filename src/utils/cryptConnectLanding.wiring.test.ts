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
            navId: 7, remoteNavCounter: { current: 7 },
            overlayBinding: { vaultId: 'live_vault', prefix: 'aerocrypt_provider' },
            maybeAutoUnlockProfileOverlay: vi.fn().mockResolvedValue(reactivated),
            setOverlayDecrypting: vi.fn(),
            setPendingOverlayUnlock: vi.fn(),
            setLockedOverlayProfile,
            setAeroCryptVaultId: vi.fn(), setRcloneCryptVaultId: vi.fn(), setCryptOverlayOwner: vi.fn(),
            overlayReloadedVaultRef: { current: null }, lockSessionCryptOverlay: vi.fn(),
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


    it.each(['aerocrypt', 'rclone-crypt'])('clears stale %s state so the real banner gate opens after a failed return', async (kind) => {
        const start = app.indexOf('const reactivated = await maybeAutoUnlockProfileOverlay(activeSession.savedServerId);');
        const end = app.indexOf('reactivatedOverlayForNav = true;', start);
        const lockStart = app.indexOf('const lockSessionCryptOverlay = (');
        const lockEnd = app.indexOf('// Tab is no longer an overlay tab', lockStart);
        const gate = app.match(/\{(lockedOverlayProfile && isConnected && [^\n]+) && \(/)?.[1];
        if (start < 0 || end < 0 || lockStart < 0 || lockEnd < 0 || !gate) throw new Error('recovery or banner block missing');
        const activeSession = { id: 'tab_1', savedServerId: 'srv_1', cryptOverlayKind: kind, cryptOverlayScope: '/Crypt', cryptOverlay: { vaultId: 'stale', kind } };
        let sessions = [activeSession, { ...activeSession, id: 'tab_other', savedServerId: 'srv_other' }];
        let aeroCryptVaultId: string | null = kind === 'aerocrypt' ? 'stale' : null;
        let rcloneCryptVaultId: string | null = kind === 'rclone-crypt' ? 'stale' : null;
        let lockedOverlayProfile: unknown = null;
        let owner: unknown = { savedServerId: 'srv_1' };
        const overlayReloadedVaultRef = { current: 'stale' as string | null };
        const lockSessionCryptOverlay = await execute<(match: { sessionId: string }) => void>(app.slice(lockStart, lockEnd), {
            setSessions: (update: (prev: typeof sessions) => typeof sessions) => { sessions = update(sessions); },
            sessionMatches: (session: typeof activeSession, match: { sessionId: string }) => session.id === match.sessionId,
        }, 'return lockSessionCryptOverlay;');
        await expect(execute(app.slice(start, end), {
            activeSession, overlayBinding: null, navId: 7, remoteNavCounter: { current: 7 },
            maybeAutoUnlockProfileOverlay: async () => null,
            setOverlayDecrypting: vi.fn(), setPendingOverlayUnlock: vi.fn(),
            setAeroCryptVaultId: (id: string | null) => { aeroCryptVaultId = id; },
            setRcloneCryptVaultId: (id: string | null) => { rcloneCryptVaultId = id; },
            setCryptOverlayOwner: (value: unknown) => { owner = value; },
            overlayReloadedVaultRef, lockSessionCryptOverlay,
            setLockedOverlayProfile: (value: unknown) => { lockedOverlayProfile = value; },
            overlayRefusalRef: { current: null }, refusalFor,
        }, '')).rejects.toThrow('Failed to re-apply crypt overlay');
        expect(await execute('', { lockedOverlayProfile, isConnected: true, aeroCryptVaultId, rcloneCryptVaultId, overlayDecrypting: false }, `return !!(${gate});`)).toBe(true);
        expect(sessions[0]).toMatchObject({ cryptOverlay: null, cryptOverlayKind: kind, cryptOverlayScope: '/Crypt' });
        expect(sessions[1].cryptOverlay).toEqual(activeSession.cryptOverlay);
        expect(owner).toBeNull();
        expect(overlayReloadedVaultRef.current).toBeNull();
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


describe('OAuth landing probe recovers the current provider directory', () => {
    for (const saved of [false, true]) {
        async function probe(outcome: 'missing' | 'cancel' | 'fallback-cancel' | 'fallback-error' | 'scope-stale' | 'success') {
            const variable = saved ? 'savedOauthResp' : 'oauthResponse';
            const start = app.indexOf(`let ${variable}: FileListResponse | null = null;`);
            const end = app.indexOf(saved ? 'const savedOverlayAnchor =' : 'const overlayAnchor =', start);
            if (start < 0 || end < 0) throw new Error('OAuth landing probe block missing');
            const source = app.slice(start, end) + '}';
            const current = { current_path: '/home', files: [{ name: 'existing.txt' }] };
            const invoke = vi.fn(async (command: string) => {
                if (command === 'provider_change_dir') {
                    if (outcome === 'success') return { ...current, current_path: '/Landing' };
                    throw new Error(outcome === 'cancel' ? 'LISTING_CANCELLED' : 'directory missing');
                }
                if (outcome === 'fallback-cancel') throw new Error('LISTING_CANCELLED');
                if (outcome === 'fallback-error') throw new Error('listing unavailable');
                return current;
            });
            const assert = vi.fn(() => { if (outcome === 'scope-stale') throw new Error('connect attempt stale'); });
            const run = execute(source, {
                overlayHint: { anchor: '/Crypt', opensOutside: true }, overlaySavedId: 'srv_1',
                savedOverlayHint: { anchor: '/Crypt', opensOutside: true }, savedOverlaySavedId: 'srv_1',
                oauthInitialPath: '/Landing', savedOauthInitialPath: '/Landing',
                invoke, connectLandsOutsideOverlay, withRemoteListSpinner: (op: () => Promise<unknown>) => op(),
                isListingCancelled: (error: unknown) => String(error).includes('LISTING_CANCELLED'),
                connectScope: { step: (op: () => Promise<unknown>) => op(), assert },
            }, `return ${variable};`);
            return { run, invoke, current };
        }

        it.each([
            ['/Crypt/missing', '/home', true],
            ['/Crypt/missing', '/', true],
            ['/outside/missing', '/Crypt/exists', false],
            ['/Crypt/missing', '/Crypt/exists', false],
        ] as const)(`${saved ? 'saved' : 'Quick Connect'} classifies and stores fallback from %s to %s`, async (landing, fallback, outside) => {
            const variable = saved ? 'savedOauthResp' : 'oauthResponse';
            const start = app.indexOf(`let ${variable}: FileListResponse | null = null;`);
            const end = app.indexOf(saved ? 'const savedOverlayAnchor =' : 'const overlayAnchor =', start);
            const sessionStart = app.indexOf(saved ? 'await connectScope.step(() => createSession(' : 'await createSession(', start);
            const sessionEnd = app.indexOf(saved ? '));' : ');', sessionStart) + (saved ? 3 : 2);
            if (start < 0 || end < 0 || sessionStart < 0 || sessionEnd <= sessionStart) throw new Error('OAuth probe/session block missing');
            const current = { current_path: fallback, files: [{ name: 'actual-file' }] };
            const createSession = vi.fn();
            const invoke = vi.fn(async (command: string) => {
                if (command === 'provider_change_dir') throw new Error('stale Remote Path');
                return current;
            });
            const opensOutside = connectLandsOutsideOverlay(landing, '/Crypt');
            const hint = await execute<{ opensOutside: boolean }>(app.slice(start, end) + '}\n' + app.slice(sessionStart, sessionEnd), {
                oauthInitialPath: landing, savedOauthInitialPath: landing,
                overlayHint: { anchor: '/Crypt', opensOutside }, overlaySavedId: 'srv_1',
                savedOverlayHint: { anchor: '/Crypt', opensOutside }, savedOverlaySavedId: 'srv_1',
                connectLandsOutsideOverlay, invoke, createSession,
                isListingCancelled: () => false,
                withRemoteListSpinner: (op: () => Promise<unknown>) => op(),
                connectScope: { step: (op: () => Promise<unknown>) => op(), assert: vi.fn() },
                oauthTabName: 'Profile', providerName: 'Profile', effectiveParams: {}, normalizedParams: {},
                oauthLocalDir: '/local', currentLocalPath: '/local', resolvedLocalPath: '/local',
            }, `return ${saved ? 'savedOverlayHint' : 'overlayHint'};`);
            expect(hint.opensOutside).toBe(outside);
            expect(createSession).toHaveBeenCalledWith('Profile', {}, fallback, '/local', outside ? current.files : [], undefined, fallback);
        });

        it(`${saved ? 'saved' : 'Quick Connect'} falls back to a usable current listing after landing failure`, async () => {
            const { run, invoke, current } = await probe('missing');
            await expect(run).resolves.toEqual(current);
            expect(invoke).toHaveBeenNthCalledWith(2, 'provider_list_files', { path: null });
        });
        it.each(['cancel', 'fallback-cancel'] as const)(`${saved ? 'saved' : 'Quick Connect'} stops on %s`, async (outcome) => {
            const { run, invoke } = await probe(outcome);
            await expect(run).resolves.toBe(saved ? 'failed' : undefined);
            expect(invoke).toHaveBeenCalledTimes(outcome === 'cancel' ? 1 : 2);
        });
        it(`${saved ? 'saved' : 'Quick Connect'} preserves a transient fallback-listing error`, async () => {
            const { run } = await probe('fallback-error');
            await expect(run).resolves.toBeNull();
        });
        it(`${saved ? 'saved' : 'Quick Connect'} does not fall back when landing succeeds`, async () => {
            const { run, invoke } = await probe('success');
            await expect(run).resolves.toMatchObject({ current_path: '/Landing' });
            expect(invoke).toHaveBeenCalledTimes(1);
        });
        if (saved) {
            it('never falls back after the saved connect scope becomes stale', async () => {
                const { run, invoke } = await probe('scope-stale');
                await expect(run).rejects.toThrow('connect attempt stale');
                expect(invoke).toHaveBeenCalledTimes(1);
            });
        }
    }
});


describe('scope-entry unlock result belongs to its navigation', () => {
    it.each([false, true])('discards an old unlock result (%s) after switching sessions', async (activated) => {
        const start = app.indexOf('const reactivated = await maybeAutoUnlockProfileOverlay(activeSession.savedServerId);');
        const marker = 'reactivatedOverlayForNav = true;';
        const end = app.indexOf(marker, start) + marker.length;
        const switchStart = app.indexOf('// Set active session immediately', app.indexOf('const switchSession = async'));
        const switchEnd = app.indexOf('quotaVersionRef.current++;', switchStart);
        if (start < 0 || end <= start || switchStart < 0 || switchEnd < 0) throw new Error('navigation ownership blocks missing');
        const remoteNavCounter = { current: 7 };
        const remoteNavInFlightRef = { current: 7 };
        const setters = {
            setOverlayDecrypting: vi.fn(), setPendingOverlayUnlock: vi.fn(),
            setAeroCryptVaultId: vi.fn(), setRcloneCryptVaultId: vi.fn(),
            setCryptOverlayOwner: vi.fn(), lockSessionCryptOverlay: vi.fn(), setLockedOverlayProfile: vi.fn(),
        };
        let finishUnlock!: (value: boolean) => void;
        const unlock = new Promise<boolean>((resolve) => { finishUnlock = resolve; });
        const pending = execute('let reactivatedOverlayForNav = false;\n' + app.slice(start, end), {
            activeSession: { id: 'old_tab', savedServerId: 'old_profile', cryptOverlayKind: 'aerocrypt' },
            overlayBinding: null, navId: 7, remoteNavCounter,
            maybeAutoUnlockProfileOverlay: () => unlock,
            overlayReloadedVaultRef: { current: 'new_tab_vault' },
            overlayRefusalRef: { current: null }, refusalFor, ...setters,
        }, "return 'completed';").catch(() => 'failed');
        await execute(app.slice(switchStart, switchEnd), {
            remoteNavCounter, remoteNavInFlightRef, sessionId: 'new_tab',
            setActiveSessionId: vi.fn(), setShowRemotePanel: vi.fn(),
        }, '');
        finishUnlock(activated);
        expect(await pending).toBeNull();
        expect(remoteNavCounter.current).toBe(8);
        expect(remoteNavInFlightRef.current).toBe(0);
        for (const setter of Object.values(setters)) expect(setter).not.toHaveBeenCalled();
    });
});
