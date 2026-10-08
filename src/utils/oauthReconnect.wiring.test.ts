// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import app from '../App.tsx?raw';
import servers from '../components/IntroHub/MyServersPanel.tsx?raw';
import settings from '../components/SettingsPanel.tsx?raw';
import { ConnectScope } from '../gui/connectScope';
import { loadOAuthClientCredentials, oauthCredentialProvider } from './oauthClientCredentials';
import { getCredentialWithRetry } from './profileVaultSecrets';
import { keyReadFailure } from './oauthKeysMissing';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
const providers = ['googledrive', 'googlephotos', 'dropbox', 'onedrive', 'box', 'pcloud', 'zohoworkdrive', 'yandexdisk'];
let vault: Record<string, string>;
beforeEach(() => {
    vault = {};
    invoke.mockReset();
    invoke.mockImplementation(async (_command: string, { account }: { account: string }) => {
        if (!(account in vault)) throw new Error(`Failed to get credential: Credential not found: ${account}`);
        return vault[account];
    });
});
const surfaces = [
    { name: 'My Servers', source: servers, start: 'let credentials: { clientId: string;', end: 'setOauthConnecting(server.id);', pair: 'credentials', scoped: true, offset: 0 },
    { name: 'Transfer resume', source: app, start: "let clientId = '';", end: "const oauthProvider = protocol === 'googledrive'", pair: '{ clientId, clientSecret }', scoped: false, offset: app.indexOf('  const connectSavedProfileForResume =') },
    { name: 'Session switch', source: app, start: '// Get OAuth credentials from vault', end: 'if (isFourSharedProvider(protocol)) {', pair: '{ clientId, clientSecret }', scoped: true, offset: 0 },
];

async function execute(source: string, context: Record<string, unknown>, suffix = '') {
    const { code } = await transformWithOxc(source, 'oauth-loading.ts');
    return new Function(...Object.keys(context), `return (async () => { ${code}\n${suffix} })();`)(...Object.values(context));
}
async function run(surface: typeof surfaces[number], provider: string, scope = new ConnectScope()) {
    const start = surface.source.indexOf(surface.start, surface.offset);
    const end = surface.source.indexOf(surface.end, start);
    if (start < 0 || end < 0) throw new Error(`Missing production credential block: ${surface.name}`);
    const notify = vi.fn();
    const result = execute(surface.source.slice(start, end), {
        invoke, getCredentialWithRetry, loadOAuthClientCredentials, oauthCredentialProvider, keyReadFailure,
        connectScope: scope, protocol: provider, server: { protocol: provider },
        notifyOAuthKeysUnavailable: notify, t: (key: string) => key, setConnectingId: vi.fn(),
    }, `return ${surface.pair};`);
    return { result, notify };
}

describe.each(surfaces)('$name OAuth app resolution', surface => {
    it.each(providers.flatMap(provider => ['modern', 'config_oauth_clients', 'config_aeroftp_oauth_settings'].map(format => ({ provider, format }))))('uses $provider credentials stored as $format', async ({ provider, format }) => {
        const slug = oauthCredentialProvider(provider);
        vault = format === 'modern'
            ? { [`oauth_${slug}_client_id`]: 'fixture-id', [`oauth_${slug}_client_secret`]: 'fixture-secret' }
            : { [format]: JSON.stringify({ [slug]: { clientId: 'fixture-id', clientSecret: 'fixture-secret' } }) };
        const { result, notify } = await run(surface, provider);
        expect(await result).toEqual({ clientId: 'fixture-id', clientSecret: 'fixture-secret' });
        expect(notify).not.toHaveBeenCalled();
        if (provider === 'googlephotos') expect(invoke.mock.calls.some(([, args]) => args.account.startsWith('oauth_googlephotos_client_'))).toBe(false);
    });
    it('stops with a read-failure notification when the vault is unreadable', async () => {
        const error = new Error('Vault I/O failure');
        invoke.mockRejectedValue(error);
        const { result, notify } = await run(surface, 'box');
        if (surface.name === 'Session switch') await expect(result).rejects.toThrow('OAuth credentials not found');
        else expect(await result).toBe(surface.name === 'My Servers' ? 'failed' : false);
        expect(notify).toHaveBeenCalledWith(expect.any(Function), 'box', error);
    });
    it('does not borrow a legacy secret for an incomplete modern app', async () => {
        vault = { oauth_box_client_id: 'new-id', config_oauth_clients: JSON.stringify({ box: { clientId: 'old-id', clientSecret: 'old-secret' } }) };
        const { result, notify } = await run(surface, 'box');
        if (surface.name === 'Session switch') await expect(result).rejects.toThrow('OAuth credentials not found');
        else expect(await result).toBe(surface.name === 'My Servers' ? 'failed' : false);
        expect(notify).toHaveBeenCalledWith(expect.any(Function), 'box', null);
    });
    if (surface.scoped) {
        it('honors cancellation during legacy credential resolution', async () => {
            const scope = new ConnectScope();
            const previous = invoke.getMockImplementation()!;
            invoke.mockImplementation((command: string, args: { account: string }) => {
                if (args.account === 'config_oauth_clients') {
                    scope.cancel(new Error('CONNECT_CANCELLED'));
                    return JSON.stringify({ box: { clientId: 'id', clientSecret: 'secret' } });
                }
                return previous(command, args);
            });
            const { result, notify } = await run(surface, 'box', scope);
            await expect(result).rejects.toThrow('CONNECT_CANCELLED');
            expect(notify).not.toHaveBeenCalled();
        });
    }
});

describe('Settings OAuth vault loading', () => {
    const start = settings.indexOf('                    const loadOAuthFromStore = async () => {');
    const end = settings.indexOf('                    await loadOAuthFromStore();', start);
    const settingsProviders = providers.filter(p => p !== 'googlephotos').concat('fourshared');
    async function runSettings(scope = new ConnectScope(), edited: Record<string, { clientId?: boolean }> = {}) {
        if (start < 0 || end < 0) throw new Error('Missing settings loader');
        let loaded = Object.fromEntries(settingsProviders.map(p => [p, { clientId: '', clientSecret: '' }]));
        loaded.box.clientId = 'typed-id';
        const notify = vi.fn();
        await execute(settings.slice(start, end), {
            scope, loadOAuthClientCredentials, keyReadFailure, notifyOAuthKeysUnavailable: notify, t: (key: string) => key,
            defaultOAuthSettings: Object.fromEntries(settingsProviders.map(p => [p, { clientId: '', clientSecret: '' }])),
            oauthEdits: { current: edited }, setOauthSettings: (update: (value: typeof loaded) => typeof loaded) => { loaded = update(loaded); },
        }, 'await loadOAuthFromStore();');
        return { loaded, notify };
    }
    it.each(['modern', 'config_oauth_clients', 'config_aeroftp_oauth_settings'])('loads every settings provider from %s', async format => {
        for (const provider of settingsProviders) {
            if (format === 'modern') {
                vault[`oauth_${provider}_client_id`] = `${provider}-id`;
                vault[`oauth_${provider}_client_secret`] = `${provider}-secret`;
            }
        }
        if (format !== 'modern') vault[format] = JSON.stringify(Object.fromEntries(settingsProviders.map(p => [p, { clientId: `${p}-id`, clientSecret: `${p}-secret` }])));
        const { loaded, notify } = await runSettings();
        for (const provider of settingsProviders) expect(loaded[provider]).toEqual({ clientId: `${provider}-id`, clientSecret: `${provider}-secret` });
        expect(notify).not.toHaveBeenCalled();
    });
    it('keeps a manually edited field while hydrating other fields', async () => {
        vault = { oauth_box_client_id: 'stored-id', oauth_box_client_secret: 'stored-secret' };
        const { loaded } = await runSettings(new ConnectScope(), { box: { clientId: true } });
        expect(loaded.box).toEqual({ clientId: 'typed-id', clientSecret: 'stored-secret' });
    });
    it('reports persistent read failures once instead of silently treating them as absent keys', async () => {
        const error = new Error('Vault I/O failure');
        invoke.mockRejectedValue(error);
        const { notify } = await runSettings();
        expect(notify).toHaveBeenCalledOnce();
        expect(notify).toHaveBeenCalledWith(expect.any(Function), 'googledrive', error);
    });
    it('does not apply a stale settings load after cancellation', async () => {
        const scope = new ConnectScope();
        scope.cancel(new Error('CONNECT_CANCELLED'));
        await expect(runSettings(scope)).rejects.toThrow('CONNECT_CANCELLED');
    });
});
