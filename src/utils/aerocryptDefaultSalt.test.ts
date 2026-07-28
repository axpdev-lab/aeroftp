// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import {
    AEROCRYPT_DEFAULT_SALT_V1_HEX,
    AEROCRYPT_DEFAULT_SALT_V1_PREIMAGE,
    formatDefaultSaltForDisplay,
} from './aerocryptDefaultSalt';

async function sha256Hex(input: string): Promise<string> {
    const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(input));
    return Array.from(new Uint8Array(digest))
        .map((b) => b.toString(16).padStart(2, '0'))
        .join('');
}

describe('AeroCrypt default salt', () => {
    it('is exactly SHA-256 of its published preimage', async () => {
        // Same pin as the Rust side (`default_salt_constant_is_nothing_up_my_sleeve`
        // in src-tauri/src/aerocrypt/mod.rs): the UI must never show a salt the
        // backend does not actually use.
        expect(AEROCRYPT_DEFAULT_SALT_V1_HEX).toBe(
            await sha256Hex(AEROCRYPT_DEFAULT_SALT_V1_PREIMAGE),
        );
    });

    it('is a 32-byte value, not a trap constant', () => {
        expect(AEROCRYPT_DEFAULT_SALT_V1_HEX).toMatch(/^[0-9a-f]{64}$/);
        expect(AEROCRYPT_DEFAULT_SALT_V1_HEX).not.toBe('0'.repeat(64));
        expect(AEROCRYPT_DEFAULT_SALT_V1_HEX).not.toBe('f'.repeat(64));
    });

    it('groups the display form without changing the value', () => {
        const shown = formatDefaultSaltForDisplay();
        expect(shown.replace(/ /g, '')).toBe(AEROCRYPT_DEFAULT_SALT_V1_HEX);
        expect(shown.split(' ')).toHaveLength(8);
    });
});
