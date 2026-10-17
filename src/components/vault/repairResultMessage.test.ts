// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import english from '../../i18n/locales/en.json';
import { repairResultMessage } from './repairResultMessage';

// The English strings with their {placeholders} filled, as the app shows them.
const t = (key: string, params?: Record<string, string | number>) => {
    const value = key.split('.').reduce<unknown>((node, part) => (node as Record<string, unknown>)?.[part], english.translations);
    if (typeof value !== 'string') throw new Error(`missing key ${key}`);
    return value.replace(/\{(\w+)\}/g, (match, name) => (params && name in params ? String(params[name]) : match));
};

describe('the repair dialog says what a repair or a dry run found', () => {
    it('reports a real repair', () => {
        expect(repairResultMessage({ repaired: 0, damaged: 0, dry_run: false }, t)).toBe('No damage; nothing to repair.');
        expect(repairResultMessage({ repaired: 0, damaged: 3, dry_run: false }, t)).toBe('Could not repair any of 3 (vault left untouched).');
        expect(repairResultMessage({ repaired: 3, damaged: 3, dry_run: false }, t)).toBe('Successfully repaired 3 chunk(s).');
    });

    it('reports a dry run as a preview that wrote nothing', () => {
        expect(repairResultMessage({ repaired: 0, damaged: 0, dry_run: true }, t)).toBe('Dry run: no damage; nothing to repair.');
        expect(repairResultMessage({ repaired: 0, damaged: 3, dry_run: true }, t)).toBe(
            'Dry run: none of 3 damaged chunk(s) can be repaired. Nothing was written.',
        );
        expect(repairResultMessage({ repaired: 3, damaged: 3, dry_run: true }, t)).toBe(
            'Dry run: 3 damaged chunk(s) can be repaired. Nothing was written.',
        );
    });

    // aerovault 0.6.6 repair_vault returns (1, None) with no damaged data block
    // when open rebuilt the header or manifest from parity; a real repair then
    // re-seals the vault, so "no damage" would be false.
    it('does not call a rebuilt header or manifest "no damage"', () => {
        expect(repairResultMessage({ repaired: 1, damaged: 0, dry_run: false }, t)).toBe(
            'Rebuilt the vault header or manifest from parity.',
        );
        expect(repairResultMessage({ repaired: 1, damaged: 0, dry_run: true }, t)).toBe(
            'Dry run: the vault header or manifest can be rebuilt from parity. Nothing was written.',
        );
    });

    it('treats missing counts as zero', () => {
        expect(repairResultMessage({}, t)).toBe('No damage; nothing to repair.');
    });
});
