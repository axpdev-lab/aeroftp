// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { countedOAuthLogin, oauthLoginCount, oauthLoginKey } from './oauthLogins';

const SOURCES = import.meta.glob<string>(
    ['/src/**/*.ts', '/src/**/*.tsx', '!/src/**/*.test.ts', '!/src/**/*.test.tsx'],
    { query: '?raw', import: 'default', eager: true },
);

describe('the interactive OAuth logins', () => {
    it('are counted on their own token whether they succeed or fail', async () => {
        const work = oauthLoginKey('googledrive', 'work');
        const personal = oauthLoginKey('googledrive', 'personal');
        const before = oauthLoginCount(work);
        await countedOAuthLogin(work, async () => 'token');
        await expect(countedOAuthLogin(work, async () => { throw new Error('cancelled'); })).rejects.toThrow('cancelled');
        expect(oauthLoginCount(work)).toBe(before + 2);
        expect(oauthLoginCount(personal)).toBe(0);
        // The OAuth calls and the connections spell some providers differently.
        expect(oauthLoginKey('google_drive', 'work')).toBe(work);
        expect(oauthLoginKey('zoho_workdrive', null)).toBe(oauthLoginKey('zohoworkdrive', null));
    });

    // A login that is not counted lets a queued OAuth write survive an
    // account switch: every full OAuth login in the app goes through it.
    it('are all counted, wherever the app starts one', () => {
        const uncounted: string[] = [];
        for (const [file, source] of Object.entries(SOURCES)) {
            source.split('\n').forEach((line, i) => {
                if (/invoke(<[^>]*>)?\('(oauth2_full_auth|fourshared_full_auth)'/.test(line) && !line.includes('countedOAuthLogin(')) {
                    uncounted.push(`${file}:${i + 1}`);
                }
            });
        }
        expect(uncounted).toEqual([]);
    });
});
