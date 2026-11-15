// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { USERNAME_TEMPLATE_UNRESOLVED } from '../utils/usernameTemplate';
import { transformWithOxc } from 'vite';
import APP from '../App.tsx?raw';
import React, { act, useState } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ConnectionParams, ProviderType, ServerProfile } from '../types';

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), log: vi.fn(), t: (key: string) => key }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../i18n', () => ({
    useTranslation: () => mocks.t,
    useI18n: () => ({ t: mocks.t, language: 'en' }),
    translate: (key: string) => key,
}));
vi.mock('../hooks/useActivityLog', () => ({ useActivityLog: () => ({ log: mocks.log }) }));
// Keep the real QuickConnect controls and save handlers, without signing in.
vi.mock('./OAuthConnect', () => ({
    OAuthConnect: ({ rightColumn, onConnected, disabled }: { rightColumn: React.ReactNode; onConnected: (name: string) => void; disabled?: boolean }) =>
        React.createElement('div', { 'data-sign-in-disabled': String(!!disabled) }, rightColumn,
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
import { BACKEND_SWAP_WAIT_MS, latestSettledBackendSwapToken, swapBackend, useOAuth2, type BackendSwapDetail } from '../hooks/useOAuth2';
import { connectStartFolder } from '../utils/usernameTemplate';
import { IntroHub } from './IntroHub/IntroHub';

const icon = 'data:image/png;base64,dGVzdA==';
let profiles: ServerProfile[];
let container: HTMLDivElement;
let root: Root;
let onFormSaved: ReturnType<typeof vi.fn<() => void>>;
let onConnectSpy: ReturnType<typeof vi.fn<(overrideParams?: ConnectionParams) => void>>;
let abandoned: number;
const countAbandoned = () => { abandoned += 1; };

beforeEach(() => {
    vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
    localStorage.clear();
    mocks.invoke.mockReset();
    mocks.log.mockReset();
    onFormSaved = vi.fn<() => void>();
    onConnectSpy = vi.fn<(overrideParams?: ConnectionParams) => void>();
    abandoned = 0;
    window.addEventListener('aeroftp-sign-in-abandoned', countAbandoned);
    mocks.invoke.mockImplementation(async (command: string, args?: { profiles?: ServerProfile[]; expectedUserId?: number }) => {
        if (command === 'user_partitions_load_active_server_profiles') return structuredClone(profiles);
        if (command === 'user_partitions_load_active_server_profiles_scoped') return { userId: 1, profiles: structuredClone(profiles) };
        if (command === 'user_partitions_save_active_server_profiles') {
            // Every edit Save writes back to the account it read.
            expect(args!.expectedUserId).toBe(1);
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
    window.removeEventListener('aeroftp-sign-in-abandoned', countAbandoned);
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
            onConnect: onConnectSpy, editingProfile: profile, onFormSaved,
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

    it.each(['config_oauth_clients', 'config_aeroftp_oauth_settings', 'fourshared_oauth_settings'])('4shared hydrates %s and reconnects with the matching app pair', async account => {
        const previous = mocks.invoke.getMockImplementation()!;
        mocks.invoke.mockImplementation(async (command: string, args?: any) => {
            if (command === 'fourshared_has_tokens') return true;
            if (command === 'get_credential' && args.account !== 'server_edited') {
                if (args.account !== account) throw new Error(`Failed to get credential: Credential not found: ${args.account}`);
                return JSON.stringify(account === 'fourshared_oauth_settings'
                    ? { consumer_key: 'legacy-key', consumer_secret: 'legacy-secret' }
                    : { fourshared: { clientId: 'legacy-key', clientSecret: 'legacy-secret' } });
            }
            return previous(command, args);
        });
        await openEditor('fourshared');
        const key = container.querySelector<HTMLInputElement>('input[placeholder="connection.fourshared.enterConsumerKey"]')!;
        const secret = container.querySelector<HTMLInputElement>('input[placeholder="connection.fourshared.enterConsumerSecret"]')!;
        expect(key.value).toBe('legacy-key');
        expect(secret.value).toBe('legacy-secret');
        expect(secret.type).toBe('password');
        const connect = button('connection.fourshared.connectTo4shared');
        expect(connect.disabled).toBe(false);
        await act(async () => connect.click());
        expect(mocks.invoke).toHaveBeenCalledWith('fourshared_connect', { params: { consumer_key: 'legacy-key', consumer_secret: 'legacy-secret' } });
    });

    it('4shared retains existing tokens after an app credential read fails', async () => {
        const previous = mocks.invoke.getMockImplementation()!;
        mocks.invoke.mockImplementation(async (command: string, args?: any) => {
            if (command === 'fourshared_has_tokens') return true;
            if (command === 'get_credential' && args.account !== 'server_edited') throw new Error('Vault read failed');
            return previous(command, args);
        });
        await openEditor('fourshared');
        expect(container.textContent).toContain('connection.oauthKeysReadFailed');
        expect(container.textContent).toContain('connection.fourshared.previouslyAuthenticated');
        const key = container.querySelector<HTMLInputElement>('input[placeholder="connection.fourshared.enterConsumerKey"]')!;
        const secret = container.querySelector<HTMLInputElement>('input[placeholder="connection.fourshared.enterConsumerSecret"]')!;
        await act(async () => {
            for (const [input, value] of [[key, 'manual-key'], [secret, 'manual-secret']] as const) {
                Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
                input.dispatchEvent(new Event('input', { bubbles: true }));
            }
        });
        const connect = button('connection.fourshared.connectTo4shared');
        expect(connect.disabled).toBe(false);
        await act(async () => connect.click());
        expect(mocks.invoke).toHaveBeenCalledWith('fourshared_connect', { params: { consumer_key: 'manual-key', consumer_secret: 'manual-secret' } });
        expect(mocks.invoke.mock.calls.map(([command]) => command)).not.toContain('fourshared_full_auth');
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
            vaultReady: true,
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

describe('Puter WebDAV credential guidance and Advanced Server override', () => {
    async function renderPuterNewConnection() {
        function Harness() {
            const [params, setParams] = useState<ConnectionParams>({
                protocol: 'webdav',
                providerId: 'puter-webdav',
                server: '',
                username: '',
                password: '',
            });
            const [dirs, setDirs] = useState({ remoteDir: '', localDir: '' });
            return React.createElement(ConnectionScreen, {
                connectionParams: params,
                quickConnectDirs: dirs,
                loading: false,
                onConnectionParamsChange: setParams,
                onQuickConnectDirsChange: setDirs,
                onConnect: vi.fn(),
            });
        }
        await act(async () => root.render(React.createElement(Harness)));
    }

    it('renders username and password helpText for puter-webdav', async () => {
        await renderPuterNewConnection();
        const text = container.textContent || '';
        expect(text).toContain('Token auth: use -token as the username (literal)');
        expect(text).toMatch(/appended verification codes expire and may stop authenticating/i);
        expect(text).toContain('-token');
    });

    it('allows unlocking and overriding Server under Advanced Options for token auth', async () => {
        await renderPuterNewConnection();
        const toggle = button('protocol.advanced');
        await act(async () => toggle.click());

        const serverInput = container.querySelector<HTMLInputElement>('input[placeholder="https://dav.puter.com/{username}"]')!;
        expect(serverInput).toBeDefined();
        expect(serverInput.disabled).toBe(true);

        const editBtn = button('common.edit');
        await act(async () => editBtn.click());

        const unlockBtn = button('protocol.advancedUnlock');
        await act(async () => unlockBtn.click());

        expect(serverInput.disabled).toBe(false);
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(serverInput, 'https://dav.puter.com/myuser');
            serverInput.dispatchEvent(new Event('input', { bubbles: true }));
        });
        expect(serverInput.value).toBe('https://dav.puter.com/myuser');
    });
});

describe('LifetimeCloud WebDAV credential helpText and overlay defaults', () => {
    it('renders username and password helpText for lifetimecloud and keeps crypt overlay opt-in', async () => {
        function Harness() {
            const [params, setParams] = useState<ConnectionParams>({
                protocol: 'webdav',
                providerId: 'lifetimecloud',
                server: '',
                username: '',
                password: '',
            });
            const [dirs, setDirs] = useState({ remoteDir: '', localDir: '' });
            return React.createElement(ConnectionScreen, {
                connectionParams: params,
                quickConnectDirs: dirs,
                loading: false,
                onConnectionParamsChange: setParams,
                onQuickConnectDirsChange: setDirs,
                onConnect: vi.fn(),
            });
        }
        await act(async () => root.render(React.createElement(Harness)));
        const text = container.textContent || '';
        expect(text).toContain('Used to fill https://lifetimecloud.me/dav/{username}/ automatically');
        expect(text).toContain('Profile > Enable WebDAV > Generate WebDAV token (not your LifetimeCloud account password)');
        expect(container.querySelector('input[placeholder="connection.passwordPlaceholder"]')).not.toBeNull();
    });
});

describe('A crypt overlay folder the user name cannot fill', () => {
    it('shows the refusal and blocks Save until the user name fills it', async () => {
        function Harness() {
            const [params, setParams] = useState<ConnectionParams>({
                protocol: 'webdav',
                server: 'https://cloud.example.test',
                username: '',
                password: 'p',
            });
            const [dirs, setDirs] = useState({ remoteDir: '/remote.php/dav/files/{username}/', localDir: '' });
            return React.createElement(ConnectionScreen, {
                connectionParams: params,
                quickConnectDirs: dirs,
                loading: false,
                onConnectionParamsChange: setParams,
                onQuickConnectDirsChange: setDirs,
                onConnect: vi.fn(),
            });
        }
        await act(async () => root.render(React.createElement(Harness)));
        const buttons = () => [...container.querySelectorAll('button')];
        await act(async () => buttons().find(b => b.textContent?.trim() === 'aerocryptProfile.overlaysSection')!.click());
        const enable = [...container.querySelectorAll('label')].find(l => l.textContent?.includes('aerocryptProfile.enable'))!;
        await act(async () => enable.click());
        // Blank user name: the overlay would anchor at a folder named after the
        // placeholder, so the form shows the refusal.
        expect(container.textContent).toContain(USERNAME_TEMPLATE_UNRESOLVED);
        // A user name that fills the folder clears it.
        const user = container.querySelector<HTMLInputElement>('input[placeholder="connection.usernamePlaceholder"]')
            ?? [...container.querySelectorAll<HTMLInputElement>('input[type="text"]')].find(i => i.value === '')!;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(user, 'alice');
            user.dispatchEvent(new Event('input', { bubbles: true }));
        });
        expect(container.textContent).not.toContain(USERNAME_TEMPLATE_UNRESOLVED);
    });

    it('checks a relative Overlays Path on the scope it saves, under the templated Remote Path', async () => {
        function Harness() {
            const [params, setParams] = useState<ConnectionParams>({
                protocol: 'webdav',
                server: 'https://cloud.example.test',
                // A backslash cannot fill a folder path: the scope keeps the placeholder.
                username: 'DOMAIN\\alice',
                password: 'p',
            });
            const [dirs, setDirs] = useState({ remoteDir: '/files/{username}/', localDir: '' });
            return React.createElement(ConnectionScreen, {
                connectionParams: params,
                quickConnectDirs: dirs,
                loading: false,
                onConnectionParamsChange: setParams,
                onQuickConnectDirsChange: setDirs,
                onConnect: vi.fn(),
            });
        }
        await act(async () => root.render(React.createElement(Harness)));
        const buttons = () => [...container.querySelectorAll('button')];
        await act(async () => buttons().find(b => b.textContent?.trim() === 'aerocryptProfile.overlaysSection')!.click());
        const enable = [...container.querySelectorAll('label')].find(l => l.textContent?.includes('aerocryptProfile.enable'))!;
        await act(async () => enable.click());
        const overlayPath = container.querySelector<HTMLInputElement>('input[placeholder="/"]')!;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(overlayPath, 'crypt');
            overlayPath.dispatchEvent(new Event('input', { bubbles: true }));
        });
        // `crypt` alone holds no placeholder, but it is saved as
        // `/files/{username}/crypt`, which the backend refuses to unlock.
        expect(overlayPath.value).toBe('crypt');
        expect(container.textContent).toContain(USERNAME_TEMPLATE_UNRESOLVED);
        const save = () => [...container.querySelectorAll('button')].find(b => ['common.save', 'common.connect'].includes(b.textContent?.trim() ?? ''))!;
        expect(save().disabled).toBe(true);
        // A user name that fills the folder clears the refusal and enables Save.
        const user = [...container.querySelectorAll<HTMLInputElement>('input')].find(i => i.value === 'DOMAIN\\alice')!;
        await act(async () => {
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(user, 'alice');
            user.dispatchEvent(new Event('input', { bubbles: true }));
        });
        expect(container.textContent).not.toContain(USERNAME_TEMPLATE_UNRESOLVED);
        expect(save().disabled).toBe(false);
    });

    it('lets an OAuth sign-in write nothing while the overlay folder is unresolved', async () => {
        // An OAuth profile has no user name, so a `{username}` folder cannot be filled.
        profiles = [{
            id: 'edited', name: 'Account', host: '', port: 443, username: '',
            protocol: 'googledrive', initialPath: '/vault/{username}', localInitialPath: '/local',
        }];
        await renderEditor();
        await act(async () => button('aerocryptProfile.overlaysSection').click());
        const enable = [...container.querySelectorAll('label')].find(l => l.textContent?.includes('aerocryptProfile.enable'))!;
        await act(async () => enable.click());
        expect(container.textContent).toContain(USERNAME_TEMPLATE_UNRESOLVED);
        const before = structuredClone(profiles);
        mocks.invoke.mockClear();
        await act(async () => button('Test OAuth sign-in').click());
        const writes = mocks.invoke.mock.calls.map(([command]) => command)
            .filter(command => /save_active_server_profiles|store_credential|credential_store|aerocrypt/.test(String(command)));
        expect(writes).toEqual([]);
        expect(profiles).toEqual(before);
        // The sign-in is offered disabled, and the swapped backend is reported
        // so App puts the live tab aside.
        expect(container.querySelector('[data-sign-in-disabled]')!.getAttribute('data-sign-in-disabled')).toBe('true');
        expect(onConnectSpy).not.toHaveBeenCalled();
        expect(abandoned).toBe(1);
    });
});

describe('A sign-in that swapped the backend', () => {
    // OAuthConnect and FourSharedConnect swap the backend before the form
    // decides: every exit either hands the connect to App, which parks the
    // live tab first, or reports the abandoned sign-in so App parks it.
    it('hands an OAuth sign-in to App and reports nothing', async () => {
        await openEditor('googledrive');
        await act(async () => button('Test OAuth sign-in').click());
        expect(onConnectSpy).toHaveBeenCalledOnce();
        expect(abandoned).toBe(0);
    });

    it('reports an OAuth sign-in whose profile read fails', async () => {
        await openEditor('googledrive');
        const previous = mocks.invoke.getMockImplementation()!;
        mocks.invoke.mockImplementation(async (command: string, args?: any) => {
            if (command === 'user_partitions_load_active_server_profiles_scoped') throw new Error('STORE_NOT_READY');
            return previous(command, args);
        });
        await act(async () => button('Test OAuth sign-in').click());
        expect(onConnectSpy).not.toHaveBeenCalled();
        expect(abandoned).toBe(1);
    });

    it('reports a 4shared connect whose profile read fails', async () => {
        const previous = mocks.invoke.getMockImplementation()!;
        mocks.invoke.mockImplementation(async (command: string, args?: any) => command === 'fourshared_has_tokens' ? true : previous(command, args));
        await openEditor('fourshared');
        const withTokens = mocks.invoke.getMockImplementation()!;
        mocks.invoke.mockImplementation(async (command: string, args?: any) => {
            if (command === 'user_partitions_load_active_server_profiles_scoped') throw new Error('STORE_NOT_READY');
            return withTokens(command, args);
        });
        await act(async () => button('connection.fourshared.connectTo4shared').click());
        expect(mocks.invoke.mock.calls.some(([command]) => command === 'fourshared_connect')).toBe(true);
        expect(onConnectSpy).not.toHaveBeenCalled();
        expect(abandoned).toBe(1);
    });

    it('tells App before the 4shared connect swaps the backend, and reports that swap', async () => {
        const previous = mocks.invoke.getMockImplementation()!;
        mocks.invoke.mockImplementation(async (command: string, args?: any) => command === 'fourshared_has_tokens' ? true : previous(command, args));
        await openEditor('fourshared');
        const withTokens = mocks.invoke.getMockImplementation()!;
        const order: string[] = [];
        mocks.invoke.mockImplementation(async (command: string, args?: any) => {
            if (command === 'fourshared_connect') order.push(command);
            if (command === 'user_partitions_load_active_server_profiles_scoped') throw new Error('STORE_NOT_READY');
            return withTokens(command, args);
        });
        const { swaps, reports, stop } = listenToSwaps(order);
        try {
            await act(async () => button('connection.fourshared.connectTo4shared').click());
        } finally { stop(); }
        expect(order).toEqual(['swap:start', 'fourshared_connect', 'swap:settled']);
        expect(swaps[0].token).toBe(swaps[1].token);
        expect(reports).toEqual([swaps[0].token]);
    });
});

/** Records the sign-in swap events and the form's abandoned reports. */
function listenToSwaps(order: string[]) {
    const swaps: BackendSwapDetail[] = [];
    const reports: (number | undefined)[] = [];
    const swap = (event: Event) => {
        const detail = (event as CustomEvent<BackendSwapDetail>).detail;
        swaps.push(detail);
        order.push(`swap:${detail.phase}`);
    };
    const report = (event: Event) => { reports.push((event as CustomEvent<{ token?: number }>).detail?.token); };
    window.addEventListener('aeroftp-backend-swap', swap);
    window.addEventListener('aeroftp-sign-in-abandoned', report);
    return {
        swaps, reports,
        stop: () => {
            window.removeEventListener('aeroftp-backend-swap', swap);
            window.removeEventListener('aeroftp-sign-in-abandoned', report);
        },
    };
}

describe('The OAuth sign-in swap (useOAuth2.connect)', () => {
    async function connectOnce(result: () => Promise<unknown>) {
        const order: string[] = [];
        mocks.invoke.mockImplementation(async (command: string) => {
            order.push(command);
            return result();
        });
        let connect!: ReturnType<typeof useOAuth2>['connect'];
        function Hook() { connect = useOAuth2().connect; return null; }
        await act(async () => root.render(React.createElement(Hook)));
        const { swaps, stop } = listenToSwaps(order);
        try {
            await act(async () => { await connect({ provider: 'google_drive', client_id: 'id', client_secret: 'secret' }).catch(() => undefined); });
        } finally { stop(); }
        return { order, swaps };
    }

    it('tells App before oauth2_connect swaps the backend, and when it ends', async () => {
        const { order, swaps } = await connectOnce(async () => ({ display_name: 'Account', account_email: null }));
        expect(order).toEqual(['swap:start', 'oauth2_connect', 'swap:settled']);
        expect(swaps[0].token).toBe(swaps[1].token);
        expect(swaps[0].token).toBe(latestSettledBackendSwapToken());
    });

    it('says the swap ended when the connect fails', async () => {
        const { order } = await connectOnce(async () => { throw new Error('refused'); });
        expect(order).toEqual(['swap:start', 'oauth2_connect', 'swap:settled']);
    });
});

/** Runs App's sign-in swap listener (the `backendSwapRef` effect) on stubs. */
async function mountSwapListener() {
    const start = APP.indexOf('  const backendSwapRef = useRef<');
    const end = APP.indexOf('  // FTP operations', start);
    expect(start, 'the swap listener in App.tsx').toBeGreaterThan(-1);
    const { code } = await transformWithOxc(APP.slice(start, end), 'backend-swap.ts');
    const ctx = {
        useRef: <T,>(current: T) => ({ current }),
        useEffect: (effect: () => () => void) => { cleanups.push(effect()); },
        useLatestCallback: <F,>(fn: F) => fn,
        notify: { error: vi.fn() },
        t: (key: string) => key,
        takeOverActiveSession: vi.fn(),
        parkActiveSession: vi.fn(),
        remoteConnectPhaseVersionRef: { current: 7 },
        backendUncertainRef: { current: false },
        replacingBackend: () => () => undefined,
    };
    new Function(...Object.keys(ctx), code)(...Object.values(ctx));
    return ctx;
}
const cleanups: (() => void)[] = [];
const abandon = (token: number) => window.dispatchEvent(new CustomEvent('aeroftp-sign-in-abandoned', { detail: { token } }));
/** The form's hand-off request for a sign-in: true when App accepts it. */
const handOffFor = (token: number) => {
    const request = { token, accepted: true };
    window.dispatchEvent(new CustomEvent('aeroftp-sign-in-handoff', { detail: request }));
    return request.accepted;
};
/** The events of a sign-in swap, sent as `swapBackend` sends them, in an
 *  order the queue no longer lets happen: the token rules hold anyway. */
let eventToken = 9000;
const swapEvents = () => {
    const token = ++eventToken;
    const tell = (phase: 'start' | 'settled') =>
        window.dispatchEvent(new CustomEvent<BackendSwapDetail>('aeroftp-backend-swap', { detail: { token, phase } }));
    tell('start');
    return { token, settle: () => tell('settled') };
};
/** A sign-in swap the test settles when it wants: its token, and the settle. */
const pendingSwap = () => {
    let settle: (() => void) | undefined;
    const done = swapBackend(() => new Promise<void>(resolve => { settle = resolve; }));
    return {
        settle: async () => {
            // Its invoke runs in its turn, a few microtasks after the start.
            for (let i = 0; i < 20 && !settle; i++) await Promise.resolve();
            settle!();
            await done;
        },
    };
};

describe('App around a sign-in swap', () => {
    afterEach(() => { cleanups.splice(0).forEach(cleanup => cleanup()); });

    it('puts the live tab aside before the backend is swapped', async () => {
        const app = await mountSwapListener();
        let parkedFirst = false;
        await swapBackend(async () => { parkedFirst = app.takeOverActiveSession.mock.calls.length === 1; });
        expect(parkedFirst).toBe(true);
        // And again when it settles: a tab connected meanwhile lost its backend.
        expect(app.takeOverActiveSession).toHaveBeenCalledTimes(2);
    });

    it('puts aside a tab opened while the abandoned swap ran', async () => {
        const app = await mountSwapListener();
        // A tab switch or a connect began while oauth2_connect ran: the swap
        // replaced the backend it had opened.
        await swapBackend(async () => { app.remoteConnectPhaseVersionRef.current++; });
        abandon(latestSettledBackendSwapToken());
        expect(app.parkActiveSession).toHaveBeenCalledOnce();
    });

    it('leaves alone a tab opened after the swap, when the report comes late', async () => {
        const app = await mountSwapListener();
        await swapBackend(async () => undefined);
        app.remoteConnectPhaseVersionRef.current++;
        abandon(latestSettledBackendSwapToken());
        expect(app.parkActiveSession).not.toHaveBeenCalled();
    });

    it('leaves the tab alone on a report about a superseded sign-in', async () => {
        const app = await mountSwapListener();
        await swapBackend(async () => undefined);
        const older = latestSettledBackendSwapToken();
        await swapBackend(async () => undefined);
        abandon(older);
        expect(app.parkActiveSession).not.toHaveBeenCalled();
        abandon(latestSettledBackendSwapToken());
        expect(app.parkActiveSession).toHaveBeenCalledOnce();
    });
});

describe('Overlapping sign-ins', () => {
    afterEach(() => { cleanups.splice(0).forEach(cleanup => cleanup()); });

    it('puts aside a tab opened while two sign-ins wait, when the first one settles', async () => {
        const app = await mountSwapListener();
        const a = swapEvents();
        const b = swapEvents();
        // Tab C connects while both wait: its connect advances the phase.
        app.remoteConnectPhaseVersionRef.current++;
        const before = app.takeOverActiveSession.mock.calls.length;
        a.settle();
        // A's settle replaced C's backend: C is put aside.
        expect(app.takeOverActiveSession.mock.calls.length).toBe(before + 1);
        // A's form fails its profile save: its report names A, which owns the backend.
        abandon(a.token);
        expect(app.parkActiveSession).toHaveBeenCalledOnce();
        b.settle();
    });

    it('puts aside the later sign-in\'s tab when the earlier one settles after it', async () => {
        const app = await mountSwapListener();
        const a = swapEvents();
        const b = swapEvents();
        b.settle();
        expect(handOffFor(b.token)).toBe(true);
        // App's connect of B's session.
        app.remoteConnectPhaseVersionRef.current++;
        const before = app.takeOverActiveSession.mock.calls.length;
        a.settle();
        expect(app.takeOverActiveSession.mock.calls.length).toBe(before + 1);
        // A now owns the backend: its own hand-off is the right one.
        expect(handOffFor(a.token)).toBe(true);
    });

    it('refuses the hand-off of a sign-in that another one overtook', async () => {
        const app = await mountSwapListener();
        const a = swapEvents();
        const b = swapEvents();
        a.settle();
        b.settle();
        expect(handOffFor(a.token)).toBe(false);
        expect(app.notify.error).toHaveBeenCalledWith('connection.connectionFailed', 'toast.remoteTargetChanged');
        // Its abandoned report is left alone; B hands off.
        abandon(a.token);
        expect(app.parkActiveSession).not.toHaveBeenCalled();
        expect(handOffFor(b.token)).toBe(true);
    });

    it('refuses a hand-off after a connect took the backend since the settle', async () => {
        const app = await mountSwapListener();
        await swapBackend(async () => undefined);
        app.remoteConnectPhaseVersionRef.current++;
        expect(handOffFor(latestSettledBackendSwapToken())).toBe(false);
    });

    it('does not connect a form sign-in whose hand-off App refuses', async () => {
        const app = await mountSwapListener();
        await openEditor('googledrive');
        await swapBackend(async () => undefined);
        app.remoteConnectPhaseVersionRef.current++;
        await act(async () => button('Test OAuth sign-in').click());
        expect(onConnectSpy).not.toHaveBeenCalled();
        expect(abandoned).toBe(1);
    });
});

/** App's backend confirmation and replacement accounting, its sign-in swap
 *  listener, `backendServesLiveTab`, `remoteActionGuard`, `queuedTargetProblem`
 *  and `queuedTargetGuard`, run together on stubs. */
async function liveTabChecks() {
    const between = (from: string, to: string) => {
        const start = APP.indexOf(from);
        expect(start, from).toBeGreaterThan(-1);
        return APP.slice(start, APP.indexOf(to, start));
    };
    const source = [
        between('  const confirmBackendConnection = () => {', '  const remoteConnectPhaseVersionRef = useRef(0);'),
        between('  const backendSwapRef = useRef<', '  // FTP operations'),
        between('  const backendServesLiveTab = () =>', '  /** Fails the rows from'),
    ].join('\n');
    const { code } = await transformWithOxc(source, 'live-tab.ts');
    const params = { protocol: 'ftp', server: 'example.test', username: 'u', password: '' };
    const target = { params, profileId: null, oauthLogins: 0, overlay: null };
    class QueuedTargetChangedError extends Error {}
    const ctx = {
        useRef: <T,>(current: T) => ({ current }),
        useEffect: (effect: () => () => void) => { cleanups.push(effect()); },
        useLatestCallback: <F,>(fn: F) => fn,
        notify: { error: vi.fn() },
        // The takeover only schedules the park: isConnected still reads true.
        takeOverActiveSession: vi.fn(),
        parkActiveSession: vi.fn(),
        remoteConnectPhaseVersionRef: { current: 7 },
        remoteConnectPhaseRef: { current: false },
        backendTeardownsRef: { current: 0 },
        backendUncertainRef: { current: false },
        backendConfirmationsRef: { current: 0 },
        backendReplacementSeqRef: { current: 0 },
        isConnected: true,
        isSameConnection: () => true,
        liveConnectionParams: params,
        accountKeysDiffer: () => false,
        liveProfileId: () => null,
        isOAuthProvider: () => false,
        isFourSharedProvider: () => false,
        liveOverlay: () => null,
        snapshotQueuedTarget: () => target,
        QueuedTargetChangedError,
        t: (key: string) => key,
    };
    const fns = new Function(...Object.keys(ctx), `${code}; return { confirmBackendConnection, replacingBackend, remoteActionGuard, queuedTargetProblem, queuedTargetGuard };`)(...Object.values(ctx));
    return { ...fns, target, ctx };
}

describe('The live tab while the backend is being replaced', () => {
    afterEach(() => { cleanups.splice(0).forEach(cleanup => cleanup()); });

    it('refuses the queue and a direct action at once, before React commits the park', async () => {
        const live = await liveTabChecks();
        expect(live.queuedTargetProblem(live.target)).toBeNull();
        expect(() => live.remoteActionGuard()()).not.toThrow();
        const swap = pendingSwap();
        expect(live.queuedTargetProblem(live.target)).toBe('transfer.queuedForAnotherServer');
        expect(() => live.remoteActionGuard()()).toThrow('toast.remoteTargetChanged');
        // The batch's own guard, run before each write while the form still
        // reads the profile to save (the swap settled, the hand-off pending).
        await swap.settle();
        expect(() => live.queuedTargetGuard(live.target)()).toThrow('transfer.queuedForAnotherServer');
        // A connect that confirms a tab binds it again.
        live.confirmBackendConnection();
        expect(live.queuedTargetProblem(live.target)).toBeNull();
    });

    it('keeps the tab another sign-in confirmed unbound while an earlier one is in flight', async () => {
        const live = await liveTabChecks();
        const a = swapEvents();
        const b = swapEvents();
        b.settle();
        // B hands off and its connect confirms B's tab.
        live.confirmBackendConnection();
        // A is still in flight: the backend may hold A's provider already.
        expect(live.queuedTargetProblem(live.target)).toBe('transfer.queuedForAnotherServer');
        expect(() => live.remoteActionGuard()()).toThrow('toast.remoteTargetChanged');
        a.settle();
        // A replaced the backend under B's tab: unbound until a connect confirms.
        expect(live.queuedTargetProblem(live.target)).toBe('transfer.queuedForAnotherServer');
        expect(live.ctx.backendTeardownsRef.current).toBe(0);
    });

    it('does the same when the earlier replacement is a resume connect', async () => {
        const live = await liveTabChecks();
        // The resume of a restored transfer starts its OAuth connect.
        const endResume = live.replacingBackend();
        // Another connect confirms its tab meanwhile.
        live.confirmBackendConnection();
        expect(live.queuedTargetProblem(live.target)).toBe('transfer.queuedForAnotherServer');
        endResume();
        expect(live.queuedTargetProblem(live.target)).toBe('transfer.queuedForAnotherServer');
        // A replacement nobody confirmed around leaves nothing behind.
        live.confirmBackendConnection();
        const endAlone = live.replacingBackend();
        endAlone();
        endAlone();
        expect(live.ctx.backendTeardownsRef.current).toBe(0);
        expect(live.queuedTargetProblem(live.target)).toBeNull();
    });
});

describe('An AeroCloud open after an abandoned sign-in', () => {
    afterEach(() => { cleanups.splice(0).forEach(cleanup => cleanup()); });

    /** App's AeroCloud connect closure, run on stubs, confirming through the
     *  same accounting as `live`; its connect replaces the backend as App's
     *  `runConnect` does. */
    async function aeroCloudConnect(live: Awaited<ReturnType<typeof liveTabChecks>>, isProvider: boolean) {
        const start = APP.indexOf('const connectToCloudServer = async');
        const end = APP.indexOf('// Helper: navigate to remote folder', start);
        expect(start, 'connectToCloudServer in App.tsx').toBeGreaterThan(-1);
        const { code } = await transformWithOxc(APP.slice(start, end), 'aerocloud-connect.ts');
        const runConnect = vi.fn(async () => { live.replacingBackend()(); });
        const ctx = {
            confirmBackendConnection: live.confirmBackendConnection,
            invoke: vi.fn(async () => undefined),
            isProvider,
            protocol: isProvider ? 'webdav' : 'ftp',
            cloudServer: { id: 'cloud', providerId: null, host: 'dav.example.test', port: 443, username: 'u', options: {} },
            cloudServerString: 'ftp.example.test:21',
            resolveProfileS3Location: () => ({ endpoint: null, signingRegion: undefined }),
            cloudConfig: { remote_folder: '/sync' },
            cloudPassword: 'dummy-password',
            cloudFilenApiKey: null,
            runConnect,
            scpFallbackConnectParam: () => ({}),
            loadedCertificateCheck: () => true,
            loadedOption: () => undefined,
            loadedOptionOn: () => false,
            isSshProtocol: () => false,
            checkSftpHostKey: vi.fn(async () => true),
            CONNECT_CANCELLED_MARKER: 'cancelled',
        };
        const connect = new Function(...Object.keys(ctx), `${code}; return connectToCloudServer;`)(...Object.values(ctx));
        await connect();
        expect(runConnect).toHaveBeenCalledOnce();
    }

    it.each([['a provider', true], ['an FTP server', false]])('lets a remote action through once it has connected %s', async (_label, isProvider) => {
        const live = await liveTabChecks();
        // A sign-in swapped the backend and was then abandoned.
        await swapBackend(async () => { live.ctx.remoteConnectPhaseVersionRef.current++; });
        abandon(latestSettledBackendSwapToken());
        expect(live.ctx.parkActiveSession).toHaveBeenCalledOnce();
        expect(() => live.remoteActionGuard()()).toThrow('toast.remoteTargetChanged');
        await aeroCloudConnect(live, isProvider);
        expect(() => live.remoteActionGuard()()).not.toThrow();
        expect(live.queuedTargetProblem(live.target)).toBeNull();
    });
});

describe('One sign-in swap at a time', () => {
    afterEach(() => { vi.useRealTimers(); });

    it('starts a second swap only once the first has settled', async () => {
        const settled: number[] = [];
        const record = (event: Event) => {
            const { token, phase } = (event as CustomEvent<BackendSwapDetail>).detail;
            if (phase === 'settled') settled.push(token);
        };
        window.addEventListener('aeroftp-backend-swap', record);
        try {
            const first = pendingSwap();
            const run = vi.fn(async () => undefined);
            const second = swapBackend(run);
            for (let i = 0; i < 5; i++) await Promise.resolve();
            expect(run).not.toHaveBeenCalled();
            await first.settle();
            await second;
            expect(run).toHaveBeenCalledOnce();
            // Installed in settle order: the one that settled last owns the backend.
            expect(latestSettledBackendSwapToken()).toBe(settled[settled.length - 1]);
        } finally {
            window.removeEventListener('aeroftp-backend-swap', record);
        }
    });

    it('stops waiting for a swap that never settles, and keeps the next ones behind it', async () => {
        vi.useFakeTimers();
        const ownerBefore = latestSettledBackendSwapToken();
        // A is still running; B and C queue behind it.
        let settleA!: () => void;
        const a = swapBackend(() => new Promise<void>((resolve) => { settleA = resolve; }));
        const runB = vi.fn(async () => undefined);
        const b = swapBackend(runB).catch((error: Error) => error.message);
        await vi.advanceTimersByTimeAsync(BACKEND_SWAP_WAIT_MS);
        // B fails as a timed-out connect, replacing and owning nothing.
        expect(await b).toBe('toast.connectTimedOut');
        expect(runB).not.toHaveBeenCalled();
        expect(latestSettledBackendSwapToken()).toBe(ownerBefore);
        // C, queued after B gave up, still waits for A, which is running.
        const runC = vi.fn(async () => undefined);
        const c = swapBackend(runC);
        await vi.advanceTimersByTimeAsync(1000);
        expect(runC).not.toHaveBeenCalled();
        // Only once A settles does C start.
        settleA();
        await a;
        await c;
        expect(runC).toHaveBeenCalledOnce();
    });

    it('lets the ones behind go when a swap that ran fails at once', async () => {
        const failing = swapBackend<void>(() => { throw new Error('refused'); }).catch(() => 'failed');
        const next = vi.fn(async () => undefined);
        expect(await failing).toBe('failed');
        await swapBackend(next);
        expect(next).toHaveBeenCalledOnce();
    });
});

describe('A connect whose backend another invoke replaced meanwhile', () => {
    /** App's createSession, run on stubs; `replaceDuringRead` ends another
     *  replacement while it reads the saved profiles. */
    async function createSessionRun(replaceDuringRead: boolean) {
        const start = APP.indexOf('  const createSession = async (');
        const end = APP.indexOf('\n  };\n', start);
        expect(start, 'createSession in App.tsx').toBeGreaterThan(-1);
        const { code } = await transformWithOxc(APP.slice(start, end + '\n  };'.length), 'create-session.ts');
        const seq = { current: 4 };
        const ctx: Record<string, unknown> = {
            backendReplacementSeqRef: seq,
            loadSavedServerProfiles: async () => {
                if (replaceDuringRead) seq.current += 1;
                return [];
            },
            confirmBackendConnection: vi.fn(),
            notify: { error: vi.fn() },
            t: (key: string) => key,
            remoteFiles: [],
            localFiles: [],
        };
        for (const setter of new Set(code.match(/\bset[A-Z]\w+/g))) ctx[setter] = vi.fn();
        const createSession = new Function(...Object.keys(ctx), `${code}; return createSession;`)(...Object.values(ctx));
        // The caller read the sequence as its own connect returned.
        await createSession('Server', { protocol: 'sftp', server: 'h', username: 'u', password: '' }, '/', '/home', [], [], undefined, 4);
        const added = (ctx.setSessions as ReturnType<typeof vi.fn>).mock.calls[0][0]([]) as { status: string }[];
        return { ctx, added };
    }

    it('keeps the tab cached and confirms nothing when a replacement ended during the profile read', async () => {
        const { ctx, added } = await createSessionRun(true);
        expect(ctx.confirmBackendConnection).not.toHaveBeenCalled();
        expect(ctx.setActiveSessionId).not.toHaveBeenCalled();
        expect(ctx.setIsConnected).toHaveBeenCalledWith(false);
        expect(added[0].status).toBe('cached');
        expect((ctx.notify as { error: ReturnType<typeof vi.fn> }).error).toHaveBeenCalledWith('connection.connectionFailed', 'toast.remoteTargetChanged');
    });

    it('confirms and makes the tab live when nothing replaced the backend', async () => {
        const { ctx, added } = await createSessionRun(false);
        expect(ctx.confirmBackendConnection).toHaveBeenCalledOnce();
        expect(ctx.setActiveSessionId).toHaveBeenCalled();
        expect(added[0].status).toBe('connected');
    });

    it('checks the sequence in every connect path that awaits before it confirms', () => {
        // Every createSession call of a connect flow passes the sequence read
        // right after that flow's own connect (or, for an OAuth hand-off, when
        // the branch starts, after its sign-in's swap returned).
        const calls = [...APP.matchAll(/createSession\(/g)].map(m => m.index!).filter(i => !APP.slice(i - 30, i).includes('const '));
        expect(calls.length).toBe(6);
        for (const call of calls) {
            // The argument list, by parenthesis depth.
            const open = APP.indexOf('(', call);
            let depth = 0;
            let close = open;
            for (let i = open; i < APP.length; i++) {
                if (APP[i] === '(') depth++;
                else if (APP[i] === ')' && --depth === 0) { close = i; break; }
            }
            const args = APP.slice(open, close + 1);
            expect(args, APP.slice(call, call + 80)).toMatch(/\w+ReplacedAt,?\s*\)$/);
        }
        for (const read of ['providerReplacedAt', 'ftpReplacedAt', 'savedProviderReplacedAt', 'savedFtpReplacedAt']) {
            // Read on the statement right after the flow's own connect.
            const at = APP.indexOf(`const ${read} = backendReplacementSeqRef.current;`);
            expect(at, read).toBeGreaterThan(-1);
            const before = APP.slice(APP.lastIndexOf('await ', at), at);
            expect(before, read).toMatch(/^await connectUnderRow\([^]*runConnect\('(provider_connect|connect_ftp)'[^]*\);\n\s+$/);
        }
        // A tab switch reads it after its own probe or connect and refuses
        // before confirming.
        const sw = APP.slice(APP.indexOf('  const switchSession = '));
        expect(sw.match(/switchReplacedAt = backendReplacementSeqRef\.current;/g)!.length).toBeGreaterThanOrEqual(5);
        expect(sw).toMatch(/if \(backendReplacementSeqRef\.current !== switchReplacedAt\) \{\n\s+throw [^\n]+\n\s+\}\n\s+setSessions\(prev => prev\.map\(s => s\.id === sessionId \? \{ \.\.\.s, status: 'connected' \}[^\n]+\n[^\n]+\n\s+confirmBackendConnection\(\);/);
        // The device open, after its listing.
        expect(APP).toMatch(/if \(backendReplacementSeqRef\.current !== deviceReplacedAt\) \{\n\s+throw [^\n]+\n\s+\}\n[^\n]+\n\s+confirmBackendConnection\(\);/);
        // The AeroCloud open (both of its connects) and a GitHub branch switch
        // confirm with no await after their connect: nothing else would.
        expect(APP.match(/await runConnect\('(provider_connect|connect_ftp)', (providerParams|ftpParams)\);\n(\s+\/\/[^\n]*\n)*\s+confirmBackendConnection\(\);/g)?.length).toBe(3);
    });
});

describe('A provider connect that could reuse a live session', () => {
    it('refuses a start folder the user name cannot fill before the reuse', async () => {
        const start = APP.indexOf('    if (isProvider) {\n', APP.indexOf('  const connectToFtpImpl = async ('));
        const end = APP.indexOf('      setLoading(true);\n', start);
        expect(start, 'the provider branch of connectToFtpImpl').toBeGreaterThan(-1);
        const { code } = await transformWithOxc(`async () => {\n${APP.slice(start, end)}}\n}`, 'provider-reuse.ts');
        const params = { protocol: 'webdav', server: 'https://dav.example.test', username: 'alice/team', password: 'x', savedServerId: 'p1' };
        const ctx: Record<string, unknown> = {
            effectiveParams: params,
            protocol: 'webdav',
            isProvider: true,
            remoteDirInput: '',
            opts: undefined,
            sessions: [{ id: 's1', connectionParams: params, remoteFiles: [], remotePath: '/docs', localPath: '/home/u' }],
            invoke: vi.fn(async (command: string) => (command === 'get_local_files' ? [] : command === 'provider_probe_alive')),
            notify: { error: vi.fn() },
            t: (key: string) => key,
            connectStartFolder,
            probeAliveArgs: () => ({}),
            sameQuickConnectTarget: () => true,
            confirmBackendConnection: vi.fn(),
            fetchStorageQuota: vi.fn(),
            showHiddenFiles: false,
            logger: { debug: () => undefined },
        };
        const run = async (remoteDirInput: string) => {
            ctx.remoteDirInput = remoteDirInput;
            for (const setter of new Set(code.match(/\bset[A-Z]\w+/g))) ctx[setter] = vi.fn();
            await (new Function(...Object.keys(ctx), `return (${code.trim().replace(/;$/, '')})`)(...Object.values(ctx)) as () => Promise<void>)();
        };
        // A folder the name fills: the live session is reused.
        await run('/docs');
        expect(ctx.setActiveSessionId).toHaveBeenCalledWith('s1');
        (ctx.invoke as ReturnType<typeof vi.fn>).mockClear();
        (ctx.confirmBackendConnection as ReturnType<typeof vi.fn>).mockClear();
        await run('/{username}');
        expect((ctx.notify as { error: ReturnType<typeof vi.fn> }).error).toHaveBeenCalledOnce();
        expect(ctx.setActiveSessionId).not.toHaveBeenCalled();
        expect(ctx.confirmBackendConnection).not.toHaveBeenCalled();
        expect((ctx.invoke as ReturnType<typeof vi.fn>).mock.calls.map(([command]) => command)).not.toContain('provider_probe_alive');
    });
});

describe('The transfer queue after a sign-in put the tab aside', () => {
    it('refuses a batch while no tab is connected', async () => {
        const start = APP.indexOf('  const queuedTargetProblem = useLatestCallback(');
        const end = APP.indexOf('\n  });\n', start);
        expect(start, 'queuedTargetProblem in App.tsx').toBeGreaterThan(-1);
        const { code } = await transformWithOxc(APP.slice(start, end + '\n  });'.length), 'queued-target.ts');
        const problem = (isConnected: boolean) => {
            const ctx = {
                useLatestCallback: <F,>(fn: F) => fn,
                backendServesLiveTab: () => true,
                isConnected,
                isSameConnection: () => true,
                liveConnectionParams: { protocol: 'ftp', server: 'example.test', username: 'u', password: '' },
                accountKeysDiffer: () => false,
                liveProfileId: () => null,
                isOAuthProvider: () => false,
                isFourSharedProvider: () => false,
                liveOverlay: () => null,
            };
            const fn = new Function(...Object.keys(ctx), `${code}; return queuedTargetProblem;`)(...Object.values(ctx));
            return fn({ params: { protocol: 'ftp', server: 'example.test', username: 'u', password: '' }, profileId: null, oauthLogins: 0, overlay: null });
        };
        // parkActiveSession sets isConnected(false).
        expect(problem(false)).toBe('transfer.queuedForAnotherServer');
        expect(problem(true)).toBeNull();
    });
});
