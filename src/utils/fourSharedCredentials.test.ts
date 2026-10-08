// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { loadFourSharedCredentials } from './fourSharedCredentials';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

let vault: Record<string, string>;
beforeEach(() => {
    vault = {};
    invoke.mockReset();
    invoke.mockImplementation(async (command: string, { account }: { account: string }) => {
        expect(command).toBe('get_credential');
        if (!(account in vault)) throw new Error('Credential not found');
        return vault[account];
    });
});

describe('4shared app credential compatibility', () => {
    const legacyAccounts = ['config_oauth_clients', 'config_aeroftp_oauth_settings', 'fourshared_oauth_settings'];
    const legacyBlob = (account: string, key = 'legacy-key', secret = 'legacy-secret') => JSON.stringify(
        account === 'fourshared_oauth_settings' ? { consumer_key: key, consumer_secret: secret } : { fourshared: { clientId: key, clientSecret: secret } },
    );

    it.each(legacyAccounts)('reads the key and secret from %s', async account => {
        vault[account] = legacyBlob(account);
        expect(await loadFourSharedCredentials()).toEqual({ consumerKey: 'legacy-key', consumerSecret: 'legacy-secret' });
    });

    it('prefers the modern app pair and avoids reading legacy blobs', async () => {
        vault = { oauth_fourshared_client_id: 'modern-key', oauth_fourshared_client_secret: 'modern-secret', config_oauth_clients: legacyBlob('config_oauth_clients') };
        expect(await loadFourSharedCredentials()).toEqual({ consumerKey: 'modern-key', consumerSecret: 'modern-secret' });
        expect(invoke).toHaveBeenCalledTimes(2);
    });

    it('preserves structured blob precedence', async () => {
        legacyAccounts.forEach((account, index) => { vault[account] = legacyBlob(account, `key-${index}`, `secret-${index}`); });
        expect(await loadFourSharedCredentials()).toEqual({ consumerKey: 'key-0', consumerSecret: 'secret-0' });
        delete vault.config_oauth_clients;
        expect(await loadFourSharedCredentials()).toEqual({ consumerKey: 'key-1', consumerSecret: 'secret-1' });
    });

    it.each(['modern', 'structured'])('never combines a partial %s app record with another app secret', async format => {
        vault.fourshared_oauth_settings = legacyBlob('fourshared_oauth_settings');
        vault[format === 'modern' ? 'oauth_fourshared_client_id' : 'config_oauth_clients'] = format === 'modern' ? 'partial-key' : JSON.stringify({ fourshared: { clientId: 'partial-key' } });
        expect(await loadFourSharedCredentials()).toEqual({ consumerKey: 'partial-key', consumerSecret: '' });
    });

    it('skips malformed or wrong-shaped records and an orphan modern secret', async () => {
        vault = { oauth_fourshared_client_secret: 'orphan-secret', config_oauth_clients: '{', config_aeroftp_oauth_settings: JSON.stringify({ fourshared: { clientId: 123 } }), fourshared_oauth_settings: legacyBlob('fourshared_oauth_settings') };
        expect(await loadFourSharedCredentials()).toEqual({ consumerKey: 'legacy-key', consumerSecret: 'legacy-secret' });
    });

    it.each(['missing', 'invalid legacy'])('returns empty fields for %s credentials', async scenario => {
        if (scenario === 'invalid legacy') vault.fourshared_oauth_settings = JSON.stringify({ consumer_key: 'key', consumer_secret: null });
        expect(await loadFourSharedCredentials()).toEqual({ consumerKey: '', consumerSecret: '' });
    });
});
