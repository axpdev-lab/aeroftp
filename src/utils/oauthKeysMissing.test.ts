// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect, vi, afterEach } from 'vitest';
import en from '../i18n/locales/en.json';
import appSource from '../App.tsx?raw';
import myServersSource from '../components/IntroHub/MyServersPanel.tsx?raw';
import credentialStoreSource from '../../src-tauri/src/credential_store.rs?raw';
import libSource from '../../src-tauri/src/lib.rs?raw';
import {
    isCredentialNotFound,
    keyReadFailure,
    notifyOAuthKeysUnavailable,
    OPEN_OAUTH_SETTINGS_EVENT,
} from './oauthKeysMissing';

/**
 * Connecting a saved OAuth server (Google Drive, Dropbox, OneDrive, Box,
 * pCloud, Zoho WorkDrive, Yandex Disk, 4shared) needs the user's own app keys
 * from the vault: AeroFTP never embeds them. When they are missing the connect
 * cannot start, and that stop has to be visible: the My Servers card used to
 * clear its spinner and return, so the click did nothing at all. A vault that
 * could not be read is a different stop and must not be reported as "keys
 * missing", which would send the user to re-enter keys they may already have.
 */

// The real English strings, so a key that is missing from en.json comes back
// as its own dotted name and fails the assertions below.
const t = (key: string, params?: Record<string, string | number>): string => {
    const value = key.split('.').reduce<unknown>(
        (node, part) => (node as Record<string, unknown> | undefined)?.[part],
        en.translations,
    );
    if (typeof value !== 'string') return key;
    return value.replace(/\{(\w+)\}/g, (m, name: string) => (params && name in params ? String(params[name]) : m));
};

describe('notifyOAuthKeysUnavailable', () => {
    afterEach(() => {
        vi.unstubAllGlobals();
    });

    const capture = () => {
        const bus = new EventTarget();
        vi.stubGlobal('window', bus);
        const toasts: Array<Record<string, unknown>> = [];
        let settingsOpened = 0;
        bus.addEventListener('aeroftp-toast', e => toasts.push((e as CustomEvent).detail));
        bus.addEventListener(OPEN_OAUTH_SETTINGS_EVENT, () => { settingsOpened += 1; });
        return { toasts, settingsOpened: () => settingsOpened };
    };

    it('raises an error toast that names the provider and the place to enter the keys', () => {
        const { toasts } = capture();
        notifyOAuthKeysUnavailable(t, 'dropbox', null);

        expect(toasts).toHaveLength(1);
        const toast = toasts[0];
        expect(toast.type).toBe('error');
        // It answers a direct click, so it must show even with ambient
        // notifications turned off.
        expect(toast.important).toBe(true);
        expect(toast.title).toContain('Dropbox');
        expect(toast.message).toContain('Dropbox');
        expect(toast.message).toContain('Settings > OAuth Providers');
        expect(String(toast.message)).not.toMatch(/\{\w+\}/);
    });

    it('names 4shared (OAuth 1.0) the same way', () => {
        const { toasts } = capture();
        notifyOAuthKeysUnavailable(t, 'fourshared', null);
        expect(toasts[0].title).toContain('4shared');
    });

    it('offers an action that opens the OAuth settings', () => {
        const { toasts, settingsOpened } = capture();
        notifyOAuthKeysUnavailable(t, 'googledrive', null);

        const action = toasts[0].action as { label: string; onClick: () => void };
        expect(action.label).toBe(en.translations.connection.oauthKeysOpenSettings);
        action.onClick();
        expect(settingsOpened()).toBe(1);
    });

    it('reports a vault read failure as such, not as missing keys', () => {
        const { toasts } = capture();
        notifyOAuthKeysUnavailable(t, 'onedrive', 'STORE_NOT_READY');

        expect(toasts).toHaveLength(1);
        const toast = toasts[0];
        expect(toast.type).toBe('error');
        expect(toast.important).toBe(true);
        expect(toast.title).toBe(t('connection.oauthKeysReadFailedTitle', { provider: 'OneDrive' }));
        expect(toast.message).toContain('OneDrive');
        expect(toast.message).toContain('STORE_NOT_READY');
        expect(String(toast.message)).not.toMatch(/\{\w+\}/);
        expect(toast.title).not.toBe(t('connection.oauthKeysMissingTitle', { provider: 'OneDrive' }));
        // No "enter your keys" action: the keys may be there.
        expect(toast.action).toBeUndefined();
    });
});

describe('telling "no keys" from "vault not readable"', () => {
    const notFound = 'Failed to get credential: Credential not found: oauth_dropbox_client_id';

    it('treats only the backend not-found rejection as missing keys', () => {
        expect(isCredentialNotFound(notFound)).toBe(true);
        expect(keyReadFailure(notFound)).toBeNull();
        for (const other of [
            'STORE_NOT_READY',
            'Failed to get credential: Encryption error: aead::Error',
            'Failed to get credential: IO error: permission denied',
            'get_credential is only available to the main window',
        ]) {
            expect(isCredentialNotFound(other), other).toBe(false);
            expect(keyReadFailure(other), other).toBe(other);
        }
    });

    it('matches the strings the backend actually produces', () => {
        // `get_credential` wraps the store error, and `CredentialError::NotFound`
        // renders with this prefix. If either text changes, the predicate above
        // would silently turn every missing key into a "read failed" toast.
        expect(libSource).toContain('.map_err(|e| format!("Failed to get credential: {}", e))');
        expect(credentialStoreSource).toContain('#[error("Credential not found: {0}")]');
    });
});

/**
 * Every connect path that reads an OAuth app key from the vault must raise the
 * signal on its missing-key exit. The set is enumerated from the source rather
 * than listed by hand, so a new connect path that reads the keys and bails out
 * quietly fails here.
 */
describe('every saved-server connect path signals missing OAuth app keys', () => {
    // A vault read of an OAuth client id (the 4shared consumer key is stored
    // under the same `oauth_<provider>_client_id` shape).
    const KEY_READ = /(?:getCredentialWithRetry\(\s*|'get_credential',\s*\{\s*account:\s*)[`'"]oauth_[^`'"]*_client_id[`'"]/g;

    /** The first `if (!...) { ... }` after `from`: the missing-key exit. */
    const missingKeyBlock = (source: string, from: number): string => {
        const start = source.indexOf('if (!', from);
        const open = source.indexOf('{', start);
        let depth = 0;
        for (let i = open; i < source.length; i++) {
            if (source[i] === '{') depth++;
            else if (source[i] === '}' && --depth === 0) return source.slice(start, i + 1);
        }
        throw new Error('unbalanced block');
    };

    const sites = (source: string) => [...source.matchAll(KEY_READ)].map(m => {
        const after = m.index! + m[0].length;
        const exit = missingKeyBlock(source, after);
        return { read: m[0], exit, between: source.slice(after, source.indexOf(exit, after)) };
    });

    const surfaces: Array<[string, string, number]> = [
        // OAuth 2.0 card connect, 4shared card connect.
        ['MyServersPanel.tsx', myServersSource, 2],
        // Session-tab reconnect (switchSession), resume connect for OAuth 2.0
        // and for 4shared.
        ['App.tsx', appSource, 3],
    ];

    for (const [name, source, expected] of surfaces) {
        it(`${name}: ${expected} key reads, each with a signalled exit`, () => {
            const found = sites(source);
            expect(found, `${name}: key read sites`).toHaveLength(expected);
            for (const site of found) {
                expect(site.exit, `${name} after ${site.read}`).toMatch(/\b(return|throw)\b/);
                // The exit passes on what the read's catch kept, so a vault
                // failure is not reported as missing keys.
                expect(site.exit, `${name} after ${site.read}`).toMatch(/notifyOAuthKeysUnavailable\(t, [^,]+, keyReadError\)/);
                expect(site.between, `${name} after ${site.read}`).toContain('keyReadError = keyReadFailure(e)');
            }
        });
    }
});
