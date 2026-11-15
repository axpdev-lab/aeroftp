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
import { fixture, local, LOCAL } from './appTransfers.testkit';
import PROVIDER_COMMANDS_RS from '../../src-tauri/src/provider_commands.rs?raw';
import MY_SERVERS from './IntroHub/MyServersPanel.tsx?raw';
import CONNECTION_SCREEN from './ConnectionScreen.tsx?raw';
import USE_OAUTH2 from '../hooks/useOAuth2.ts?raw';

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

    it("refuses App's own upload while the sign-in still reads the profile to save", async () => {
        // App's transfers on the live tab, and App's swap listener on the same
        // backend accounting.
        const app = fixture({ folders: { [LOCAL]: [local('a.txt')] } });
        const start = APP.indexOf('  const backendMark = (): BackendMark => ({');
        const listener = APP.indexOf('  const backendSwapRef = useRef<');
        const { code } = await transformWithOxc([
            APP.slice(start, APP.indexOf('  const remoteConnectPhaseVersionRef = useRef(0);', start)),
            APP.slice(listener, APP.indexOf('  // FTP operations', listener)),
        ].join('\n'), 'swap-listener.ts');
        const ctx = {
            useRef: <T,>(current: T) => ({ current }),
            useEffect: (effect: () => () => void) => { cleanups.push(effect()); },
            useLatestCallback: <F,>(fn: F) => fn,
            notify: { error: vi.fn() }, t: (key: string) => key,
            // The takeover only schedules the park: the tab still reads connected.
            takeOverActiveSession: vi.fn(), parkActiveSession: vi.fn(),
            remoteConnectPhaseVersionRef: { current: 7 },
            backendUncertainRef: app.uncertain, backendConfirmationsRef: app.confirmations,
            backendTeardownsRef: app.teardowns, backendReplacementSeqRef: { current: 0 },
        };
        const { confirmBackendConnection } = new Function(...Object.keys(ctx), `${code}; return { confirmBackendConnection };`)(...Object.values(ctx));
        try {
            await openEditor('googledrive');
            // The form's read of the profile to save is held after the swap.
            let release!: () => void;
            let reading!: () => void;
            const readStarted = new Promise<void>(resolve => { reading = resolve; });
            const previous = mocks.invoke.getMockImplementation()!;
            mocks.invoke.mockImplementation(async (command: string, args?: any) => {
                if (command === 'user_partitions_load_active_server_profiles_scoped') {
                    reading();
                    await new Promise<void>(resolve => { release = resolve; });
                }
                return previous(command, args);
            });
            // OAuthConnect swaps the backend through `swapBackend`, then hands
            // the sign-in to the form (`onConnected`), which reads the profile.
            await swapBackend(async () => undefined);
            let signIn!: Promise<void>;
            act(() => { signIn = Promise.resolve(button('Test OAuth sign-in').click()); });
            await readStarted;
            expect(ctx.takeOverActiveSession).toHaveBeenCalled();
            // A write through App's upload now: refused, nothing written.
            await app.uploadFile(`${LOCAL}/a.txt`, 'a.txt', false, 40);
            expect(app.written()).toEqual([]);
            await act(async () => { release(); await signIn; });
            expect(onConnectSpy).toHaveBeenCalledOnce();
            // Once a connect confirms the tab, the same upload lands.
            confirmBackendConnection();
            await app.uploadFile(`${LOCAL}/a.txt`, 'a.txt', false, 40);
            expect(app.written()).toEqual(['/remote/a.txt']);
        } finally {
            cleanups.splice(0).forEach(cleanup => cleanup());
        }
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
        between('  const backendMark = (): BackendMark => ({', '  const remoteConnectPhaseVersionRef = useRef(0);'),
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
            remoteConnectPhaseVersionRef: { current: 0 },
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
    async function createSessionRun(replaceDuringRead: boolean, confirmDuringRead = false) {
        const start = APP.indexOf('  const createSession = async (');
        const end = APP.indexOf('\n  };\n', start);
        expect(start, 'createSession in App.tsx').toBeGreaterThan(-1);
        const { code } = await transformWithOxc(APP.slice(start, end + '\n  };'.length), 'create-session.ts');
        const seq = { current: 4 };
        const confirmations = { current: 2 };
        const ctx: Record<string, unknown> = {
            backendReplacementSeqRef: seq,
            backendConfirmationsRef: confirmations,
            loadSavedServerProfiles: async () => {
                if (replaceDuringRead) seq.current += 1;
                // Another connect finished meanwhile and confirmed its tab.
                if (confirmDuringRead) confirmations.current += 1;
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
        // The caller read the mark as its own connect returned.
        const outcome = await createSession('Server', { protocol: 'sftp', server: 'h', username: 'u', password: '' }, '/', '/home', [], [], undefined, { replacements: 4, confirmations: 2 });
        const added = (ctx.setSessions as ReturnType<typeof vi.fn>).mock.calls[0][0]([]) as { status: string }[];
        return { ctx, added, outcome };
    }

    it('keeps the tab cached and confirms nothing when a replacement ended during the profile read', async () => {
        const { ctx, added, outcome } = await createSessionRun(true);
        expect(outcome).toBe('stale');
        expect(ctx.confirmBackendConnection).not.toHaveBeenCalled();
        expect(ctx.setActiveSessionId).not.toHaveBeenCalled();
        expect(ctx.setIsConnected).toHaveBeenCalledWith(false);
        expect(added[0].status).toBe('cached');
        expect((ctx.notify as { error: ReturnType<typeof vi.fn> }).error).toHaveBeenCalledWith('connection.connectionFailed', 'toast.remoteTargetChanged');
    });

    it("leaves a newer connect's workspace connected when that one confirmed during the profile read", async () => {
        const { ctx, added } = await createSessionRun(true, true);
        expect(ctx.confirmBackendConnection).not.toHaveBeenCalled();
        expect(ctx.setActiveSessionId).not.toHaveBeenCalled();
        expect(ctx.setIsConnected).not.toHaveBeenCalled();
        expect(added[0].status).toBe('cached');
    });

    it("leaves the live connection's Navigation Sync alone when A is refused as stale", async () => {
        const { ctx } = await createSessionRun(true, true);
        expect(ctx.setIsSyncNavigation).not.toHaveBeenCalled();
        expect(ctx.setSyncBasePaths).not.toHaveBeenCalled();
        const live = await createSessionRun(false);
        expect(live.ctx.setIsSyncNavigation).toHaveBeenCalledWith(false);
        expect(live.ctx.setSyncBasePaths).toHaveBeenCalledWith(null);
    });

    it('confirms and makes the tab live when nothing replaced the backend', async () => {
        const { ctx, added, outcome } = await createSessionRun(false);
        expect(outcome).toBe('live');
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
            // A stale outcome stops the flow right there: nothing after it
            // (the quota, the overlay, the success return) describes the
            // backend that replaced this tab's.
            const head = APP.slice(APP.lastIndexOf('\n', call) + 1, call);
            expect(head, APP.slice(call, call + 80)).toMatch(/^\s+if \(await (connectScope\.step\(\(\) => )?$/);
            const tail = APP.slice(close + 1, APP.indexOf('\n', close));
            expect(tail, APP.slice(call, call + 80)).toMatch(/^\)? === 'stale'\) return( 'failed' as const)?;$/);
        }
        for (const read of ['providerReplacedAt', 'ftpReplacedAt', 'savedProviderReplacedAt', 'savedFtpReplacedAt']) {
            // Taken from the receipt the flow's own connect filled as its
            // replacement ended, on the statement right after that connect.
            const receipt = read.replace('ReplacedAt', 'Receipt');
            const at = APP.indexOf(`const ${read} = ${receipt}.mark!;`);
            expect(at, read).toBeGreaterThan(-1);
            const before = APP.slice(APP.lastIndexOf('await ', at), at);
            expect(before, read).toMatch(new RegExp(`^await connectUnderRow\\([^]*runConnect\\('(provider_connect|connect_ftp)'[^]*, ${receipt}\\)\\)?\\);\\n\\s+$`));
        }
        for (const read of ['oauthReplacedAt', 'savedOAuthReplacedAt']) {
            expect(APP, read).toContain(`const ${read} = backendMark();`);
        }
        // A tab switch reads it after each of its own connects, before any
        // other statement, and refuses before confirming.
        const swStart = APP.indexOf('  const switchSession = ');
        const sw = APP.slice(swStart, APP.indexOf('\n  };\n', swStart));
        const connects = [...sw.matchAll(/await connectScope\.step\(\(\) => (runConnect|cancellableConnect|swapBackend)\(/g)];
        expect(connects.length).toBeGreaterThanOrEqual(6);
        for (const connect of connects) {
            // The statement's end, by parenthesis depth.
            let depth = 0;
            let close = connect.index!;
            for (let i = sw.indexOf('(', connect.index!); i < sw.length; i++) {
                if (sw[i] === '(') depth++;
                else if (sw[i] === ')' && --depth === 0) { close = i; break; }
            }
            // Only more connects of the same step (a sign-in, then its
            // connect) may come before the read.
            const after = sw.slice(sw.indexOf(';', close) + 1);
            expect(after, sw.slice(connect.index!, connect.index! + 120)).toMatch(/^(\s*await connectScope\.step\(\(\) => (runConnect|cancellableConnect)\([^;]*;)*\s*switchReplacedAt = switchReceipt\.mark!\.replacements;/);
            // Each connect carries the switch's receipt.
            expect(sw.slice(connect.index!, close + 1), sw.slice(connect.index!, connect.index! + 120)).toMatch(/, switchReceipt\)\)$/);
        }
        expect(sw).toMatch(/if \(backendReplacementSeqRef\.current !== switchReplacedAt\) \{\n\s+throw [^\n]+\n\s+\}\n\s+setSessions\(prev => prev\.map\(s => s\.id === sessionId \? \{ \.\.\.s, status: 'connected' \}[^\n]+\n[^\n]+\n\s+confirmBackendConnection\(\);/);
        // The device open counts its own replacement, reads the sequence once
        // it ended, and checks it after its listing.
        expect(APP).toMatch(/const endDeviceReplacement = replacingBackend\(\);\n\s+try \{[^]*?mtp_open_gvfs_mount[^]*?mtp_open_device[^]*?\} finally \{\n\s+endDeviceReplacement\(\);\n\s+\}\n\s+setActivePortableDeviceId\(session\.deviceId\);\n(\s+\/\/[^\n]*\n)*\s+const deviceReplacedAt = backendReplacementSeqRef\.current;/);
        expect(APP).toMatch(/if \(backendReplacementSeqRef\.current !== deviceReplacedAt\) \{\n\s+throw [^\n]+\n\s+\}\n[^\n]+\n\s+confirmBackendConnection\(\);/);
        // The AeroCloud open (both of its connects) and a GitHub branch switch
        // confirm with no await after their connect: nothing else would.
        // confirm with no await after their connect, and only while no newer
        // connect owns the workspace.
        expect(APP.match(/await runConnect\('(provider_connect|connect_ftp)', (providerParams|ftpParams)\);\n(\s+\/\/[^\n]*\n)*\s+if \((remoteConnectPhaseVersionRef\.current === cloudPhase|phaseVersion === remoteConnectPhaseVersionRef\.current)\) confirmBackendConnection\(\);/g)?.length).toBe(3);
    });
});

/** App's code from `from` to `to`, run with every free name it reads taken
 *  from `overrides`, else from the globals, else a stub made on first read (a
 *  `{ current: 0 }` for a ref, a spy otherwise). */
async function runAppSlice(source: string, overrides: Record<string, unknown>, result: string) {
    const { code } = await transformWithOxc(source, 'app-slice.ts');
    const made = new Map<string | symbol, unknown>();
    const scope = new Proxy(overrides, {
        has: (target, key) => typeof key === 'string' && (key in target || !(key in globalThis)),
        get: (target, key) => {
            if (key === Symbol.unscopables) return undefined;
            if (key in target) return target[key as string];
            if (key in globalThis) return (globalThis as Record<string | symbol, unknown>)[key];
            if (!made.has(key)) made.set(key, String(key).endsWith('Ref') ? { current: 0 } : vi.fn(async () => undefined));
            return made.get(key);
        },
        set: (target, key, value) => { target[key as string] = value; return true; },
    });
    return new Function('__scope', `with (__scope) { ${code}\n return ${result}; }`)(scope);
}
const appBetween = (from: string, to: string) => {
    const start = APP.indexOf(from);
    expect(start, from).toBeGreaterThan(-1);
    const end = APP.indexOf(to, start);
    expect(end, to).toBeGreaterThan(start);
    return APP.slice(start, end + to.length);
};
const connectScope = () => ({ assert: () => undefined, step: <T,>(fn: () => T) => fn(), cancellable: <T,>(_c: unknown, fn: () => T) => fn() });

describe('Bot-named runs of the backend replacement checks', () => {
    /** App's real switchSession for an FTP tab whose liveness probe fails, so
     *  the switch reconnects through `runConnect('connect_ftp')`, which ends
     *  its own replacement. `replaceAfterRefresh` ends another one during the
     *  listing that follows the refresh. */
    async function ftpSwitch(replaceAfterRefresh: boolean) {
        const seq = { current: 3 };
        const confirmBackendConnection = vi.fn();
        const logger = { debug: vi.fn(), warn: vi.fn(), error: vi.fn(), info: vi.fn() };
        const activityLog = { updateEntry: vi.fn(), log: vi.fn() };
        const target = {
            id: 'ftp-tab', serverName: 'ftp.example.test', status: 'cached', remotePath: '/pub', localPath: '/home/me',
            remoteFiles: [], localFiles: [], connectionParams: { protocol: 'ftp', server: 'ftp.example.test', username: 'u', password: '' },
        };
        const invoke = vi.fn(async (command: string) => {
            if (command === 'provider_probe_alive') return false;
            if (command === 'list_files') {
                if (replaceAfterRefresh) seq.current += 1;
                return { files: [], current_path: '/pub' };
            }
            if (command === 'get_local_files') return [];
            return undefined;
        });
        // Ends its own replacement, and hands back the receipt that end leaves.
        const runConnect = vi.fn(async (_command: string, _params: unknown, _scope: unknown, receipt?: { mark?: unknown }) => {
            seq.current += 1;
            if (receipt) receipt.mark = { replacements: seq.current, confirmations: 0 };
        });
        const source = appBetween('  const switchSession = async (', '\n  };\n');
        const switchSession = await runAppSlice(source, {
            sessions: [target], activeSessionId: 'other-tab', backendReplacementSeqRef: seq, confirmBackendConnection,
            logger, activityLog, invoke, runConnect, remoteFiles: [], localFiles: [],
            isSshProtocol: () => false, isOAuthProvider: () => false, isFourSharedProvider: () => false, usesProviderApi: () => false,
            probeAliveArgs: () => ({}), loadedOptionOn: () => false, isListingCancelled: () => false, isConnectCancelledError: () => false,
            withRemoteListSpinner: (fn: () => unknown) => fn(), t: (key: string) => key, humanLog: { logRaw: () => 'log' },
        }, 'switchSession');
        await switchSession('ftp-tab', connectScope());
        return { confirmBackendConnection, logger, activityLog, runConnect };
    }

    it('completes an FTP switch whose liveness probe fails without remoteTargetChanged', async () => {
        const { confirmBackendConnection, logger, activityLog, runConnect } = await ftpSwitch(false);
        expect(runConnect).toHaveBeenCalledWith('connect_ftp', expect.anything(), expect.anything(), expect.anything());
        expect(logger.error).not.toHaveBeenCalled();
        expect(confirmBackendConnection).toHaveBeenCalledOnce();
        expect(activityLog.updateEntry).toHaveBeenLastCalledWith('log', expect.objectContaining({ status: 'success' }));
    });

    it('still fails the FTP switch for a separate replacement after the refresh', async () => {
        const { confirmBackendConnection, logger, activityLog } = await ftpSwitch(true);
        expect(logger.error).toHaveBeenCalledWith('Reconnect error:', new Error('toast.remoteTargetChanged'));
        expect(confirmBackendConnection).not.toHaveBeenCalled();
        expect(activityLog.updateEntry).toHaveBeenLastCalledWith('log', expect.objectContaining({ status: 'error' }));
    });

    it("pauses A's profile lookup, opens a device, and A never becomes live", async () => {
        // App's own replacement accounting, shared by A's createSession and the
        // device open.
        const refs = {
            backendTeardownsRef: { current: 0 }, backendUncertainRef: { current: false },
            backendConfirmationsRef: { current: 5 }, backendReplacementSeqRef: { current: 2 },
        };
        const accounting = await runAppSlice(
            appBetween('  const backendMark = (): BackendMark => ({', '\n  const remoteConnectPhaseVersionRef = useRef(0);'),
            { ...refs, useRef: (current: unknown) => ({ current }) },
            '{ confirmBackendConnection, replacingBackend, backendMark }',
        );
        const deviceInvoke = vi.fn(async (command: string) => {
            if (command === 'mtp_open_device') return { deviceId: 'phone', displayName: 'Phone' };
            if (command === 'provider_list_files') return { files: [], current_path: '/' };
            if (command === 'get_local_files') return [];
            return undefined;
        });
        // App's real device open: the callback useCallback wraps.
        const { code: deviceCode } = await transformWithOxc(
            `const openDevice = ${appBetween('  const openMtpSessionFromLiveDevice = useCallback(', '\n  }, [').replace('  const openMtpSessionFromLiveDevice = useCallback(', '').replace(/\n  \}, \[$/, '\n  }')};`,
            'device-open.ts');
        const deviceScope = { ...refs, ...accounting, invoke: deviceInvoke, t: (key: string) => key, findGvfsMtpMount: () => null,
            localFiles: [], remoteFiles: [], currentLocalPath: '/home/me', notify: { info: vi.fn(), error: vi.fn() } };
        const open = await runAppSlice(deviceCode, deviceScope, 'openDevice');
        expect(deviceInvoke).not.toHaveBeenCalled();

        const createStart = APP.indexOf('  const createSession = async (');
        const createSource = APP.slice(createStart, APP.indexOf('\n  };\n', createStart) + '\n  };'.length);
        const setActiveSessionId = vi.fn();
        const setSessions = vi.fn();
        const createSession = await runAppSlice(createSource, {
            ...refs, ...accounting, setActiveSessionId, setSessions, remoteFiles: [], localFiles: [],
            notify: { error: vi.fn() }, t: (key: string) => key,
            loadSavedServerProfiles: async () => {
                // The device opens while A reads the saved profiles.
                await open({ deviceId: 'phone', displayName: 'Phone' }, undefined, connectScope());
                return [];
            },
        }, 'createSession');
        // A read its mark as its own connect returned.
        const mark = accounting.backendMark();
        expect(mark).toEqual({ replacements: 2, confirmations: 5 });
        const outcome = await createSession('A', { protocol: 'sftp', server: 'a.example.test', username: 'u', password: '' }, '/', '/home', [], [], undefined, mark);
        expect(deviceInvoke).toHaveBeenCalledWith('mtp_open_device', expect.anything());
        expect(outcome).toBe('stale');
        const added = setSessions.mock.calls.map(([update]) => update([])).flat() as { serverId?: string; status: string; id: string }[];
        const tabA = added.find(tab => tab.serverId === 'A');
        expect(tabA?.status).toBe('cached');
        expect(setActiveSessionId).not.toHaveBeenCalledWith(tabA?.id);
    });

    it.each([
        ['the OAuth connect', 'oauthReplacedAt,', 'fetchStorageQuota(protocol, effectiveParams, { newConnection: true });'],
        ['the provider connect', 'providerReplacedAt,', 'fetchStorageQuota(protocol, sessionParams, { newConnection: true });'],
    ])("does not persist backend B's metadata onto saved profile A after %s is refused as stale", async (_name, mark, quota) => {
        // From the caller's createSession to its quota fetch, which writes the
        // connection's quota onto the saved profile (`persistQuotaToProfile`).
        const at = APP.indexOf(mark);
        const from = APP.lastIndexOf('if (await createSession(', at);
        const source = `const tail = async () => {\n${APP.slice(from, APP.indexOf(quota, at) + quota.length)}\n};`;
        const fetchStorageQuota = vi.fn();
        const markSessionOverlayKind = vi.fn();
        const setPendingOverlayUnlock = vi.fn();
        const tail = await runAppSlice(source, {
            createSession: async () => 'stale', fetchStorageQuota, markSessionOverlayKind, setPendingOverlayUnlock,
            overlayHint: { kind: 'aerocrypt', anchor: '/vault', opensOutside: false }, overlaySavedId: 'profile-A',
            shouldAutoUnlockOverlayOnConnect: () => true, getEnabledProfileOverlay: async () => ({}),
        }, 'tail');
        await tail();
        expect(fetchStorageQuota).not.toHaveBeenCalled();
        expect(markSessionOverlayKind).not.toHaveBeenCalled();
        expect(setPendingOverlayUnlock).not.toHaveBeenCalled();
    });
});

describe('Reviewed ownership of a connect', () => {
    /** App's backend accounting on fresh refs. */
    async function accountingOn(refs: Record<string, unknown>) {
        return runAppSlice(
            appBetween('  const backendMark = (): BackendMark => ({', '\n  const remoteConnectPhaseVersionRef = useRef(0);'),
            { ...refs, useRef: (current: unknown) => ({ current }) },
            '{ confirmBackendConnection, replacingBackend, backendMark }',
        );
    }
    const freshRefs = () => ({
        backendTeardownsRef: { current: 0 }, backendUncertainRef: { current: false },
        backendConfirmationsRef: { current: 0 }, backendReplacementSeqRef: { current: 0 },
        remoteConnectPhaseVersionRef: { current: 0 },
    });

    it("keeps the backend uncertain when B confirms before A's branch-switch connect returns", async () => {
        const refs = freshRefs();
        const accounting = await accountingOn(refs);
        // A's connect replaces the backend; B starts, connects and confirms
        // its tab before A's connect returns.
        const runConnect = vi.fn(async () => {
            const endA = accounting.replacingBackend();
            refs.remoteConnectPhaseVersionRef.current += 1;
            accounting.confirmBackendConnection();
            endA();
        });
        const source = APP.slice(APP.indexOf('  const switchGitHubBranch ='), APP.indexOf('  const requestGitHubBatchCommitMessage'));
        const switchGitHubBranch = await runAppSlice(`const __switch = ${source.replace(/^\s*const switchGitHubBranch =\s*/, '').replace(/;\s*$/, '')};`, {
            ...refs, ...accounting, runConnect, useCallback: (fn: unknown) => fn,
            normalizeProviderConnectionParams: () => ({}), liveConnectionParams: {}, activeSessionId: null, currentRemotePath: '/',
            buildProviderParams: async () => ({ effectiveParams: {}, providerParams: {} }),
            invoke: vi.fn(async (command: string) => command === 'provider_list_files' ? { files: [], current_path: '/' } : undefined),
            withRemoteListSpinner: (fn: () => unknown) => fn(), t: (key: string) => key, notify: { success: vi.fn(), error: vi.fn() },
            isConnectCancelledError: () => false, isListingCancelled: () => false,
        }, '__switch');
        await switchGitHubBranch('other');
        expect(runConnect).toHaveBeenCalledOnce();
        expect(refs.backendUncertainRef.current).toBe(true);
    });

    it.each([['a provider', true], ['an FTP server', false]])("keeps the backend uncertain when B confirms before A's AeroCloud connect to %s returns", async (_label, isProvider) => {
        const refs = freshRefs();
        const accounting = await accountingOn(refs);
        const runConnect = vi.fn(async () => {
            const endA = accounting.replacingBackend();
            refs.remoteConnectPhaseVersionRef.current += 1;
            accounting.confirmBackendConnection();
            endA();
        });
        const connect = await runAppSlice(appBetween('const connectToCloudServer = async', '\n      };\n'), {
            ...refs, ...accounting, runConnect, isProvider, protocol: isProvider ? 'webdav' : 'ftp',
            cloudServer: { id: 'cloud', providerId: null, host: 'dav.example.test', port: 443, username: 'u', options: {} },
            cloudServerString: 'ftp.example.test:21', cloudConfig: { remote_folder: '/sync' }, cloudPassword: 'p', cloudFilenApiKey: null,
            resolveProfileS3Location: () => ({ endpoint: null, signingRegion: undefined }), scpFallbackConnectParam: () => ({}),
            loadedCertificateCheck: () => true, loadedOption: () => undefined, loadedOptionOn: () => false, isSshProtocol: () => false,
        }, 'connectToCloudServer');
        await connect();
        expect(runConnect).toHaveBeenCalledOnce();
        expect(refs.backendUncertainRef.current).toBe(true);
    });

    it("hands A the receipt of its own replacement when B's ends between A's end and A's continuation", async () => {
        // A sequence ref that queues B's end right after A's end, ahead of
        // A's continuation.
        let seq = 0;
        let afterNextEnd: (() => void) | null = null;
        const backendReplacementSeqRef = {
            get current() { return seq; },
            set current(value: number) {
                seq = value;
                const next = afterNextEnd;
                afterNextEnd = null;
                if (next) queueMicrotask(next);
            },
        };
        const refs = { ...freshRefs(), backendReplacementSeqRef };
        const accounting = await accountingOn(refs);
        const wrappers = await runAppSlice(
            `${appBetween('  const cancellableConnect = useCallback(async <T,>(', '\n  }, [beginConnectAttempt, cancelActiveConnect, endConnectAttempt, notify, t]);')}\n${appBetween('  const runConnect = useCallback(<T,>(', '\n  }, receipt), [cancellableConnect]);')}\n${appBetween('  const connectUnderRow = async (', '\n  };\n')}`,
            { ...refs, ...accounting, useCallback: (fn: unknown) => fn, beginConnectAttempt: () => 'token', endConnectAttempt: () => undefined,
                invoke: vi.fn(async () => undefined), connectCancelledRef: { current: false }, extendHardTimeoutRef: { current: null },
                isConnectHardTimeoutError: () => false, isConnectCancelledError: () => false, humanLog: { updateEntry: vi.fn() } },
            '{ runConnect, connectUnderRow }',
        );
        const endB = accounting.replacingBackend();
        afterNextEnd = endB;
        const receipt: { mark?: { replacements: number; confirmations: number } } = {};
        await wrappers.connectUnderRow(null, () => wrappers.runConnect('provider_connect', {}, undefined, receipt));
        // B's end landed before A's continuation: the receipt is A's own.
        expect(seq).toBe(2);
        expect(receipt.mark).toEqual({ replacements: 1, confirmations: 0 });
        expect(accounting.backendMark().replacements).toBe(2);
        // And createSession refuses A on that receipt.
        const createStart = APP.indexOf('  const createSession = async (');
        const createSession = await runAppSlice(APP.slice(createStart, APP.indexOf('\n  };\n', createStart) + '\n  };'.length), {
            ...refs, ...accounting, remoteFiles: [], localFiles: [], notify: { error: vi.fn() }, t: (key: string) => key,
            loadSavedServerProfiles: async () => [],
        }, 'createSession');
        expect(await createSession('A', { protocol: 'sftp', server: 'a.example.test', username: 'u', password: '' }, '/', '/home', [], [], undefined, receipt.mark)).toBe('stale');
    });
});

describe('Every invoke that installs a provider in the backend slot', () => {
    it('is counted as a replacement, for every command the backend lists', () => {
        // The backend's own list of the commands that install into the slot
        // (`PROVIDER_SLOT_INSTALLERS`, pinned there against its installers).
        const list = PROVIDER_COMMANDS_RS.match(/const PROVIDER_SLOT_INSTALLERS: &\[&str\] = &\[([^\]]*)\]/);
        expect(list, 'PROVIDER_SLOT_INSTALLERS in provider_commands.rs').not.toBeNull();
        const commands = [...list![1].matchAll(/"([a-z0-9_]+)"/g)].map(m => m[1]);
        expect(commands).toContain('mtp_open_device');
        const sources = { 'App.tsx': APP, 'MyServersPanel.tsx': MY_SERVERS, 'ConnectionScreen.tsx': CONNECTION_SCREEN, 'useOAuth2.ts': USE_OAUTH2 };
        let sites = 0;
        for (const command of commands) {
            for (const [file, source] of Object.entries(sources)) {
                for (const site of source.matchAll(new RegExp(`(invoke(<[^()]*?>)?\\(|runConnect\\()'${command}'`, 'g'))) {
                    sites += 1;
                    const at = site.index!;
                    const line = source.slice(source.lastIndexOf('\n', at) + 1, source.indexOf('\n', at));
                    const before = source.slice(Math.max(0, at - 1500), at);
                    const where = `${command} in ${file}: ${line.trim()}`;
                    if (site[0].startsWith('runConnect(') || /swapBackend\(\(\) => invoke/.test(line)) continue;
                    if (file === 'MyServersPanel.tsx') {
                        // Its connect closures run through App's cancellableConnect.
                        const fn = before.match(/const (do\w+Connect) = async[^]*$/);
                        expect(fn, where).not.toBeNull();
                        expect(source, where).toContain(`scopedCancellable(${fn![1]})`);
                        continue;
                    }
                    expect(/cancellableConnect\(|= replacingBackend\(\);/.test(before), where).toBe(true);
                }
            }
        }
        expect(sites).toBeGreaterThanOrEqual(commands.length);
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
