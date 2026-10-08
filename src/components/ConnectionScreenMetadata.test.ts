// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act, useState } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ConnectionParams, ProviderType, ServerProfile } from '../types';

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), log: vi.fn(), t: (key: string) => key }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../i18n', () => ({
    useTranslation: () => mocks.t,
    useI18n: () => ({ t: mocks.t, language: 'en' }),
}));
vi.mock('../hooks/useActivityLog', () => ({ useActivityLog: () => ({ log: mocks.log }) }));
// Keep the real QuickConnect controls and save handlers, without signing in.
vi.mock('./OAuthConnect', () => ({
    OAuthConnect: ({ rightColumn, onConnected }: { rightColumn: React.ReactNode; onConnected: (name: string) => void }) =>
        React.createElement('div', null, rightColumn,
            React.createElement('button', { onClick: () => onConnected('Account') }, 'Test OAuth sign-in')),
}));
vi.mock('./IconPickerDialog', () => ({
    IconPickerDialog: ({ onSelect, onClose }: { onSelect: (url: string) => void; onClose: () => void }) =>
        React.createElement('button', { onClick: () => { onSelect(icon); onClose(); } }, 'Test select icon'),
}));
vi.mock('./IntroHub/MyServersPanel', () => ({
    MyServersPanel: ({ onEdit }: { onEdit: (profile: ServerProfile) => void }) =>
        React.createElement('div', null, profiles.map(profile => React.createElement('button', {
            key: profile.id, onClick: () => onEdit(profile),
        }, `Edit ${profile.name}`))),
}));
vi.mock('./IntroHub/DiscoverPanel', () => ({
    DiscoverPanel: ({ onSelectProvider }: { onSelectProvider: (protocol: ProviderType) => void }) =>
        React.createElement('button', { onClick: () => onSelectProvider('ftp') }, 'Add FTP'),
}));
vi.mock('./IntroHub/PortableIsolationBanner', () => ({ PortableIsolationBanner: () => null }));

import { ConnectionScreen } from './ConnectionScreen';
import { IntroHub } from './IntroHub/IntroHub';

const icon = 'data:image/png;base64,dGVzdA==';
let profiles: ServerProfile[];
let container: HTMLDivElement;
let root: Root;
let onFormSaved: ReturnType<typeof vi.fn<() => void>>;

beforeEach(() => {
    vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
    localStorage.clear();
    mocks.invoke.mockReset();
    mocks.log.mockReset();
    onFormSaved = vi.fn<() => void>();
    mocks.invoke.mockImplementation(async (command: string, args?: { profiles?: ServerProfile[] }) => {
        if (command === 'user_partitions_load_active_server_profiles') return structuredClone(profiles);
        if (command === 'user_partitions_save_active_server_profiles') {
            // Match the vault's JSON roundtrip, including removal of undefined fields.
            profiles = JSON.parse(JSON.stringify(args!.profiles));
            return;
        }
        if (command === 'get_credential') return 'fixture-password';
        if (command === 'fourshared_connect') return { display_name: 'Account', account_email: null };
        return null;
    });
    container = document.createElement('div');
    document.body.append(container);
    root = createRoot(container);
});

afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    vi.unstubAllGlobals();
});

async function openEditor(protocol: ProviderType, customIconUrl: string | null = icon) {
    profiles = [{
        id: 'edited', name: 'Account', host: 'example.test', port: 443, username: 'user',
        protocol, password: 'fixture-password', initialPath: '/remote', localInitialPath: '/local', customIconUrl: customIconUrl ?? undefined,
        options: { region: 'eu' },
    }, {
        id: 'other', name: 'Other', host: 'other.test', port: 22, username: 'other', protocol: 'sftp',
    }];
    await renderEditor();
}

async function renderEditor() {
    const profile = profiles[0];
    function Harness() {
        const [params, setParams] = useState<ConnectionParams>({ protocol: profile.protocol, server: '', username: '', password: '' });
        const [dirs, setDirs] = useState({ remoteDir: '', localDir: '' });
        return React.createElement(ConnectionScreen, {
            connectionParams: params, quickConnectDirs: dirs, loading: false,
            onConnectionParamsChange: setParams, onQuickConnectDirsChange: setDirs,
            onConnect: vi.fn(), editingProfile: profile, onFormSaved,
        });
    }
    await act(async () => root.render(React.createElement(Harness)));
}

function button(text: string) {
    const found = [...container.querySelectorAll('button')].find(b => b.textContent?.trim() === text);
    expect(found, `button ${text}`).toBeDefined();
    return found!;
}

async function removeIcon() {
    const remove = container.querySelector<HTMLButtonElement>('button[title="settings.removeIcon"]');
    expect(remove, 'custom icon remove button').not.toBeNull();
    await act(async () => remove!.click());
}

async function clearRemotePath() {
    const input = [...container.querySelectorAll('input')].find(i => i.value === '/remote');
    expect(input, 'remote path input').toBeDefined();
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, '');
        input!.dispatchEvent(new Event('input', { bubbles: true }));
    });
}

describe('QuickConnect metadata removals (#1100)', () => {
    it.each<ProviderType>(['googledrive', 'googlephotos', 'dropbox', 'onedrive', 'box', 'pcloud', 'zohoworkdrive', 'yandexdisk', 'fourshared'])(
        '%s enables Save for icon removal alone and keeps it removed after reopening', async protocol => {
            await openEditor(protocol);
            expect(button('common.save').disabled).toBe(true);
            const other = structuredClone(profiles[1]);
            await removeIcon();
            expect(button('common.save').disabled).toBe(false);
            await act(async () => button('common.save').click());
            expect(profiles[0].customIconUrl).toBeUndefined();
            expect(profiles[0]).toMatchObject({ host: 'example.test', username: 'user', initialPath: '/remote', options: { region: 'eu' } });
            expect(profiles[1]).toEqual(other);
            expect(onFormSaved).toHaveBeenCalledOnce();
            await renderEditor();
            expect(container.querySelector('button[title="settings.removeIcon"]')).toBeNull();
            expect(button('common.save').disabled).toBe(true);
        },
    );

    it.each([false, true])('persists an empty OAuth remote path (also remove icon: %s)', async alsoRemoveIcon => {
        await openEditor('pcloud');
        await clearRemotePath();
        if (alsoRemoveIcon) await removeIcon();
        expect(button('common.save').disabled).toBe(false);
        await act(async () => button('common.save').click());
        expect(profiles[0].initialPath).toBe('');
        expect(profiles[0].customIconUrl).toBe(alsoRemoveIcon ? undefined : icon);
        await renderEditor();
        expect(button('common.save').disabled).toBe(true);
        expect([...container.querySelectorAll('input')].some(i => i.value === '/remote')).toBe(false);
    });

    it('returns Save to disabled when the original icon is selected again', async () => {
        await openEditor('pcloud');
        await removeIcon();
        expect(button('common.save').disabled).toBe(false);
        await act(async () => container.querySelector<HTMLButtonElement>('button[title="settings.chooseIcon"]')!.click());
        await act(async () => button('Test select icon').click());
        expect(button('common.save').disabled).toBe(true);
    });

    it('keeps a profile without a custom icon unchanged until another field changes', async () => {
        await openEditor('pcloud', null);
        expect(button('common.save').disabled).toBe(true);
        expect(container.querySelector('button[title="settings.removeIcon"]')).toBeNull();
        await clearRemotePath();
        await act(async () => button('common.save').click());
        expect(profiles[0].customIconUrl).toBeUndefined();
    });

    it('discards icon and path removals on Cancel', async () => {
        await openEditor('pcloud');
        const before = structuredClone(profiles);
        await removeIcon();
        await clearRemotePath();
        await act(async () => button('common.cancel').click());
        expect(profiles).toEqual(before);
        expect(mocks.invoke.mock.calls.some(([command]) => command === 'user_partitions_save_active_server_profiles')).toBe(false);
        await renderEditor();
        expect(button('common.save').disabled).toBe(true);
        expect(container.querySelector('button[title="settings.removeIcon"]')).not.toBeNull();
    });

    it('also persists removals when an edited OAuth profile signs in again', async () => {
        await openEditor('pcloud');
        await removeIcon();
        await clearRemotePath();
        await act(async () => button('Test OAuth sign-in').click());
        expect(profiles[0].customIconUrl).toBeUndefined();
        expect(profiles[0].initialPath).toBe('');
        expect(profiles).toHaveLength(2);
    });

    it('persists icon removal together with a rename', async () => {
        await openEditor('pcloud');
        await removeIcon();
        const name = [...container.querySelectorAll('input')].find(i => i.value === 'Account')!;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(name, 'Renamed');
            name.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await act(async () => button('common.save').click());
        expect(profiles[0]).toMatchObject({ name: 'Renamed', initialPath: '/remote' });
        expect(profiles[0].customIconUrl).toBeUndefined();
    });

    it.each<ProviderType>(['ftp', 'sftp', 'webdav', 'mega', 'swift'])('%s persists removal through the standard Save', async protocol => {
        await openEditor(protocol);
        await removeIcon();
        await clearRemotePath();
        const save = button('common.save');
        expect(save.disabled).toBe(false);
        await act(async () => save.click());
        expect(profiles[0].customIconUrl).toBeUndefined();
        expect(profiles[0].initialPath).toBe('');
        expect(onFormSaved).toHaveBeenCalledOnce();
    });
});

describe('4shared and Swift share the modern QuickConnect columns', () => {
    it('Swift keeps optional endpoint settings collapsed and retains edits after closing them', async () => {
        await openEditor('swift');
        const toggle = button('connection.optionalSettings');
        expect(toggle.getAttribute('aria-expanded')).toBe('false');
        expect(container.querySelector('input[placeholder="connection.swiftAuthUrl"]')).toBeNull();
        await act(async () => toggle.click());
        const endpoint = container.querySelector<HTMLInputElement>('input[placeholder="connection.swiftAuthUrl"]')!;
        expect(endpoint.value).toBe('example.test');
        expect(endpoint.disabled).toBe(true);
        expect(container.querySelector(`label[for="${endpoint.id}"]`)?.textContent).toBe('connection.swiftAuthUrl');
        await act(async () => button('common.edit').click());
        expect(endpoint.disabled).toBe(true);
        await act(async () => button('common.cancel').click());
        expect(endpoint.disabled).toBe(true);
        await act(async () => button('common.edit').click());
        await act(async () => button('protocol.advancedUnlock').click());
        expect(endpoint.disabled).toBe(false);
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(endpoint, 'https://authenticate.blomp.com');
            endpoint.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await act(async () => toggle.click());
        expect(container.querySelector('input[placeholder="connection.swiftAuthUrl"]')).toBeNull();
        await act(async () => toggle.click());
        expect(container.querySelector<HTMLInputElement>('input[placeholder="connection.swiftAuthUrl"]')!.value).toBe('https://authenticate.blomp.com');
        expect(container.querySelector<HTMLInputElement>('input[placeholder="connection.swiftAuthUrl"]')!.disabled).toBe(false);
    });

    it.each<ProviderType>(['fourshared', 'swift'])('%s hydrates the actual profile name beside its icon in the right column', async protocol => {
        await openEditor(protocol);
        const columns = container.querySelector('[class~="md:grid-cols-2"]')!;
        expect(columns).not.toBeNull();
        const right = columns.lastElementChild!;
        expect(right.querySelector('input')!.value).toBe('Account');
        expect(right.querySelector('button[title="settings.chooseIcon"]')).not.toBeNull();
        expect([...right.querySelectorAll('input')].map(input => input.value)).toEqual(expect.arrayContaining(['/local', '/remote']));
        expect(container.textContent).not.toContain('connection.saveThisConnection');
    });

    it('4shared saves profile metadata without authenticating again', async () => {
        await openEditor('fourshared');
        const name = [...container.querySelectorAll('input')].find(input => input.value === 'Account')!;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(name, 'Renamed');
            name.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await clearRemotePath();
        await removeIcon();
        await act(async () => button('common.save').click());
        expect(profiles[0]).toMatchObject({ id: 'edited', name: 'Renamed', initialPath: '' });
        expect(profiles[0].customIconUrl).toBeUndefined();
        expect(onFormSaved).toHaveBeenCalledOnce();
        expect(mocks.invoke.mock.calls.map(([command]) => command)).not.toContain('fourshared_full_auth');
        expect(mocks.invoke.mock.calls.map(([command]) => command)).not.toContain('fourshared_connect');
    });

    it.each([false, true])('4shared reconnect updates the edited ID after a rename (existing tokens: %s)', async tokens => {
        const previous = mocks.invoke.getMockImplementation()!;
        mocks.invoke.mockImplementation(async (command: string, args?: any) => command === 'fourshared_has_tokens' ? tokens : previous(command, args));
        await openEditor('fourshared');
        const name = [...container.querySelectorAll('input')].find(input => input.value === 'Account')!;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(name, 'Renamed');
            name.dispatchEvent(new Event('input', { bubbles: true }));
        });
        await clearRemotePath();
        await removeIcon();
        await act(async () => button(tokens ? 'connection.fourshared.connectTo4shared' : 'connection.fourshared.signInWith4shared').click());
        expect(profiles).toHaveLength(2);
        expect(profiles[0]).toMatchObject({ id: 'edited', name: 'Renamed', initialPath: '' });
        expect(profiles[0].customIconUrl).toBeUndefined();
        expect(mocks.invoke.mock.calls.some(([command]) => command === 'fourshared_full_auth')).toBe(!tokens);
    });
});

describe('IntroHub form drafts across tab switches', () => {
    async function openHub() {
        profiles = ['First', 'Second'].map((name, index) => ({
            id: `profile-${index}`, name, host: `${index}.example.test`, port: 21,
            username: `user-${index}`, protocol: 'ftp', password: 'fixture-password',
            initialPath: `/remote-${index}`, localInitialPath: `/local-${index}`, customIconUrl: icon,
        }));
        await act(async () => root.render(React.createElement(IntroHub, {
            connectionParams: { server: '', username: '', password: '' },
            quickConnectDirs: { remoteDir: '', localDir: '' }, loading: false,
            onConnectionParamsChange: vi.fn(), onQuickConnectDirsChange: vi.fn(), onConnect: vi.fn(),
            onSavedServerConnect: vi.fn(async () => {}), onSkipToFileManager: vi.fn(),
        })));
    }
    const activeForm = () => container.querySelector<HTMLElement>('[data-form-tab-id]:not([hidden])')!;
    async function selectTab(label: string) {
        const tab = [...container.querySelectorAll('span')].find(element => element.textContent === label);
        expect(tab, `tab ${label}`).toBeDefined();
        await act(async () => tab!.click());
    }
    async function type(input: HTMLInputElement, value: string) {
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
            input.dispatchEvent(new Event('input', { bubbles: true }));
        });
    }

    it('preserves independent edit drafts and revealed passwords across two tabs and My Servers', async () => {
        await openHub();
        await act(async () => button('Edit First').click());
        const first = activeForm();
        const name = [...first.querySelectorAll('input')].find(input => input.value === 'First')!;
        await type(name, 'First draft');
        const path = [...first.querySelectorAll('input')].find(input => input.value === '/remote-0')!;
        await type(path, '/different-folder');
        await act(async () => first.querySelector<HTMLButtonElement>('button[title="settings.removeIcon"]')!.click());
        const password = first.querySelector<HTMLInputElement>('input[type="password"]')!;
        const eye = password.parentElement!.querySelector<HTMLButtonElement>('button')!;
        await act(async () => eye.click());
        expect(password.type).toBe('text');
        const within = (text: string) => [...first.querySelectorAll('button')].find(element => element.textContent?.trim() === text)!;
        await act(async () => within('aerocryptProfile.overlaysSection').click());
        const overlayPath = first.querySelector<HTMLInputElement>('input[placeholder="/"]')!;
        await type(overlayPath, '/different-folder/crypt');
        const enableCrypt = [...first.querySelectorAll('label')].find(element => element.textContent?.includes('aerocryptProfile.enable'))!;
        await act(async () => enableCrypt.click());
        await act(async () => within('aerocryptProfile.kindRclone').click());
        await act(async () => button('Edit Second').click());
        const second = activeForm();
        const secondUser = [...second.querySelectorAll('input')].find(input => input.value === 'user-1')!;
        await type(secondUser, 'second-draft');
        await selectTab('First draft');
        expect(activeForm()).toBe(first);
        expect(name.value).toBe('First draft');
        expect(path.value).toBe('/different-folder');
        expect(password.type).toBe('text');
        expect(first.querySelector('button[title="settings.removeIcon"]')).toBeNull();
        expect(overlayPath.value).toBe('/different-folder/crypt');
        expect(enableCrypt.querySelector('[role="checkbox"]')!.getAttribute('aria-checked')).toBe('true');
        expect(within('aerocryptProfile.kindRclone').className).toContain('border-emerald-500');
        await selectTab('introHub.tab.myServers');
        await selectTab('First draft');
        expect(activeForm()).toBe(first);
        expect(path.value).toBe('/different-folder');
        await selectTab('Second');
        expect(activeForm()).toBe(second);
        expect(secondUser.value).toBe('second-draft');
        expect(profiles[0].name).toBe('First');
    });

    it('keeps a new profile name, credentials and paths until the tab is closed', async () => {
        await openHub();
        await selectTab('introHub.tab.discover');
        await act(async () => button('Add FTP').click());
        const draft = activeForm();
        const name = draft.querySelector<HTMLInputElement>('input[placeholder="ftp"]')!;
        expect(name).toBeDefined();
        await type(name, 'New backup');
        const user = [...draft.querySelectorAll('input')].find(input => input.placeholder === 'connection.usernamePlaceholder')!;
        expect(user).toBeDefined();
        await type(user, 'backup-user');
        await act(async () => button('Edit First').click());
        await selectTab('New backup');
        expect(activeForm()).toBe(draft);
        expect(name.value).toBe('New backup');
        expect(user.value).toBe('backup-user');
        // Closing releases the form and all its transient secret state.
        const tab = [...container.querySelectorAll('span')].find(element => element.textContent === 'New backup')!.parentElement!;
        await act(async () => tab.querySelector<HTMLButtonElement>('button[title="common.close"]')!.click());
        expect(draft.isConnected).toBe(false);
    });
});
