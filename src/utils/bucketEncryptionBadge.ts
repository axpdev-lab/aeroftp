// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { ServerProfile } from '../types';

/**
 * What the saved-server card may show about a bucket's default server-side
 * encryption.
 *
 * Four states reach us and three answers leave, which is the whole reason this
 * is a function and not an inline condition. `on` earns a lock with the type
 * and the strength B2 named. The other two earn NO lock, and no crossed-out
 * lock either: "no default encryption" is not "not encrypted", because SSE-C
 * and client-side encryption are not reported by that field, and "we could not
 * read it" asserts nothing at all. What separates those two is the tooltip, and
 * the tooltip key depends on WHY we do not know, because naming a missing
 * capability is defensible only when the key really lacked it.
 */
export type BucketEncryptionBadge = {
    /** Text beside the lock, e.g. `SSE-B2 256`. `null` means: draw no lock. */
    label: string | null;
    /** i18n key for the tooltip, or `null` when there is nothing cached. */
    titleKey: string | null;
};

/**
 * Bits come from the algorithm B2 named and never from a constant: a strength
 * nobody told us is not a strength to display, and `AES256` is the only shape
 * documented today.
 */
const bitsOf = (algorithm?: string): string | undefined =>
    algorithm?.match(/(\d{3,4})/)?.[1];

export const bucketEncryptionBadge = (
    cached: ServerProfile['lastBucketEncryption'],
): BucketEncryptionBadge => {
    if (!cached) return { label: null, titleKey: null };
    if (cached.state === 'on') {
        const label = [cached.mode, bitsOf(cached.algorithm)].filter(Boolean).join(' ');
        return {
            label: label || null,
            titleKey: 'introHub.bucketEncryption.onTitle',
        };
    }
    if (cached.state === 'off') {
        return { label: null, titleKey: 'introHub.bucketEncryption.offTitle' };
    }
    return {
        label: null,
        titleKey:
            cached.reason === 'missing_capability'
                ? 'introHub.bucketEncryption.unknownKeyTitle'
                : 'introHub.bucketEncryption.unknownShapeTitle',
    };
};
