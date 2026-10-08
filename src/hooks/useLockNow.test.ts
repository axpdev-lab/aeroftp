// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement as h } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useLockNow } from './useLockNow';
import { LockNowError } from '../components/LockNowError';
import { useKeyboardShortcuts } from './useKeyboardShortcuts';
import { PROFILES_CHANGED_EVENT } from '../utils/serverProfileStore';
const bridge = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => bridge);
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
let api: ReturnType<typeof useLockNow>;
let root: Root; let host: HTMLDivElement;
let vault = false; let locked = false;
const onVaultLocked = vi.fn(); const interrupted = vi.fn(); const unrelated = vi.fn();
function Harness() {
    api = useLockNow({ vaultConfigured: vault, locked, onVaultLocked });
    useKeyboardShortcuts({ 'Ctrl+K': unrelated });
    return h('div', {}, h('textarea'), api.error ? h(LockNowError, { error: api.error, onClose: api.dismissError }) : null);
}
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    bridge.invoke.mockReset().mockImplementation(async name => {
        if (name === 'user_partitions_list_users') return [{ id: 1, hasPassphrase: true }];
        if (name === 'user_partitions_unlock_status') return { activeUserId: 1, isUnlocked: true };
    });
    window.__aeroftpController = { interrupt: interrupted } as unknown as NonNullable<Window['__aeroftpController']>;
    vault = false; locked = false;
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); delete window.__aeroftpController; vi.clearAllMocks(); });
const mount = async () => { await act(async () => root.render(h(Harness))); };
it.each(['ctrl', 'meta'] as const)('locks while typing with %s, Alt/Option, Shift and physical K', async modifier => {
    vault = true; await mount(); const input = host.querySelector('textarea')!; input.focus();
    const event = new KeyboardEvent('keydown', { code: 'KeyK', key: modifier === 'meta' ? '˚' : 'K', ctrlKey: modifier === 'ctrl', metaKey: modifier === 'meta', altKey: true, shiftKey: true, bubbles: true, cancelable: true });
    const terminal = vi.fn(); input.addEventListener('keydown', terminal);
    await act(async () => input.dispatchEvent(event));
    expect(event.defaultPrevented).toBe(true); expect(terminal).not.toHaveBeenCalled(); expect(interrupted).toHaveBeenCalled();
    expect(bridge.invoke).toHaveBeenCalledWith('user_partitions_lock_session');
    expect(bridge.invoke).toHaveBeenCalledWith('lock_credential_store'); expect(onVaultLocked).toHaveBeenCalledTimes(1);
});
it('leaves unrelated editor/terminal modifiers alone and ignores repeat lock keys', async () => {
    await mount(); const input = host.querySelector('textarea')!; input.focus(); const terminal = vi.fn(); input.addEventListener('keydown', terminal);
    const event = new KeyboardEvent('keydown', { key: 'k', code: 'KeyK', ctrlKey: true, bubbles: true, cancelable: true });
    await act(async () => input.dispatchEvent(event)); expect(event.defaultPrevented).toBe(false); expect(terminal).toHaveBeenCalledTimes(1);
    await act(async () => input.dispatchEvent(new KeyboardEvent('keydown', { key: 'K', ctrlKey: true, altKey: true, shiftKey: true, repeat: true, bubbles: true })));
    expect(interrupted).not.toHaveBeenCalled();
});
it('shares the vault-only control without requiring account discovery', async () => {
    vault = true; bridge.invoke.mockRejectedValueOnce(new Error('lock failed')); await mount();
    await act(async () => { await api.lock('vault'); });
    expect(bridge.invoke).toHaveBeenCalledTimes(1); expect(bridge.invoke).toHaveBeenCalledWith('lock_credential_store');
    expect(onVaultLocked).not.toHaveBeenCalled(); expect(host.querySelector('[role="alertdialog"]')).not.toBeNull();
    expect(host.textContent).toContain('lockScreen.lockFailed');
});
it('reports a password-free single-account/no-vault configuration without claiming it is locked', async () => {
    bridge.invoke.mockImplementation(async name => name === 'user_partitions_list_users' ? [{ id: 1, hasPassphrase: false }] : { activeUserId: 1, isUnlocked: true });
    await mount(); await act(async () => { await api.lock(); });
    expect(api.error).toBe('unavailable'); expect(onVaultLocked).not.toHaveBeenCalled();
    expect(bridge.invoke.mock.calls.map(call => call[0])).not.toContain('user_partitions_lock_session');
});
it('keeps a multi-account logout available even without an account passphrase', async () => {
    bridge.invoke.mockImplementation(async name => name === 'user_partitions_list_users' ? [{ id: 1, hasPassphrase: false }, { id: 2, hasPassphrase: false }] : { activeUserId: 1, isUnlocked: true });
    await mount(); await act(async () => { await api.lock('account'); });
    expect(api.error).toBeNull(); expect(bridge.invoke).toHaveBeenCalledWith('user_partitions_lock_session');
});
it('drops in-flight UI confirmation after a profile/account change and cleans up capture handling', async () => {
    vault = true; await mount(); const removed = vi.spyOn(window, 'removeEventListener');
    bridge.invoke.mockImplementation(async () => { window.dispatchEvent(new Event(PROFILES_CHANGED_EVENT)); return []; });
    await act(async () => { await api.lock(); });
    expect(api.error).toBe('stale'); expect(onVaultLocked).not.toHaveBeenCalled();
    await act(async () => root.unmount());
    expect(removed).toHaveBeenCalledWith('keydown', expect.any(Function), true); root = createRoot(host);
});
