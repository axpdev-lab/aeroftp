// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';

interface FourSharedCredentials {
    consumerKey: string;
    consumerSecret: string;
}

const emptyCredentials = (): FourSharedCredentials => ({ consumerKey: '', consumerSecret: '' });
const readCredential = (account: string) => invoke<string>('get_credential', { account }).catch(() => '');

// Match the CLI's vault-only precedence. Keep each key/secret pair together:
// a partial newer record must never borrow a secret from another developer app.
export async function loadFourSharedCredentials(): Promise<FourSharedCredentials> {
    const [consumerKey, consumerSecret] = await Promise.all([
        readCredential('oauth_fourshared_client_id'),
        readCredential('oauth_fourshared_client_secret'),
    ]);
    if (consumerKey) return { consumerKey, consumerSecret: consumerSecret || '' };

    for (const account of ['config_oauth_clients', 'config_aeroftp_oauth_settings', 'fourshared_oauth_settings']) {
        const json = await readCredential(account);
        if (!json) continue;
        try {
            const settings = JSON.parse(json);
            if (account === 'fourshared_oauth_settings') {
                if (typeof settings?.consumer_key === 'string' && typeof settings?.consumer_secret === 'string') {
                    return { consumerKey: settings.consumer_key, consumerSecret: settings.consumer_secret };
                }
            } else {
                const entry = settings?.fourshared;
                if (typeof entry?.clientId === 'string' && entry.clientId) {
                    return { consumerKey: entry.clientId, consumerSecret: typeof entry.clientSecret === 'string' ? entry.clientSecret : '' };
                }
            }
        } catch {
            // An invalid legacy blob must not hide credentials in the next format.
        }
    }
    return emptyCredentials();
}
