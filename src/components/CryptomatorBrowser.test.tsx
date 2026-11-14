// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
import React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { CryptomatorBrowser } from './CryptomatorBrowser';

const mocks = vi.hoisted(() => ({
    invoke: vi.fn(),
    decryptResolvers: [] as ((v: unknown) => void)[],
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../utils/pickPath', () => ({
    pickFile: vi.fn(async () => '/vault'),
    pickSave: vi.fn(async () => '/tmp/out.bin'),
}));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../hooks/useArchiveProgress', () => ({ useArchiveProgress: () => null }));
vi.mock('./common/MountVaultButton', () => ({ MountVaultButton: () => null }));
vi.mock('./common/SaveAllMenu', () => ({ SaveAllMenu: () => null }));

let root: Root | undefined;
let host: HTMLDivElement;

const entries = [
    { name: 'a.txt', isDir: false, size: 10, dirId: null },
    { name: 'b.txt', isDir: false, size: 20, dirId: null },
];

beforeEach(async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    mocks.decryptResolvers = [];
    mocks.invoke.mockImplementation(async (command: string) => {
        if (command === 'cryptomator_unlock') return { vaultId: 'v1', name: 'vault', format: 8 };
        if (command === 'cryptomator_list') return entries;
        if (command === 'cryptomator_decrypt_file') {
            return new Promise(res => mocks.decryptResolvers.push(res));
        }
        return undefined;
    });
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});

afterEach(async () => {
    if (root) await React.act(async () => root!.unmount());
    host?.remove();
    root = undefined;
    mocks.invoke.mockReset();
});

async function unlockAndList() {
    await React.act(async () => root!.render(<CryptomatorBrowser onClose={vi.fn()} initialVaultPath="/vault" />));
    const password = host.querySelector<HTMLInputElement>('input[type="password"]')!;
    await React.act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(password, 'pw');
        password.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cryptomator.unlock'))!.click());
    await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
    expect(host.textContent).toContain('a.txt');
}

describe('CryptomatorBrowser decrypt guard', () => {
    it('stays busy until every overlapping decrypt settles', async () => {
        const onClose = vi.fn();
        await React.act(async () => root!.render(<CryptomatorBrowser onClose={onClose} initialVaultPath="/vault" />));
        const password = host.querySelector<HTMLInputElement>('input[type="password"]')!;
        await React.act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(password, 'pw');
            password.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('cryptomator.unlock'))!.click());
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        expect(host.textContent).toContain('a.txt');

        const decryptButtons = () => [...host.querySelectorAll<HTMLButtonElement>('button[title="cryptomator.decryptAndSave"]')];
        expect(decryptButtons()).toHaveLength(2);
        // Two decrypts start before the first settle.
        await React.act(async () => { decryptButtons()[0].click(); decryptButtons()[1].click(); });
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); });
        expect(mocks.invoke.mock.calls.filter(c => c[0] === 'cryptomator_decrypt_file')).toHaveLength(2);
        expect(mocks.decryptResolvers).toHaveLength(2);

        const closeX = () => host.querySelector<HTMLButtonElement>('button[title="common.close"]')!;
        await React.act(async () => closeX().click());
        expect(host.textContent).toContain('modalGuard.busyTitle');
        expect(onClose).not.toHaveBeenCalled();

        // The first decrypt settling must not drop the guard.
        await React.act(async () => mocks.decryptResolvers[0]({}));
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        await React.act(async () => [...host.querySelectorAll('button')].find(b => b.textContent?.includes('modalGuard.wait'))!.click());
        await React.act(async () => closeX().click());
        expect(host.textContent).toContain('modalGuard.busyTitle');
        expect(onClose).not.toHaveBeenCalled();

        await React.act(async () => mocks.decryptResolvers[1]({}));
        await React.act(async () => { await Promise.resolve(); await Promise.resolve(); });
        await React.act(async () => closeX().click());
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});
