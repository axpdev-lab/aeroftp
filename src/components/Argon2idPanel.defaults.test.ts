// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { HASH_FORGE_ARGON2_DEFAULTS } from './Argon2idPanel';

// Mirror of aerocrypt::audited_argon2_profile (128 * 1024 KiB, t=4, p=4)
// and of the 32-byte tag. A drift here is a drift from AeroVault / AeroCrypt.
describe('Hash Forge Argon2id defaults', () => {
    it('matches the audited AeroVault and AeroCrypt profile', () => {
        expect(HASH_FORGE_ARGON2_DEFAULTS).toEqual({
            memoryKib: 128 * 1024,
            iterations: 4,
            parallelism: 4,
            outputLen: 32,
        });
    });
});
