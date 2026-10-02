// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { TranslationFunction } from '../i18n';

/**
 * Event that asks App to open Settings on the OAuth Providers tab. Dispatched
 * by the action button of the "app keys missing" toast below, so a component
 * outside App (the My Servers panel) can send the user to the keys without
 * plumbing a settings opener through props.
 */
export const OPEN_OAUTH_SETTINGS_EVENT = 'aeroftp-open-oauth-settings';

const OAUTH_PROVIDER_NAMES: Record<string, string> = {
    googledrive: 'Google Drive',
    dropbox: 'Dropbox',
    onedrive: 'OneDrive',
    box: 'Box',
    pcloud: 'pCloud Drive',
    zohoworkdrive: 'Zoho WorkDrive',
    yandexdisk: 'Yandex Disk',
    fourshared: '4shared',
};

export const oauthProviderDisplayName = (protocol: string): string =>
    OAUTH_PROVIDER_NAMES[protocol] || protocol;

/**
 * True when a `get_credential` rejection means "this key is not in the vault".
 * The backend renders `CredentialError::NotFound` as
 * `Failed to get credential: Credential not found: <account>`; every other
 * rejection (`STORE_NOT_READY`, I/O, decryption) is a vault that could not be
 * read, where the keys may well exist.
 */
export const isCredentialNotFound = (err: unknown): boolean =>
    /^Failed to get credential: Credential not found: /.test(String(err));

/**
 * Keep a key-read rejection only when it is a real read failure, so the caller
 * can tell "no keys" (`null`) from "could not read the vault" (the error).
 */
export const keyReadFailure = (err: unknown): unknown =>
    (isCredentialNotFound(err) ? null : err);

/**
 * Surface why an OAuth provider (OAuth 2.0 or 4shared OAuth 1.0) cannot start
 * a connect because its app keys (Client ID / Client Secret) are unavailable.
 * A connect that stops here must say so instead of looking like a click that
 * did nothing.
 *
 * - `readError` null: the keys are not in the vault. AeroFTP never ships
 *   embedded app keys, every user registers their own developer app, so this
 *   is a setup step: the toast names the provider, says where the keys go and
 *   offers an action that opens Settings > OAuth Providers.
 * - `readError` set: the vault could not be read. Telling the user to enter
 *   keys they may already have would be wrong, so the toast reports the read
 *   failure with the error instead.
 *
 * Goes through the `aeroftp-toast` bus, marked `important` so it shows even
 * with ambient notifications off: it answers a direct click.
 */
export function notifyOAuthKeysUnavailable(
    t: TranslationFunction,
    protocol: string,
    readError: unknown,
): void {
    const provider = oauthProviderDisplayName(protocol);
    if (readError != null) {
        const error = readError instanceof Error ? readError.message : String(readError);
        window.dispatchEvent(new CustomEvent('aeroftp-toast', {
            detail: {
                type: 'error',
                title: t('connection.oauthKeysReadFailedTitle', { provider }),
                message: t('connection.oauthKeysReadFailed', { provider, error }),
                duration: 12000,
                important: true,
            },
        }));
        return;
    }
    const location = `${t('settings.title')} > ${t('settings.oauthProviders')}`;
    window.dispatchEvent(new CustomEvent('aeroftp-toast', {
        detail: {
            type: 'error',
            title: t('connection.oauthKeysMissingTitle', { provider }),
            message: t('connection.oauthKeysMissing', { provider, location }),
            duration: 12000,
            important: true,
            action: {
                label: t('connection.oauthKeysOpenSettings'),
                onClick: () => window.dispatchEvent(new CustomEvent(OPEN_OAUTH_SETTINGS_EVENT)),
            },
        },
    }));
}
