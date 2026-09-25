// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { blake3Args } from './hashForgeArgs';

const KEY = '77686174732074686520456c7669736820776f726420666f7220667269656e64';

describe('blake3Args', () => {
    it('sends the key only in keyed mode and the context only in derive-key mode', () => {
        expect(blake3Args('blake3', 'keyed', KEY, 'ctx')).toEqual({ blake3Key: KEY, blake3Context: null });
        expect(blake3Args('blake3', 'derive', KEY, 'ctx')).toEqual({ blake3Key: null, blake3Context: 'ctx' });
        expect(blake3Args('blake3', 'hash', KEY, 'ctx')).toEqual({ blake3Key: null, blake3Context: null });
    });

    it('never sends a BLAKE3 key or context with another algorithm', () => {
        for (const algorithm of ['md5', 'sha1', 'sha256', 'sha512']) {
            expect(blake3Args(algorithm, 'keyed', KEY, '')).toEqual({ blake3Key: null, blake3Context: null });
            expect(blake3Args(algorithm, 'derive', '', 'ctx')).toEqual({ blake3Key: null, blake3Context: null });
        }
    });

    it('computes nothing while the selected mode is missing its input', () => {
        expect(blake3Args('blake3', 'keyed', '', '')).toBeNull();
        expect(blake3Args('blake3', 'keyed', '   ', '')).toBeNull();
        expect(blake3Args('blake3', 'derive', '', '')).toBeNull();
    });
});
