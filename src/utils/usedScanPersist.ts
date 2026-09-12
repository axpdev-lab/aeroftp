// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * A used-storage scan may be written onto the profile only when it is a
 * complete answer. A truncated listing (the provider's own cap, typically
 * S3 ListObjectsV2) or a cancelled scan is a lower bound; persisting it
 * would pin that figure as the profile's authoritative `used` for the rest
 * of its life.
 *
 * `cancelled` is additive: an older backend that omits the field is treated
 * as not-cancelled, and the truncated flag remains the other gate.
 */
export type UsedScanPersistInput = {
    truncated: boolean;
    cancelled?: boolean;
};

export function shouldPersistUsedScan(res: UsedScanPersistInput): boolean {
    return !res.truncated && !res.cancelled;
}
