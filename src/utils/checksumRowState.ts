// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

/** What one row of the Checksum tab shows when it has no value yet. */
type ChecksumRowState = 'server-only' | 'not-on-backend' | 'calculate';

/**
 * Decide what an empty Checksum row offers.
 *
 * `available` is the set of algorithms the backend can produce for this file
 * (the `provider_checksum_capability` reply), or `null` for a local file,
 * which AeroFTP reads itself. A backend's own digest (`serverOnly`: CRC32,
 * QuickXor, Dropbox...) cannot be computed locally.
 */
export function checksumRowState(
    algorithm: string | undefined,
    serverOnly: boolean,
    available: ReadonlySet<string> | null,
): ChecksumRowState {
    if (serverOnly) {
        // Asked for by name when the backend says it can produce it: FTP
        // computes each digest on request and returns only the one requested.
        return algorithm && available?.has(algorithm) ? 'calculate' : 'server-only';
    }
    if (algorithm && available !== null && !available.has(algorithm)) return 'not-on-backend';
    return 'calculate';
}
