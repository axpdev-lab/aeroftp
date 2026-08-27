// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// Validator tests for the UI token overrides (docs/UI-TOKENS.md). Validation
// is reject, not sanitise; these pin the published-list whitelist, the shape
// and range checks, the forbidden-content drop, the silent missing-file case,
// and that reset() only touches published properties.

import { describe, expect, it, vi, beforeEach } from 'vitest';

const mockInvoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
    invoke: (cmd: string, args?: unknown) => mockInvoke(cmd, args),
}));

import {
    PUBLISHED_UI_TOKEN_NAMES,
    loadUiTokenOverrides,
    resetUiTokenOverrides,
    validateUiTokenOverrides,
} from './useUiTokens';

describe('validateUiTokenOverrides', () => {
    it('drops an unknown key', () => {
        const result = validateUiTokenOverrides({ '--color-success': '#00ff00' });
        expect(result.accepted).toEqual({});
        expect(result.rejected).toHaveLength(1);
        expect(result.rejected[0].key).toBe('--color-success');
        expect(result.rejected[0].reason).toContain('unknown token');
    });

    it('accepts a published key with a valid value', () => {
        const result = validateUiTokenOverrides({
            '--aeroftp-scrollbar-width': '12px',
            '--aeroftp-scrollbar-radius': '0px',
            '--aeroftp-scrollbar-thumb': 'rgba(200, 200, 200, 0.4)',
            '--color-accent': '#ff8800',
            '--color-bg-primary': '#fff',
        });
        expect(result.rejected).toEqual([]);
        expect(result.accepted).toEqual({
            '--aeroftp-scrollbar-width': '12px',
            '--aeroftp-scrollbar-radius': '0px',
            '--aeroftp-scrollbar-thumb': 'rgba(200, 200, 200, 0.4)',
            '--color-accent': '#ff8800',
            '--color-bg-primary': '#fff',
        });
    });

    it('drops a length outside the allowed range', () => {
        const tooWide = validateUiTokenOverrides({ '--aeroftp-scrollbar-width': '9999px' });
        expect(tooWide.accepted).toEqual({});
        expect(tooWide.rejected[0].key).toBe('--aeroftp-scrollbar-width');

        const tooThin = validateUiTokenOverrides({ '--aeroftp-scrollbar-width': '1px' });
        expect(tooThin.accepted).toEqual({});
        expect(tooThin.rejected).toHaveLength(1);

        const notALength = validateUiTokenOverrides({ '--aeroftp-scrollbar-radius': 'wide' });
        expect(notALength.accepted).toEqual({});
        expect(notALength.rejected).toHaveLength(1);
    });

    it('drops a value containing url( regardless of anything else', () => {
        const result = validateUiTokenOverrides({
            '--aeroftp-scrollbar-thumb': 'url(https://example.invalid/x) 12px',
        });
        expect(result.accepted).toEqual({});
        expect(result.rejected).toHaveLength(1);
        expect(result.rejected[0].key).toBe('--aeroftp-scrollbar-thumb');
        expect(result.rejected[0].reason).toContain('forbidden content');
    });

    it('drops a malformed colour', () => {
        for (const bad of ['reddish', '#ff', '#ffff', 'rgb(300, 0, 0)', 'rgba(10, 20, 30, 2)', 'rgba(10, 20, 30)']) {
            const result = validateUiTokenOverrides({ '--color-accent': bad });
            expect(result.accepted, `value ${bad} must be rejected`).toEqual({});
            expect(result.rejected).toHaveLength(1);
            expect(result.rejected[0].key).toBe('--color-accent');
        }
    });

    it('drops a non-object input with a clear reason', () => {
        for (const bad of ['a string', 42, ['--color-accent'], null]) {
            const result = validateUiTokenOverrides(bad);
            expect(result.accepted).toEqual({});
            expect(result.rejected).toHaveLength(1);
            expect(result.rejected[0].reason).toContain('JSON object');
        }
    });
});

describe('loadUiTokenOverrides', () => {
    beforeEach(() => {
        mockInvoke.mockReset();
    });

    it('a missing file yields no overrides and no error', async () => {
        // The backend returns null for "absent", which is the normal case.
        mockInvoke.mockResolvedValue(null);

        const result = await loadUiTokenOverrides();

        expect(result).toBeNull();
        expect(mockInvoke).toHaveBeenCalledWith('read_ui_tokens_file', undefined);
    });

    it('a read failure is reported, not silently treated as absent', async () => {
        // The distinction that matters: a permission or path problem must not
        // look like a clean run with no overrides. That conflation is how the
        // fs-scope bug survived its first live test.
        mockInvoke.mockRejectedValue(new Error('Cannot read /x/ui-tokens.json: denied'));

        const result = await loadUiTokenOverrides();

        expect(result).not.toBeNull();
        expect(result?.accepted).toEqual({});
        expect(result?.rejected).toHaveLength(1);
        expect(result?.rejected[0].key).toBe('(file)');
        expect(result?.rejected[0].reason).toContain('cannot read');
    });

    it('a valid file is parsed and validated', async () => {
        mockInvoke.mockResolvedValue('{"--aeroftp-scrollbar-width":"12px","--nope":"1px"}');

        const result = await loadUiTokenOverrides();

        expect(result?.accepted).toEqual({ '--aeroftp-scrollbar-width': '12px' });
        expect(result?.rejected.map((r) => r.key)).toEqual(['--nope']);
    });

    it('malformed JSON is reported as a rejection, not a crash', async () => {
        mockInvoke.mockResolvedValue('{ not json');

        const result = await loadUiTokenOverrides();

        expect(result?.accepted).toEqual({});
        expect(result?.rejected[0].reason).toContain('not valid JSON');
    });
});

describe('resetUiTokenOverrides', () => {
    it('removes every published property and leaves unpublished ones untouched', () => {
        // The unit tests run under `environment: 'node'` (vitest.config.ts), so
        // the style target is a minimal stub rather than a DOM implementation.
        const store = new Map<string, string>();
        const style = {
            setProperty: (name: string, value: string): void => { store.set(name, value); },
            removeProperty: (name: string): string => {
                const old = store.get(name) ?? '';
                store.delete(name);
                return old;
            },
        };

        for (const name of PUBLISHED_UI_TOKEN_NAMES) {
            style.setProperty(name, '1px');
        }
        style.setProperty('--color-success', '#00ff00');
        style.setProperty('--aeroftp-unpublished', 'keep-me');

        resetUiTokenOverrides(style);

        for (const name of PUBLISHED_UI_TOKEN_NAMES) {
            expect(store.has(name), `${name} must be removed`).toBe(false);
        }
        expect(store.get('--color-success')).toBe('#00ff00');
        expect(store.get('--aeroftp-unpublished')).toBe('keep-me');
    });
});
