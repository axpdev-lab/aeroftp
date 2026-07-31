// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import en from './locales/en.json';

/**
 * Every `t('some.key')` in the source has to resolve in en.json.
 *
 * `t()` returns the key itself when nothing resolves (`I18nContext.tsx`), so a
 * key that was never added is not a silent miss — it prints its own name at the
 * user. The delete confirmation of #537 asked for `duplicates.confirmDelete`,
 * which existed in none of the 47 locale files, so the question it put to the
 * user was the string "duplicates.confirmDelete". The sweep that found it found
 * 31 more of the same across the app.
 *
 * en.json is the right reference for all 47: `lookup()` falls back to English
 * before it gives up and returns the key, so a string that exists here is never
 * shown as a raw key in any language.
 *
 * The idiom `t('x') || 'Fallback'` does NOT protect against this — the key comes
 * back truthy, so the fallback is dead code. One of them had been holding an
 * Italian string as the English fallback for that reason.
 */

const sources = import.meta.glob('../**/*.{ts,tsx}', {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

const translations = (en as { translations: Record<string, unknown> }).translations;

const resolve = (key: string): unknown => {
    let cursor: unknown = translations;
    for (const part of key.split('.')) {
        if (typeof cursor !== 'object' || cursor === null) return undefined;
        cursor = (cursor as Record<string, unknown>)[part];
    }
    return cursor;
};

/**
 * Literal `t('a.b')` calls. Only dotted keys: a bare `t('x')` is almost always a
 * different `t` (a test helper, a tagged template), and a computed key cannot be
 * checked statically either way.
 */
const usedKeys = (): Map<string, string> => {
    const found = new Map<string, string>();
    for (const [file, source] of Object.entries(sources)) {
        if (file.includes('.test.')) continue;
        for (const match of source.matchAll(/\bt\(\s*'([a-zA-Z][\w]*(?:\.[\w]+)+)'/g)) {
            if (!found.has(match[1])) found.set(match[1], file.replace('../', 'src/i18n/../'));
        }
    }
    return found;
};

describe('no phantom translation keys (#537 sweep)', () => {
    const keys = usedKeys();

    it('collects the keys to check', () => {
        expect(keys.size).toBeGreaterThan(3000);
    });

    it('resolves every key the source asks for', () => {
        const missing = [...keys.entries()]
            .filter(([key]) => resolve(key) === undefined)
            .map(([key, file]) => `${key}  (${file})`);
        expect(missing, 'keys used in the source but absent from en.json').toEqual([]);
    });

    it('resolves every key to a string, not to a branch of the tree', () => {
        // `t()` on an object renders "[object Object]". Catching it here is the
        // same class of defect one level down.
        const notStrings = [...keys.keys()]
            .map((key) => [key, resolve(key)] as const)
            .filter(([, value]) => value !== undefined && typeof value !== 'string')
            .map(([key]) => key);
        expect(notStrings, 'keys that resolve to an object rather than a string').toEqual([]);
    });
});
