// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot } from 'react-dom/client';
import { afterEach, expect, it, vi } from 'vitest';
import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { isLoginRelativeScope, profileOverlayScope, restoredTransferCryptScope, restoredTransferRequiresOverlay } from './cryptScope';
import { useResumeProfileGuard } from '../hooks/useResumeProfileGuard';
import type { FtpSession } from '../types';

function block(start: string, end: string) {
    const a = app.indexOf(start);
    const b = app.indexOf(end, a);
    if (a < 0 || b < 0) throw new Error(`Missing resume block: ${start}`);
    return app.slice(a, b);
}

const connectCode = (await transformWithOxc(block('  const connectSavedProfileForResume =', '  // A resume on a session'), 'resume-connect.ts')).code;
type Session = Pick<FtpSession, 'id' | 'savedServerId' | 'status' | 'cryptOverlay'>;
const host = document.createElement('div');
const root = createRoot(host);
afterEach(async () => { await act(async () => root.render(null)); });

async function fixture() {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    let current = { isConnected: true, activeSessionId: 'A', sessions: [{ id: 'A', savedServerId: 'profile-A', status: 'connected' }] as Session[] };
    let guard!: ReturnType<typeof useResumeProfileGuard>;
    function Probe() {
        guard = useResumeProfileGuard(current.isConnected, current.sessions, current.activeSessionId);
        return null;
    }
    async function render(next: typeof current) {
        current = next;
        await act(async () => root.render(createElement(Probe)));
    }
    await render(current);
    const readFromOriginalRender = guard.isLiveOnProfile;
    const connectToFtp = vi.fn(async () => undefined);
    const switchSession = vi.fn(async () => undefined);
    const notify = { info: vi.fn(), error: vi.fn() };
    const context = {
        restoredTransferCryptScope, restoredTransferRequiresOverlay, restoredTransferLoginFolder: async () => null, sessions: current.sessions, isLiveOnProfile: readFromOriginalRender, waitForLiveProfile: guard.waitForLiveProfile, connectToFtp, switchSession,
        setQuickConnectDirs: vi.fn(), setShowConnectionScreen: vi.fn(), setShowRemotePanel: vi.fn(), setShowLocalPreview: vi.fn(), setIsConnected: vi.fn(),
        notify, t: (s: string) => s, isOAuthProvider: () => false, isFourSharedProvider: () => false,
        getCredentialWithRetry: async () => 'fixture', logger: { error: vi.fn() }, isConnectCancelledError: () => false,
    };
    const connect = new Function(...Object.keys(context), `${connectCode}; return connectSavedProfileForResume;`)(...Object.values(context));
    return { connect, connectToFtp, switchSession, notify, render, readFromOriginalRender, waitForLiveProfile: guard.waitForLiveProfile, current };
}

it('refuses resume when a swallowed connect failure leaves another profile active', async () => {
    const f = await fixture();
    await expect(f.connect({ id: 'profile-B', name: 'B', host: 'b.invalid', username: 'fixture', protocol: 'ftp' })).resolves.toBe(false);
    expect(f.connectToFtp).toHaveBeenCalledOnce();
    expect(f.readFromOriginalRender('profile-A')).toBe(true);
});

it('reads the newly committed session after connecting, not the original render', async () => {
    const f = await fixture();
    f.connectToFtp.mockImplementation(async () => {
        await f.render({ isConnected: true, activeSessionId: 'B', sessions: [{ id: 'B', savedServerId: 'profile-B', status: 'connected' }] });
    });
    await expect(f.connect({ id: 'profile-B', name: 'B', host: 'b.invalid', username: 'fixture', protocol: 'ftp' })).resolves.toBe(true);
    expect(f.readFromOriginalRender('profile-A')).toBe(false);
});

it('refuses a cached session even if its profile and active tab match', async () => {
    const f = await fixture();
    await f.render({ isConnected: true, activeSessionId: 'B', sessions: [{ id: 'B', savedServerId: 'profile-B', status: 'cached' }] });
    expect(f.readFromOriginalRender('profile-B')).toBe(false);
});

it('waits for the successful tab commit instead of refusing a still-pending React update', async () => {
    const f = await fixture();
    let settled = false;
    const pending = f.waitForLiveProfile('profile-B').then(live => { settled = true; return live; });
    await Promise.resolve();
    expect(settled).toBe(false);
    await f.render({ isConnected: true, activeSessionId: 'B', sessions: [{ id: 'B', savedServerId: 'profile-B', status: 'connected' }] });
    await expect(pending).resolves.toBe(true);
    expect(app).toContain('useResumeProfileGuard(isConnected, sessions, activeSessionId)');
});

it('refuses pending resumes when the owning app unmounts', async () => {
    const f = await fixture();
    const pending = f.waitForLiveProfile('profile-B');
    await act(async () => root.render(null));
    await expect(pending).resolves.toBe(false);
    expect(f.readFromOriginalRender('profile-A')).toBe(false);
});

it('keeps restored flags and callbacks untouched when the connect did not establish ownership', async () => {
    const source = block('      const ok = await connectSavedProfileForResume(profile, false);', '\n    }\n  };\n\n  // Two-level cancel');
    const code = (await transformWithOxc(`for (const profileId of ['profile-B']) { ${source} }`, 'resume-dispatch.ts')).code;
    const clearRestoredFlags = vi.fn();
    const fireRetryCallbacks = vi.fn();
    const ctx = { profile: { id: 'profile-B' }, groupIds: ['restored'], connectSavedProfileForResume: async () => true,
        isLiveOnProfile: () => false, resumeOwnedEntries: vi.fn(), transferQueue: { clearRestoredFlags }, fireRetryCallbacks };
    await new Function(...Object.keys(ctx), `return (async () => { ${code} })();`)(...Object.values(ctx));
    expect(clearRestoredFlags).not.toHaveBeenCalled();
    expect(fireRetryCallbacks).not.toHaveBeenCalled();
});

it('does not dispatch a queued retry after ownership changes before its microtask', async () => {
    const source = block('  const fireRetryCallbacks =', '  const { isLiveOnProfile, waitForLiveProfile } =');
    const code = (await transformWithOxc(source, 'resume-callback.ts')).code;
    const retry = vi.fn();
    let live = true;
    const context = { retryCallbacksRef: { current: new Map([['entry', retry]]) }, isLiveOnProfile: () => live };
    const fire = new Function(...Object.keys(context), `${code}; return fireRetryCallbacks;`)(...Object.values(context));
    fire(['entry'], 'profile-B');
    live = false;
    await Promise.resolve();
    expect(retry).not.toHaveBeenCalled();
});


it('waits for the overlay commit before resuming an enabled crypt profile', async () => {
    const f = await fixture();
    let connected!: () => void;
    const reached = new Promise<void>(resolve => { connected = resolve; });
    const session = { id: 'B', savedServerId: 'profile-B', status: 'connected' as const };
    f.connectToFtp.mockImplementation(async () => {
        await f.render({ isConnected: true, activeSessionId: 'B', sessions: [session] });
        connected();
    });
    let settled = false;
    const pending = f.connect({ id: 'profile-B', name: 'B', host: 'b.invalid', username: 'fixture', protocol: 'ftp',
        aeroCryptOverlay: { enabled: true, kind: 'aerocrypt' } }).then((ready: boolean) => { settled = true; return ready; });
    await reached;
    await Promise.resolve();
    expect(settled).toBe(false);
    await f.render({ isConnected: true, activeSessionId: 'B', sessions: [{ ...session,
        cryptOverlay: { vaultId: 'vault-B', kind: 'aerocrypt', remoteScope: '/vault' } }] });
    await expect(pending).resolves.toBe(true);
});

it('keeps an enabled overlay pending if activation never succeeds', async () => {
    const f = await fixture();
    await f.render({ isConnected: true, activeSessionId: 'B', sessions: [{ id: 'B', savedServerId: 'profile-B', status: 'connected' }] });
    vi.useFakeTimers();
    try {
        const pending = f.waitForLiveProfile('profile-B', true);
        await vi.advanceTimersByTimeAsync(30_000);
        await expect(pending).resolves.toBe(false);
        expect(f.readFromOriginalRender('profile-B', true)).toBe(false);
    } finally { vi.useRealTimers(); }
});

it('captures a backend generation only while the intended profile stays ready', async () => {
    const source = block('  const retryRestoredEntry =', '  // Queues the restorable entries');
    const code = (await transformWithOxc(source, 'restored-entry.ts')).code;
    const downloadFile = vi.fn(async () => true);
    const failTransfer = vi.fn();
    const completeTransfer = vi.fn();
    let live = true;
    const ctx = { useLatestCallback: (fn: unknown) => fn, transferQueue: { startTransfer: vi.fn(), failTransfer, completeTransfer },
        restoredTransferCryptScope, restoredTransferRequiresOverlay, restoredTransferLoginFolder: async () => null, loadSavedServerProfiles: async () => [{ id: 'profile-A' }], isLiveOnProfile: () => live,
        invoke: vi.fn(async () => { live = false; return 7; }), downloadFile, uploadFile: vi.fn(),
        parentDir: () => '/local', batchCancelledRef: { current: false }, notify: { error: vi.fn() }, t: (s: string) => s,
        isCancelledTransferError: () => false };
    const retry = new Function(...Object.keys(ctx), `${code}; return retryRestoredEntry;`)(...Object.values(ctx));
    await retry('entry', { direction: 'download', profile_id: 'profile-A', filename: 'f.txt', remote_path: '/f.txt', local_path: '/local/f.txt' });
    expect(downloadFile).not.toHaveBeenCalled();
    expect(completeTransfer).not.toHaveBeenCalled();
    expect(failTransfer).toHaveBeenCalledWith('entry', expect.stringContaining('CONNECTION_CHANGED'));
});

it('connects a plaintext resume on an encrypted profile without waiting for a vault', async () => {
    const f = await fixture();
    f.connectToFtp.mockImplementation(async () => {
        await f.render({ isConnected: true, activeSessionId: 'B', sessions: [{ id: 'B', savedServerId: 'profile-B', status: 'connected' }] });
    });
    await expect(f.connect({ id: 'profile-B', name: 'B', host: 'b.invalid', username: 'u', protocol: 'ftp',
        initialPath: '/public', aeroCryptOverlay: { enabled: true, remoteScope: '/vault' } }, false)).resolves.toBe(true);
});

it('resumes plaintext siblings immediately while encrypted entries wait for their own overlay', async () => {
    const source = block('      const resumeOwnedEntries =', '      if (isLiveOnProfile(profileId))');
    const code = (await transformWithOxc(source, 'resume-owned-entries.ts')).code;
    let unlock!: (ready: boolean) => void;
    const encrypted = new Promise<boolean>(resolve => { unlock = resolve; });
    const fireRetryCallbacks = vi.fn();
    const clearRestoredFlags = vi.fn();
    const ctx = { groupIds: ['crypt', 'plain'], profileId: 'A', profile: { name: 'A', username: 'u', aeroCryptOverlay: { enabled: true, remoteScope: '/vault' } },
        journalDescriptorsRef: { current: new Map([['crypt', { remote_path: '/vault/opaque' }], ['plain', { remote_path: '/public/f.txt' }]]) },
        restoredTransferCryptScope, restoredTransferRequiresOverlay, restoredTransferLoginFolder: async () => null, waitForLiveProfile: (_id: string, crypt: boolean) => crypt ? encrypted : Promise.resolve(true),
        transferQueue: { clearRestoredFlags }, logResume: vi.fn(), fireRetryCallbacks };
    const resume = new Function(...Object.keys(ctx), `${code}; return resumeOwnedEntries;`)(...Object.values(ctx));
    const pending = resume();
    await Promise.resolve();
    expect(fireRetryCallbacks.mock.calls).toEqual([[['plain'], 'A', false]]);
    expect(clearRestoredFlags).toHaveBeenCalledWith(['plain']);
    unlock(true);
    await pending;
    expect(fireRetryCallbacks).toHaveBeenLastCalledWith(['crypt'], 'A', true);
});

it('retries an out-of-scope entry with backend profile identity but without requiring crypt', async () => {
    const code = (await transformWithOxc(block('  const retryRestoredEntry =', '  // Queues the restorable entries'), 'retry-plaintext.ts')).code;
    const downloadFile = vi.fn(async () => true);
    const ctx = { useLatestCallback: (fn: unknown) => fn, transferQueue: { startTransfer: vi.fn(), failTransfer: vi.fn(), completeTransfer: vi.fn() },
        loadSavedServerProfiles: async () => [{ id: 'A', username: 'u', aeroCryptOverlay: { enabled: true, remoteScope: '/vault' } }],
        restoredTransferCryptScope, restoredTransferRequiresOverlay, restoredTransferLoginFolder: async () => null, isLiveOnProfile: (_id: string, requireOverlay: boolean) => !requireOverlay,
        invoke: vi.fn(async () => 17), downloadFile, uploadFile: vi.fn(), parentDir: () => '/local',
        batchCancelledRef: { current: false }, notify: { error: vi.fn() }, t: (s: string) => s, isCancelledTransferError: () => false };
    const retry = new Function(...Object.keys(ctx), `${code}; return retryRestoredEntry;`)(...Object.values(ctx));
    await retry('entry', { direction: 'download', profile_id: 'A', filename: 'f', remote_path: '/public/f', local_path: '/local/f' });
    expect(downloadFile).toHaveBeenCalledWith('/public/f', 'f', '/local', false, undefined, false, 'throw', { profileId: 'A', generation: 17, requireOverlay: false, cryptScope: '/vault' });
    expect(ctx.transferQueue.completeTransfer).toHaveBeenCalledWith('entry');
    expect(ctx.transferQueue.failTransfer).not.toHaveBeenCalled();
});

// #1523 Greptile: the backend compares the active overlay's absolute anchor,
// and an SFTP profile's `docs` is `~/docs`, which only the session places.
// Unplaced, every restored retry under it was refused.
it('places a scope under the login folder through the live session before a restored retry', async () => {
    const helper = (await transformWithOxc(block('  const restoredTransferLoginFolder =', '  const getEnabledProfileOverlay ='), 'login-folder.ts')).code;
    const code = (await transformWithOxc(block('  const retryRestoredEntry =', '  // Queues the restorable entries'), 'retry-login.ts')).code;
    const invoke = vi.fn(async (cmd: string) => (cmd === 'provider_login_folder' ? '/home/u' : 23));
    const restoredTransferLoginFolder = new Function('invoke', 'isLoginRelativeScope', 'profileOverlayScope',
        `${helper}; return restoredTransferLoginFolder;`)(invoke, isLoginRelativeScope, profileOverlayScope);
    const profile = { id: 'S', username: 'u', protocol: 'sftp', initialPath: 'docs', aeroCryptOverlay: { enabled: true, remoteScope: '' } };
    const run = async (remotePath: string) => {
        const downloadFile = vi.fn(async (..._args: unknown[]) => true);
        const ctx = { useLatestCallback: (fn: unknown) => fn, transferQueue: { startTransfer: vi.fn(), failTransfer: vi.fn(), completeTransfer: vi.fn() },
            loadSavedServerProfiles: async () => [profile],
            restoredTransferCryptScope, restoredTransferRequiresOverlay, restoredTransferLoginFolder, isLiveOnProfile: () => true,
            invoke, downloadFile, uploadFile: vi.fn(), parentDir: () => '/local',
            batchCancelledRef: { current: false }, notify: { error: vi.fn() }, t: (s: string) => s, isCancelledTransferError: () => false };
        const retry = new Function(...Object.keys(ctx), `${code}; return retryRestoredEntry;`)(...Object.values(ctx));
        await retry('entry', { direction: 'download', profile_id: 'S', filename: 'f', remote_path: remotePath, local_path: '/local/f' });
        expect(ctx.transferQueue.failTransfer).not.toHaveBeenCalled();
        return downloadFile.mock.calls[0]?.[7];
    };
    expect(await run('/home/u/docs/opaque')).toEqual({ profileId: 'S', generation: 23, requireOverlay: true, cryptScope: '/home/u/docs' });
    // Beside the island the file is plaintext, read against the same placed anchor.
    expect(await run('/home/u/public/f')).toEqual({ profileId: 'S', generation: 23, requireOverlay: false, cryptScope: '/home/u/docs' });
    expect(invoke).toHaveBeenCalledWith('provider_login_folder');
});

it('asks the session for no login folder when the scope is absolute', async () => {
    const helper = (await transformWithOxc(block('  const restoredTransferLoginFolder =', '  const getEnabledProfileOverlay ='), 'login-folder-abs.ts')).code;
    const invoke = vi.fn(async () => '/home/u');
    const restoredTransferLoginFolder = new Function('invoke', 'isLoginRelativeScope', 'profileOverlayScope',
        `${helper}; return restoredTransferLoginFolder;`)(invoke, isLoginRelativeScope, profileOverlayScope);
    expect(await restoredTransferLoginFolder({ protocol: 'sftp', initialPath: '/srv', aeroCryptOverlay: { enabled: true, remoteScope: '' } })).toBeNull();
    expect(await restoredTransferLoginFolder({ protocol: 'ftp', initialPath: 'docs', aeroCryptOverlay: { enabled: true, remoteScope: '' } })).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
    // A session that cannot report it leaves the scope unplaced (fail closed).
    const failing = vi.fn(async () => { throw new Error('no session'); });
    const unplaced = new Function('invoke', 'isLoginRelativeScope', 'profileOverlayScope',
        `${helper}; return restoredTransferLoginFolder;`)(failing, isLoginRelativeScope, profileOverlayScope);
    expect(await unplaced({ protocol: 'scp', initialPath: 'docs', aeroCryptOverlay: { enabled: true, remoteScope: '' } })).toBeNull();
});
