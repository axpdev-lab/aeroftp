// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The public default salt of AeroCrypt v3, surfaced in the UI (Ehud #369).
 *
 * There is exactly ONE such salt, not one per tier. It is a nothing-up-my-sleeve
 * constant, `SHA-256("AeroCrypt default salt v1")`, and it is deliberately
 * public: default-salt mode trades the per-vault random salt for portability
 * (password alone opens the vault anywhere), which is only safe because the
 * password itself has to carry the entropy. The 128-bit / 256-bit radios pick
 * how much entropy that is — they are a password requirement, NOT a salt size.
 *
 * Mirrors `AEROCRYPT_DEFAULT_SALT_V1` in `src-tauri/src/aerocrypt/mod.rs`. Both
 * sides are independently pinned to the same SHA-256 derivation by a test, so
 * neither can drift silently.
 */
export const AEROCRYPT_DEFAULT_SALT_V1_HEX =
    'cdfc274561c3c3a8771d7dbb787049133b2f3e703696a7143f5ca585c0a1ca63';

/** The preimage the salt is derived from, quoted in the UI and the docs. */
export const AEROCRYPT_DEFAULT_SALT_V1_PREIMAGE = 'AeroCrypt default salt v1';

/**
 * The salt in `xxxx xxxx …` groups of 8 hex digits, so it can be read aloud and
 * copied by hand off the screen without losing your place in 64 characters.
 */
export function formatDefaultSaltForDisplay(hex = AEROCRYPT_DEFAULT_SALT_V1_HEX): string {
    return (hex.match(/.{1,8}/g) ?? []).join(' ');
}
