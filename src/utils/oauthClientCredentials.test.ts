// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
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
afterEach(() => vi.useRealTimers());

describe.each(['googledrive', 'googlephotos', 'dropbox', 'onedrive', 'box', 'pcloud', 'zohoworkdrive', 'yandexdisk', 'fourshared'])('%s vault app credentials', provider => {
    it.each(['modern', 'config_oauth_clients', 'config_aeroftp_oauth_settings'])('reads a complete pair from %s', async format => {
        const slug = oauthCredentialProvider(provider);
        vault = format === 'modern'
            ? { [`oauth_${slug}_client_id`]: 'fixture-id', [`oauth_${slug}_client_secret`]: 'fixture-secret' }
            : { [format]: JSON.stringify({ [slug]: { clientId: 'fixture-id', clientSecret: 'fixture-secret' } }) };
        expect(await loadOAuthClientCredentials(provider)).toEqual({ clientId: 'fixture-id', clientSecret: 'fixture-secret' });
    });
});

describe('OAuth app pair precedence and read failures', () => {
    it('prefers modern keys over either legacy format', async () => {
        vault = { oauth_box_client_id: 'new-id', oauth_box_client_secret: 'new-secret', config_oauth_clients: JSON.stringify({ box: { clientId: 'old-id', clientSecret: 'old-secret' } }) };
        expect(await loadOAuthClientCredentials('box')).toEqual({ clientId: 'new-id', clientSecret: 'new-secret' });
        expect(invoke).toHaveBeenCalledTimes(2);
    });
    it('keeps a partial newer app pair separate from a complete older app', async () => {
        vault = { oauth_box_client_id: 'new-id', config_oauth_clients: JSON.stringify({ box: { clientId: 'old-id', clientSecret: 'old-secret' } }) };
        expect(await loadOAuthClientCredentials('box')).toEqual({ clientId: 'new-id', clientSecret: '' });
    });
    it('recovers using a complete legacy pair after a modern read failure', async () => {
        invoke.mockRejectedValueOnce(new Error('Vault I/O failure'));
        vault.config_oauth_clients = JSON.stringify({ box: { clientId: 'old-id', clientSecret: 'old-secret' } });
        expect(await loadOAuthClientCredentials('box')).toEqual({ clientId: 'old-id', clientSecret: 'old-secret' });
    });
    it('does not disguise an unreadable vault as empty fields', async () => {
        invoke.mockRejectedValue(new Error('Vault I/O failure'));
        await expect(loadOAuthClientCredentials('box')).rejects.toThrow('Vault I/O failure');
    });
    it('does not hide a secret read error behind a partial modern app', async () => {
        vault.oauth_box_client_id = 'id';
        const previous = invoke.getMockImplementation()!;
        invoke.mockImplementation((command: string, args: { account: string }) => args.account.endsWith('_client_secret') ? Promise.reject(new Error('Vault I/O failure')) : previous(command, args));
        await expect(loadOAuthClientCredentials('box')).rejects.toThrow('Vault I/O failure');
    });
    it('retries STORE_NOT_READY and then returns the existing modern keys', async () => {
        vi.useFakeTimers();
        invoke.mockRejectedValueOnce(new Error('STORE_NOT_READY')).mockRejectedValueOnce(new Error('STORE_NOT_READY'));
        vault = { oauth_box_client_id: 'id', oauth_box_client_secret: 'secret' };
        const result = loadOAuthClientCredentials('box');
        await vi.advanceTimersByTimeAsync(1000);
        expect(await result).toEqual({ clientId: 'id', clientSecret: 'secret' });
        expect(invoke).toHaveBeenCalledTimes(4);
    });
    it('skips malformed and wrong-shaped records without exposing their contents', async () => {
        vault = { config_oauth_clients: '{secret-bearing-invalid-json', config_aeroftp_oauth_settings: JSON.stringify({ box: { clientId: 12, clientSecret: 'secret' } }) };
        expect(await loadOAuthClientCredentials('box')).toEqual({ clientId: '', clientSecret: '' });
    });
    it('returns an empty pair for genuinely absent keys', async () => {
        expect(await loadOAuthClientCredentials('box')).toEqual({ clientId: '', clientSecret: '' });
    });
});
