// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { readFileSync } from 'fs';
import { fileURLToPath } from 'url';
import path from 'path';
import { DEFAULT_PATTERN_MESSAGE_KEY, fieldPatternIssue } from './fieldValidation';
import { PROVIDERS } from './registry';
import type { ProviderField } from './types';

const field = (partial: Partial<ProviderField>): ProviderField => ({
    key: 'username',
    label: 'Username',
    type: 'text',
    required: true,
    ...partial,
});

describe('fieldPatternIssue', () => {
    it('returns null for a field without a pattern', () => {
        expect(fieldPatternIssue(field({}), 'anything')).toBeNull();
        expect(fieldPatternIssue(undefined, 'anything')).toBeNull();
    });

    it('returns null for an empty value, whatever the pattern', () => {
        expect(fieldPatternIssue(field({ pattern: '^[^@]*$' }), '')).toBeNull();
        expect(fieldPatternIssue(field({ pattern: '^[^@]*$' }), undefined)).toBeNull();
    });

    it('returns null when the value matches the pattern', () => {
        expect(fieldPatternIssue(field({ pattern: '^[^@]*$' }), 'AeroFTP')).toBeNull();
    });

    it('returns the field message key when the value does not match', () => {
        expect(fieldPatternIssue(field({ pattern: '^[^@]*$', patternMessageKey: 'connection.puterUsernameIsEmail' }), 'ale@example.com'))
            .toBe('connection.puterUsernameIsEmail');
    });

    it('falls back to the default message key when the field has none', () => {
        expect(fieldPatternIssue(field({ pattern: '^[^@]*$' }), 'ale@example.com')).toBe(DEFAULT_PATTERN_MESSAGE_KEY);
    });

    it('returns null when the pattern does not compile', () => {
        expect(fieldPatternIssue(field({ pattern: '([' }), 'anything')).toBeNull();
    });

    it('checks the real Puter username field from the registry', () => {
        const puter = PROVIDERS.find(p => p.id === 'puter-webdav');
        const usernameField = puter?.fields.find(f => f.key === 'username');
        expect(usernameField).toBeDefined();
        expect(fieldPatternIssue(usernameField, 'ale@example.com')).toBe('connection.puterUsernameIsEmail');
        expect(fieldPatternIssue(usernameField, 'AeroFTP')).toBeNull();
        expect(fieldPatternIssue(usernameField, '-token')).toBeNull();
    });
});

describe('registry pattern guard', () => {
    const enJson = JSON.parse(readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), '../i18n/locales/en.json'), 'utf-8'));
    const connectionKeys = new Set(Object.keys(enJson.translations.connection ?? {}));

    it('every registry field pattern compiles and its message key exists in en.json', () => {
        const withPattern: Array<{ provider: string; key: string }> = [];
        for (const provider of PROVIDERS) {
            for (const f of provider.fields ?? []) {
                if (!f.pattern) continue;
                withPattern.push({ provider: provider.id, key: f.key });
                expect(() => new RegExp(f.pattern as string), `${provider.id}.${f.key} pattern must compile`).not.toThrow();
                const messageKey = (f.patternMessageKey ?? DEFAULT_PATTERN_MESSAGE_KEY).replace(/^connection\./, '');
                expect(connectionKeys.has(messageKey), `${provider.id}.${f.key} message key connection.${messageKey} must exist in en.json`).toBe(true);
            }
        }
        expect(withPattern.length).toBeGreaterThan(0);
    });
});

describe('approved username/key shape patterns (MPUT01 R2 Part B)', () => {
    const rows: Array<{ provider: string; fieldKey: string; accept: string; reject: string }> = [
        { provider: 'koofr', fieldKey: 'username', accept: 'user@koofr.net', reject: 'plainname' },
        { provider: 'mega', fieldKey: 'username', accept: 'user@example.com', reject: 'plainname' },
        { provider: '4shared-webdav', fieldKey: 'username', accept: 'user@example.com', reject: 'plainname' },
        { provider: 'opendrive-webdav', fieldKey: 'username', accept: 'user@example.com', reject: 'plainname' },
        { provider: 'blomp', fieldKey: 'username', accept: 'user@blomp.com', reject: 'plainname' },
        { provider: 'infinicloud', fieldKey: 'username', accept: 'user@example.com', reject: 'plainname' },
        { provider: 'cloudme', fieldKey: 'username', accept: 'plainname', reject: 'user@example.com' },
        { provider: 'amazon-s3', fieldKey: 'username', accept: 'AKIAIOSFODNN7EXAMPLE', reject: 'AKIAIOSFODNN7EXAMPL' },
        { provider: 'cloudflare-r2', fieldKey: 'accountId', accept: 'a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4', reject: 'a1b2c3d4' },
    ];
    for (const row of rows) {
        it(`${row.provider}.${row.fieldKey} accepts the documented shape and rejects the wrong one`, () => {
            const provider = PROVIDERS.find(p => p.id === row.provider);
            const field = provider?.fields?.find(f => f.key === row.fieldKey);
            expect(field?.pattern, `${row.provider}.${row.fieldKey} must carry a pattern`).toBeTruthy();
            expect(fieldPatternIssue(field, row.accept)).toBeNull();
            expect(fieldPatternIssue(field, row.reject)).not.toBeNull();
        });
    }
    it('the email rows share one message key and the shapes their own', () => {
        const keyOf = (id: string, key: string) => PROVIDERS.find(p => p.id === id)?.fields?.find(f => f.key === key)?.patternMessageKey;
        for (const id of ['koofr', 'mega', '4shared-webdav', 'opendrive-webdav', 'blomp', 'infinicloud']) {
            expect(keyOf(id, 'username')).toBe('connection.fieldExpectsEmail');
        }
        expect(keyOf('cloudme', 'username')).toBe('connection.fieldNotEmail');
        expect(keyOf('amazon-s3', 'username')).toBe('connection.fieldAwsAccessKeyId');
        expect(keyOf('cloudflare-r2', 'accountId')).toBe('connection.fieldR2AccountId');
    });
    it('all email rows share the very same pattern string', () => {
        const pats = ['koofr', 'mega', '4shared-webdav', 'opendrive-webdav', 'blomp', 'infinicloud']
            .map(id => PROVIDERS.find(p => p.id === id)?.fields?.find(f => f.key === 'username')?.pattern);
        expect(new Set(pats).size).toBe(1);
        expect(pats[0]).toMatch(/@/);
    });
});
