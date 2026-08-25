// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { discoveryResetKey } from './DiscoverableTargetField';

describe('discoveryResetKey', () => {
    it('changes when any credential input changes without returning the input', () => {
        const first = discoveryResetKey('account', 'secret-one', 'https://s3.example');
        const second = discoveryResetKey('account', 'secret-two', 'https://s3.example');
        expect(first).not.toBe(second);
        expect(first).not.toContain('secret-one');
    });

    it('is stable for an unchanged form', () => {
        expect(discoveryResetKey('a', 'b')).toBe(discoveryResetKey('a', 'b'));
    });
});
