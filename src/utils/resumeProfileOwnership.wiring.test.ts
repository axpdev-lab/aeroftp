// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement } from 'react';
import { createRoot } from 'react-dom/client';
import { afterEach, expect, it, vi } from 'vitest';
import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
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
        sessions: current.sessions, isLiveOnProfile: readFromOriginalRender, waitForLiveProfile: guard.waitForLiveProfile, connectToFtp, switchSession,
        setShowConnectionScreen: vi.fn(), setShowRemotePanel: vi.fn(), setShowLocalPreview: vi.fn(), setIsConnected: vi.fn(),
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
    const source = block('      const ok = await connectSavedProfileForResume(profile);', '\n    }\n  };\n\n  // Two-level cancel');
    const code = (await transformWithOxc(`for (const profileId of ['profile-B']) { ${source} }`, 'resume-dispatch.ts')).code;
    const clearRestoredFlags = vi.fn();
    const fireRetryCallbacks = vi.fn();
    const ctx = { profile: { id: 'profile-B' }, groupIds: ['restored'], connectSavedProfileForResume: async () => true,
        isLiveOnProfile: () => false, transferQueue: { clearRestoredFlags }, fireRetryCallbacks };
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
        loadSavedServerProfiles: async () => [{ id: 'profile-A' }], isLiveOnProfile: () => live,
        invoke: vi.fn(async () => { live = false; return 7; }), downloadFile, uploadFile: vi.fn(),
        parentDir: () => '/local', batchCancelledRef: { current: false }, notify: { error: vi.fn() }, t: (s: string) => s,
        isCancelledTransferError: () => false };
    const retry = new Function(...Object.keys(ctx), `${code}; return retryRestoredEntry;`)(...Object.values(ctx));
    await retry('entry', { direction: 'download', profile_id: 'profile-A', filename: 'f.txt', remote_path: '/f.txt', local_path: '/local/f.txt' });
    expect(downloadFile).not.toHaveBeenCalled();
    expect(completeTransfer).not.toHaveBeenCalled();
    expect(failTransfer).toHaveBeenCalledWith('entry', expect.stringContaining('CONNECTION_CHANGED'));
});
