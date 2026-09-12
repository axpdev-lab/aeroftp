// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { usedScanBoundKey } from './usedScanBound';

describe('usedScanBoundKey', () => {
    it('names the cancel when both flags are up, which is the state the backend produces', () => {
        // Not a corner case: both scan paths in used_scan.rs do
        // `if cancelled { truncated = true; }`, so this is what a cancelled
        // scan actually looks like by the time it reaches the interface. Read
        // for `truncated` alone, it used to report the user's own cancel as
        // the provider cutting its listing.
        expect(usedScanBoundKey({ truncated: true, cancelled: true })).toBe('transfer.cancelled');
    });

    it('names the truncation when the provider cut the listing', () => {
        expect(usedScanBoundKey({ truncated: true, cancelled: false }))
            .toBe('statusBar.usedScanTruncated');
    });

    it('keeps the order of size_bound_marker, so a swap of the two branches is red', () => {
        // A cancel without truncation is not reachable today. It is asserted
        // anyway: it is the half of the order that the reachable case cannot
        // pin, since there both branches would answer.
        expect(usedScanBoundKey({ truncated: false, cancelled: true })).toBe('transfer.cancelled');
    });

    it('says nothing when the figure is a complete answer', () => {
        expect(usedScanBoundKey({ truncated: false, cancelled: false })).toBeNull();
    });

    it('treats a missing cancelled field as not-cancelled (additive default)', () => {
        expect(usedScanBoundKey({ truncated: false })).toBeNull();
        expect(usedScanBoundKey({ truncated: true })).toBe('statusBar.usedScanTruncated');
    });
});
