// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { mapVaultError } from './useVaultState';

// mapVaultError keeps the raw text when a key comes back untranslated.
const t = (key: string) => `[${key}]`;

describe('mapVaultError', () => {
    // The texts the aerovault crate gives for a wrong password on an
    // AEROVAULT3 container (pinned on the Rust side by
    // a_wrong_password_gives_a_text_the_gui_reads_as_a_wrong_password).
    // The bare "key unwrap failed" holds the `key` alternative on its own,
    // since the full text also matches /wrong password/.
    it.each([
        'AES-KW unwrap failed',
        'key unwrap failed',
        'key unwrap failed (wrong password?)',
    ])(
        'reads %s as a wrong password',
        (raw) => {
            expect(mapVaultError(raw, t)).toBe('[vault.errors.wrongPassword]');
        },
    );
});
