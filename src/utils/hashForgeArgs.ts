// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** The three BLAKE3 modes, as `b3sum` names them: plain, `--keyed`, `--derive-key`. */
export const BLAKE3_MODES = ['hash', 'keyed', 'derive'] as const;
export type Blake3Mode = (typeof BLAKE3_MODES)[number];

export interface Blake3Args {
    blake3Key: string | null;
    blake3Context: string | null;
}

/**
 * The BLAKE3 mode arguments for `hash_text` / `hash_file`, or `null` when the
 * selected mode is missing its key or context and nothing should be computed.
 *
 * The mode survives a switch to another algorithm in the UI, so the arguments
 * are sent only while BLAKE3 is selected: the backend refuses a key or context
 * for any other algorithm. And a keyed or derive-key hash without its input is
 * not a result: showing the plain digest there would pass for a MAC.
 */
export function blake3Args(
    algorithm: string,
    mode: Blake3Mode,
    key: string,
    context: string,
): Blake3Args | null {
    if (algorithm !== 'blake3' || mode === 'hash') return { blake3Key: null, blake3Context: null };
    if (mode === 'keyed') return key.trim() ? { blake3Key: key, blake3Context: null } : null;
    return context ? { blake3Key: null, blake3Context: context } : null;
}
