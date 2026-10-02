// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect, vi, afterEach } from 'vitest';
import en from '../i18n/locales/en.json';
import appSource from '../App.tsx?raw';
import myServersSource from '../components/IntroHub/MyServersPanel.tsx?raw';
import { notifyOAuthKeysMissing, OPEN_OAUTH_SETTINGS_EVENT } from './oauthKeysMissing';

/**
 * Connecting a saved OAuth server (Google Drive, Dropbox, OneDrive, Box,
 * pCloud, Zoho WorkDrive, Yandex Disk, 4shared) needs the user's own app keys
 * from the vault: AeroFTP never embeds them. When they are missing the connect
 * cannot start, and that stop has to be visible: the My Servers card used to
 * clear its spinner and return, so the click did nothing at all.
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

describe('notifyOAuthKeysMissing', () => {
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
        notifyOAuthKeysMissing(t, 'dropbox');

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
        notifyOAuthKeysMissing(t, 'fourshared');
        expect(toasts[0].title).toContain('4shared');
    });

    it('offers an action that opens the OAuth settings', () => {
        const { toasts, settingsOpened } = capture();
        notifyOAuthKeysMissing(t, 'googledrive');

        const action = toasts[0].action as { label: string; onClick: () => void };
        expect(action.label).toBe(en.translations.connection.oauthKeysOpenSettings);
        action.onClick();
        expect(settingsOpened()).toBe(1);
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

    const sites = (source: string) => [...source.matchAll(KEY_READ)].map(m => ({
        read: m[0],
        exit: missingKeyBlock(source, m.index! + m[0].length),
    }));

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
                expect(site.exit, `${name} after ${site.read}`).toContain('notifyOAuthKeysMissing(');
            }
        });
    }
});
