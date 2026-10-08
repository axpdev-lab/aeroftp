// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { transformWithOxc } from 'vite';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import app from '../App.tsx?raw';
import servers from '../components/IntroHub/MyServersPanel.tsx?raw';
import { ConnectScope } from '../gui/connectScope';
import { loadFourSharedCredentials } from './fourSharedCredentials';
import { getCredentialWithRetry } from './profileVaultSecrets';
import { keyReadFailure } from './oauthKeysMissing';
import { loadOAuthClientCredentials, oauthCredentialProvider } from './oauthClientCredentials';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

let vault: Record<string, string>;
beforeEach(() => {
    vault = {};
    invoke.mockReset();
    invoke.mockImplementation(async (_command: string, { account }: { account: string }) => {
        if (!(account in vault)) throw new Error(`Failed to get credential: Credential not found: ${account}`);
        return vault[account];
    });
});

// Execute each production credential-loading block with the real vault readers;
// stop before authentication so these regressions do not access a network.
const surfaces = [
    { name: 'My Servers', source: servers, start: "let consumerKey = '', consumerSecret = '';", end: 'setOauthConnecting(server.id);', pair: '{ consumerKey, consumerSecret }', scoped: true },
    { name: 'Transfer resume', source: app, start: "let consumerKey = '';", end: "const hasTokens = await invoke<boolean>('fourshared_has_tokens'", pair: '{ consumerKey, consumerSecret }', scoped: false },
    { name: 'Session switch', source: app, start: '// Get OAuth credentials from vault', end: 'if (isFourSharedProvider(protocol)) {', pair: '{ consumerKey: clientId, consumerSecret: clientSecret }', scoped: true },
];

async function run(surface: typeof surfaces[number], scope = new ConnectScope()) {
    const start = surface.source.indexOf(surface.start);
    const end = surface.source.indexOf(surface.end, start);
    if (start < 0 || end < 0) throw new Error(`Missing credential block: ${surface.name}`);
    const notify = vi.fn();
    const context = {
        invoke, getCredentialWithRetry, loadFourSharedCredentials, loadOAuthClientCredentials, oauthCredentialProvider, keyReadFailure,
        connectScope: scope, secureGetWithFallback: async () => null,
        protocol: 'fourshared', server: { protocol: 'fourshared' },
        isFourSharedProvider: (protocol: string) => protocol === 'fourshared',
        notifyOAuthKeysUnavailable: notify, t: (key: string) => key,
        setConnectingId: vi.fn(), console,
    };
    const { code } = await transformWithOxc(surface.source.slice(start, end), 'fourshared-reconnect.ts');
    const execute = new Function(...Object.keys(context), `return (async () => { ${code}\nreturn ${surface.pair}; })();`);
    return { result: execute(...Object.values(context)), notify };
}

describe.each(surfaces)('$name 4shared credential loading', surface => {
    it.each(['config_oauth_clients', 'config_aeroftp_oauth_settings', 'fourshared_oauth_settings'])('reconnects with app keys only stored in %s', async account => {
        vault[account] = JSON.stringify(account === 'fourshared_oauth_settings'
            ? { consumer_key: 'legacy-key', consumer_secret: 'legacy-secret' }
            : { fourshared: { clientId: 'legacy-key', clientSecret: 'legacy-secret' } });
        const { result, notify } = await run(surface);
        expect(await result).toEqual({ consumerKey: 'legacy-key', consumerSecret: 'legacy-secret' });
        expect(notify).not.toHaveBeenCalled();
    });

    it('preserves modern keys without reading any legacy blob', async () => {
        vault = { oauth_fourshared_client_id: 'modern-key', oauth_fourshared_client_secret: 'modern-secret' };
        const { result } = await run(surface);
        expect(await result).toEqual({ consumerKey: 'modern-key', consumerSecret: 'modern-secret' });
        expect(invoke.mock.calls.map(([, args]) => args.account)).toEqual(['oauth_fourshared_client_id', 'oauth_fourshared_client_secret']);
    });

    it('recovers from an unreadable modern key when the next read resolves a complete pair', async () => {
        invoke.mockRejectedValueOnce(new Error('Vault read failure'));
        vault = { oauth_fourshared_client_id: 'modern-key', oauth_fourshared_client_secret: 'modern-secret' };
        const { result, notify } = await run(surface);
        expect(await result).toEqual({ consumerKey: 'modern-key', consumerSecret: 'modern-secret' });
        expect(notify).not.toHaveBeenCalled();
    });

    it('retains the existing retry for a vault that is not ready yet', async () => {
        invoke.mockRejectedValueOnce(new Error('STORE_NOT_READY'));
        vault = { oauth_fourshared_client_id: 'modern-key', oauth_fourshared_client_secret: 'modern-secret' };
        const { result, notify } = await run(surface);
        expect(await result).toEqual({ consumerKey: 'modern-key', consumerSecret: 'modern-secret' });
        // Session switching retains its direct read, then retries through the
        // fallback; My Servers and transfer resume retain their retry helper.
        expect(invoke.mock.calls.map(([, args]) => args.account)).toEqual(['oauth_fourshared_client_id', 'oauth_fourshared_client_id', 'oauth_fourshared_client_secret']);
        expect(notify).not.toHaveBeenCalled();
    });

    it('retains the read failure and stops when fallback cannot supply a complete pair', async () => {
        const error = new Error('Vault read failure');
        invoke.mockRejectedValueOnce(error);
        vault.config_oauth_clients = JSON.stringify({ fourshared: { clientId: 'partial-key' } });
        const { result, notify } = await run(surface);
        if (surface.name === 'Session switch') await expect(result).rejects.toThrow('OAuth credentials not found');
        else expect(await result).toBe(surface.name === 'My Servers' ? 'failed' : false);
        expect(notify).toHaveBeenCalledWith(expect.any(Function), 'fourshared', error);
    });

    if (surface.scoped) {
        it('honors cancellation during a legacy credential read', async () => {
            const scope = new ConnectScope();
            const previous = invoke.getMockImplementation()!;
            invoke.mockImplementation(async (command: string, args: { account: string }) => {
                if (args.account === 'config_oauth_clients') {
                    scope.cancel(new Error('CONNECT_CANCELLED'));
                    return JSON.stringify({ fourshared: { clientId: 'legacy-key', clientSecret: 'legacy-secret' } });
                }
                return previous(command, args);
            });
            const { result, notify } = await run(surface, scope);
            await expect(result).rejects.toThrow('CONNECT_CANCELLED');
            expect(notify).not.toHaveBeenCalled();
        });
    }
});
