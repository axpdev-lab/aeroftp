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
 * Surface the "your app keys are missing" signal for an OAuth provider
 * (OAuth 2.0 or 4shared OAuth 1.0) whose Client ID / Client Secret are not in
 * the vault. AeroFTP never ships embedded app keys: every user registers their
 * own developer app, so a missing key is a setup step the user has to take,
 * and a connect that stops here must say so and point to where the keys go
 * instead of looking like a click that did nothing.
 *
 * Goes through the `aeroftp-toast` bus (marked `important`, so it shows even
 * with ambient notifications off: it answers a direct click) with an action
 * that opens Settings > OAuth Providers.
 */
export function notifyOAuthKeysMissing(t: TranslationFunction, protocol: string): void {
    const provider = oauthProviderDisplayName(protocol);
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
