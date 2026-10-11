// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// The unlock dialogs await a key derivation, a probe or a create, and then the
// apply in App. Closing the dialog meanwhile must not open an overlay for it,
// nor leave behind the standalone rclone keys its Decrypt tools would have used
// (#1162 review).

import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { RcloneCryptUnlock } from './RcloneCryptUnlock';
import { AeroCryptUnlock } from './AeroCryptUnlock';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('@tauri-apps/plugin-fs', () => ({ writeTextFile: vi.fn() }));
vi.mock('../utils/pickPath', () => ({ pickFile: vi.fn(), pickSave: vi.fn() }));
vi.mock('../i18n', async () => {
    const { translations } = (await import('../i18n/locales/en.json')).default as { translations: Record<string, unknown> };
    const t = (key: string) => {
        const value = key.split('.').reduce<unknown>((node, part) => (node as Record<string, unknown> | undefined)?.[part], translations);
        return typeof value === 'string' ? value : key;
    };
    return { useTranslation: () => t };
});

const deferred = <T,>() => {
    let resolve!: (value: T) => void;
    const promise = new Promise<T>((r) => { resolve = r; });
    return { promise, resolve };
};

const TOOL_KEYS = { vault_id: 'tool-1', filename_encryption: 'standard', directory_name_encryption: true };

let root: Root;
let host: HTMLDivElement;
const passwordFields = () => Array.from(host.querySelectorAll<HTMLInputElement>('input[type=password]'));
const type = async (element: HTMLInputElement, value: string) => {
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(element, value);
        element.dispatchEvent(new Event('input', { bubbles: true }));
    });
};
const click = async (label: string) => {
    const button = Array.from(host.querySelectorAll('button')).find((b) => b.textContent?.trim() === label);
    if (!button) throw new Error(`no button "${label}"`);
    await act(async () => button.click());
};
const pressEnter = async (element: HTMLInputElement) => {
    await act(async () => {
        element.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    });
};
const close = async () => { await act(async () => root.render(null)); };
const lockCalls = () => invoke.mock.calls.filter(([command]) => command === 'rclone_crypt_lock').map(([, args]) => args);

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    invoke.mockReset();
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

describe('RcloneCryptUnlock closed while it works', () => {
    const render = async (onUnlocked: () => Promise<void>) => {
        await act(async () => root.render(createElement(RcloneCryptUnlock, { onClose: () => {}, onUnlocked, scope: '/RcVault' })));
    };

    it('applies nothing and locks the keys when closed during the key derivation', async () => {
        const unlock = deferred<typeof TOOL_KEYS>();
        invoke.mockImplementation(async (command: string) => (command === 'rclone_crypt_unlock' ? unlock.promise : undefined));
        const onUnlocked = vi.fn(async () => {});
        await render(onUnlocked);
        await type(passwordFields()[0], 'rc-pass');
        await click('Unlock');
        await close();
        await act(async () => unlock.resolve(TOOL_KEYS));
        expect(onUnlocked).not.toHaveBeenCalled();
        expect(lockCalls()).toEqual([{ vaultId: 'tool-1' }]);
    });

    it('locks the tools\' keys when closed while App applies the overlay', async () => {
        invoke.mockImplementation(async (command: string) => (command === 'rclone_crypt_unlock' ? TOOL_KEYS : undefined));
        const apply = deferred<void>();
        const onUnlocked = vi.fn(() => apply.promise);
        await render(onUnlocked);
        await type(passwordFields()[0], 'rc-pass');
        await click('Unlock');
        expect(onUnlocked).toHaveBeenCalledTimes(1);
        await close();
        await act(async () => apply.resolve());
        expect(lockCalls()).toEqual([{ vaultId: 'tool-1' }]);
    });

    it('locks a remote it created when closed before the apply', async () => {
        const create = deferred<typeof TOOL_KEYS & { root: string }>();
        invoke.mockImplementation(async (command: string) => (command === 'rclone_crypt_provider_create_remote' ? create.promise : undefined));
        const onUnlocked = vi.fn(async () => {});
        await render(onUnlocked);
        await click('Create new');
        const [password, confirm] = passwordFields();
        await type(password, 'rc-pass');
        await type(confirm, 'rc-pass');
        await click('Create & Unlock');
        await close();
        await act(async () => create.resolve({ ...TOOL_KEYS, root: '/RcVault' }));
        expect(onUnlocked).not.toHaveBeenCalled();
        expect(lockCalls()).toEqual([{ vaultId: 'tool-1' }]);
    });

    it('derives the keys once when Enter is pressed again while unlocking', async () => {
        const unlock = deferred<typeof TOOL_KEYS>();
        invoke.mockImplementation(async (command: string) => (command === 'rclone_crypt_unlock' ? unlock.promise : undefined));
        await render(vi.fn(async () => {}));
        const password = passwordFields()[0];
        await type(password, 'rc-pass');
        await pressEnter(password);
        await pressEnter(password);
        expect(invoke.mock.calls.filter(([command]) => command === 'rclone_crypt_unlock')).toHaveLength(1);
        await act(async () => unlock.resolve(TOOL_KEYS));
    });
});

describe('AeroCryptUnlock closed while it works', () => {
    const render = async (onUnlocked: () => Promise<void>) => {
        await act(async () => root.render(createElement(AeroCryptUnlock, { onClose: () => {}, onUnlocked, scope: '/Vault' })));
    };

    it('opens nothing when closed while probing the folder for its marker', async () => {
        const probe = deferred<{ hasCurrentMarker: boolean; hasLegacyMarker: boolean }>();
        invoke.mockImplementation(async (command: string) => (command === 'aerocrypt_provider_marker_status' ? probe.promise : undefined));
        const onUnlocked = vi.fn(async () => {});
        await render(onUnlocked);
        await type(passwordFields()[0], 'Xq7#mP2$vL9&wR4!tZ6^kNb');
        await click('Unlock');
        await close();
        await act(async () => probe.resolve({ hasCurrentMarker: true, hasLegacyMarker: false }));
        expect(onUnlocked).not.toHaveBeenCalled();
    });

    it('opens nothing when closed while creating the vault', async () => {
        const create = deferred<{ root: string; version: number; configJson: string }>();
        invoke.mockImplementation(async (command: string) => (command === 'aerocrypt_provider_create_remote' ? create.promise : undefined));
        const onUnlocked = vi.fn(async () => {});
        await render(onUnlocked);
        await click('Create new');
        const [password, confirm] = passwordFields();
        await type(password, 'Xq7#mP2$vL9&wR4!tZ6^kNb');
        await type(confirm, 'Xq7#mP2$vL9&wR4!tZ6^kNb');
        await click('Create and unlock');
        await close();
        await act(async () => create.resolve({ root: '/Vault/New', version: 3, configJson: '{}' }));
        expect(onUnlocked).not.toHaveBeenCalled();
    });

    it('creates once when Enter is pressed again while creating', async () => {
        const create = deferred<{ root: string; version: number; configJson: string }>();
        invoke.mockImplementation(async (command: string) => (command === 'aerocrypt_provider_create_remote' ? create.promise : undefined));
        await render(vi.fn(async () => {}));
        await click('Create new');
        const [password, confirm] = passwordFields();
        await type(password, 'Xq7#mP2$vL9&wR4!tZ6^kNb');
        await type(confirm, 'Xq7#mP2$vL9&wR4!tZ6^kNb');
        await pressEnter(confirm);
        await pressEnter(confirm);
        expect(invoke.mock.calls.filter(([command]) => command === 'aerocrypt_provider_create_remote')).toHaveLength(1);
        await act(async () => create.resolve({ root: '/Vault/New', version: 3, configJson: '{}' }));
    });
});
