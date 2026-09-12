// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { shouldPersistUsedScan } from './usedScanPersist';

describe('shouldPersistUsedScan', () => {
    it('refuses a listing the provider itself cut', () => {
        // The damage is not the number, it is who believes it: App.tsx used
        // to persist whenever !truncated, and the GUI fast path declared
        // truncated = false without reading the listing flag.
        expect(shouldPersistUsedScan({ truncated: true, cancelled: false })).toBe(false);
    });

    it('refuses a cancelled scan even if truncated were ever false', () => {
        expect(shouldPersistUsedScan({ truncated: false, cancelled: true })).toBe(false);
        expect(shouldPersistUsedScan({ truncated: true, cancelled: true })).toBe(false);
    });

    it('refuses a cancel that arrived on the last directory, with no further queue turn', () => {
        // Mirrors used_scan::a_cancel_raised_while_listing_the_last_directory_is_reported:
        // the BFS lists the only directory, the flag goes up during that list,
        // the queue is empty, and only the exit poll sets cancelled. Before
        // that poll, both flags were false and this figure would have been
        // written onto the profile.
        expect(shouldPersistUsedScan({ truncated: true, cancelled: true })).toBe(false);
    });

    it('persists a complete scan', () => {
        expect(shouldPersistUsedScan({ truncated: false, cancelled: false })).toBe(true);
    });

    it('treats a missing cancelled field as not-cancelled (additive default)', () => {
        expect(shouldPersistUsedScan({ truncated: false })).toBe(true);
        expect(shouldPersistUsedScan({ truncated: true })).toBe(false);
    });
});
