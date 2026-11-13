// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import english from '../i18n/locales/en.json';
import { UsersManagePanel } from './UsersManagePanel';
import { addUser, listUsers } from '../utils/userPartitions';

const { adminUser } = vi.hoisted(() => ({
    adminUser: {
        id: 1, name: 'admin', hasPassphrase: false, sortOrder: 0, createdAt: 0, updatedAt: 0,
        isActive: true, isAdmin: true, isDefault: true,
    },
}));
vi.mock('../utils/userPartitions', () => ({
    addUser: vi.fn(), changeUserPassphrase: vi.fn(), copyUser: vi.fn(), deleteUser: vi.fn(),
    getUserStorageStats: vi.fn(async () => []),
    getUnlockStatus: vi.fn(async () => ({ isUnlocked: true, unlockedUserId: 1, activeUserId: 1 })),
    defaultUserIdFromList: vi.fn(() => 1),
    initUserPartitions: vi.fn(async () => ({
        schemaVersion: '1', createdDefaultUser: false, migratedProfiles: 0,
        migratedSettingsScopes: 0, alreadyMigrated: true,
    })),
    legacyDefaultToMigrate: vi.fn(() => false),
    listUsers: vi.fn(async () => [adminUser]),
    readDefaultAccountId: vi.fn(() => null), renameUser: vi.fn(), reorderUsers: vi.fn(),
    setDefaultUser: vi.fn(), setUserAdmin: vi.fn(), setUserAvatar: vi.fn(),
    writeDefaultAccountId: vi.fn(),
}));
vi.mock('../utils/serverProfileStore', () => ({ PROFILES_CHANGED_EVENT: 'profiles-changed' }));
const translate = (key: string) => key.split('.').reduce<unknown>(
    (value, part) => (value as Record<string, unknown>)?.[part], english.translations,
) as string;
vi.mock('../i18n', () => ({ useTranslation: () => translate }));
vi.mock('../hooks/useDraggableModal', () => ({
    useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => undefined) }));

let root: Root;
let host: HTMLDivElement;

const render = async (el: React.ReactElement) => {
    await act(async () => root.render(el));
};
const flush = async () => {
    await act(async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); });
};

beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    vi.mocked(listUsers).mockResolvedValue([adminUser]);
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it('reports account creation as busy, not dirty, until addUser settles', async () => {
    let resolveAdd!: () => void;
    vi.mocked(addUser).mockImplementation(() => new Promise<typeof adminUser>(res => { resolveAdd = () => res(adminUser); }));
    const onClose = vi.fn();
    await render(createElement(UsersManagePanel, { isOpen: true, onClose, onChanged: vi.fn() }));
    await flush();
    const nameInput = host.querySelector<HTMLInputElement>('input[placeholder]');
    expect(nameInput).not.toBeNull();
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(nameInput, 'newbie');
        nameInput!.dispatchEvent(new Event('input', { bubbles: true }));
    });
    const submit = host.querySelector<HTMLButtonElement>('button[type=submit]');
    await act(async () => { submit!.closest('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); });
    await flush();
    expect(addUser).toHaveBeenCalled();
    // Close while creation is in flight: the confirm must be the busy one.
    const x = [...host.querySelectorAll<HTMLButtonElement>('button')].find(b => b.innerHTML.includes('lucide-x'));
    await act(async () => { x!.click(); });
    const alert = host.querySelector('[role=alertdialog]');
    expect(alert).not.toBeNull();
    expect(alert!.textContent).toContain('Operation in progress');
    expect(onClose).not.toHaveBeenCalled();
    await act(async () => { resolveAdd(); });
    await flush();
});

it('stays busy while a second account creation is still pending', async () => {
    const resolvers: Array<() => void> = [];
    vi.mocked(addUser).mockImplementation(() => new Promise<typeof adminUser>(res => {
        resolvers.push(() => res(adminUser));
    }));
    const onClose = vi.fn();
    await render(createElement(UsersManagePanel, { isOpen: true, onClose, onChanged: vi.fn() }));
    await flush();
    const nameInput = host.querySelector<HTMLInputElement>('input[placeholder]')!;
    const submitForm = async () => {
        const submit = host.querySelector<HTMLButtonElement>('button[type=submit]')!;
        await act(async () => { submit.closest('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); });
        await flush();
    };
    const typeName = async (value: string) => {
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(nameInput, value);
            nameInput.dispatchEvent(new Event('input', { bubbles: true }));
        });
    };
    const clickX = async () => {
        const x = [...host.querySelectorAll<HTMLButtonElement>('button')].find(b => b.innerHTML.includes('lucide-x'))!;
        await act(async () => { x.click(); });
    };
    await typeName('first');
    await submitForm();
    await typeName('second');
    await submitForm();
    expect(addUser).toHaveBeenCalledTimes(2);
    expect(resolvers.length).toBe(2);
    // The first creation settles while the second is still in flight.
    await act(async () => { resolvers[0](); });
    await flush();
    await clickX();
    const alert = host.querySelector('[role=alertdialog]');
    expect(alert).not.toBeNull();
    expect(alert!.textContent).toContain('Operation in progress');
    expect(onClose).not.toHaveBeenCalled();
    await act(async () => { resolvers[1](); });
    await flush();
});
