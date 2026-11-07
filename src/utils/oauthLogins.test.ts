// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { countedOAuthLogin, oauthLoginCount } from './oauthLogins';

const SOURCES = import.meta.glob<string>(
    ['/src/**/*.ts', '/src/**/*.tsx', '!/src/**/*.test.ts', '!/src/**/*.test.tsx'],
    { query: '?raw', import: 'default', eager: true },
);

describe('the interactive OAuth logins', () => {
    it('are counted whether they succeed or fail', async () => {
        const before = oauthLoginCount();
        await countedOAuthLogin(async () => 'token');
        await expect(countedOAuthLogin(async () => { throw new Error('cancelled'); })).rejects.toThrow('cancelled');
        expect(oauthLoginCount()).toBe(before + 2);
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
