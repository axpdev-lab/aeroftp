// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { getCredentialWithRetry } from './profileVaultSecrets';
import { isCredentialNotFound } from './oauthKeysMissing';

export interface OAuthClientCredentials {
    clientId: string;
    clientSecret: string;
}

export const oauthCredentialProvider = (provider: string): string => provider === 'googlephotos' ? 'googledrive' : provider;

/** Resolve vault app credentials with CLI precedence, keeping each app pair together. */
export async function loadOAuthClientCredentials(
    provider: string,
    readCredential: (account: string) => Promise<string> = getCredentialWithRetry,
): Promise<OAuthClientCredentials> {
    const slug = oauthCredentialProvider(provider);
    let readError: unknown = null;
    const read = async (account: string): Promise<string> => {
        try { return await readCredential(account) || ''; }
        catch (error) {
            if (!isCredentialNotFound(error)) readError ??= error;
            return '';
        }
    };
    const finish = (clientId: string, clientSecret: string): OAuthClientCredentials => {
        // A complete alternate record can recover from an unreadable key. An
        // incomplete record must not disguise that failure as missing credentials.
        if ((!clientId || !clientSecret) && readError != null) throw readError;
        return { clientId, clientSecret };
    };
    const [clientId, clientSecret] = await Promise.all([
        read(`oauth_${slug}_client_id`), read(`oauth_${slug}_client_secret`),
    ]);
    if (clientId) return finish(clientId, clientSecret);

    const accounts = ['config_oauth_clients', 'config_aeroftp_oauth_settings'];
    if (slug === 'fourshared') accounts.push('fourshared_oauth_settings');
    for (const account of accounts) {
        const json = await read(account);
        if (!json) continue;
        let settings;
        try { settings = JSON.parse(json); }
        catch { continue; } // Do not expose secret-bearing malformed JSON in an error.
        if (account === 'fourshared_oauth_settings') {
            if (typeof settings?.consumer_key === 'string' && typeof settings?.consumer_secret === 'string') {
                return finish(settings.consumer_key, settings.consumer_secret);
            }
        } else {
            const entry = settings?.[slug];
            if (typeof entry?.clientId === 'string' && entry.clientId) {
                return finish(entry.clientId, typeof entry.clientSecret === 'string' ? entry.clientSecret : '');
            }
        }
    }
    return finish('', '');
}
