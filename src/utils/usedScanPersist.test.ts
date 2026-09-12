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

    it('persists a complete scan', () => {
        expect(shouldPersistUsedScan({ truncated: false, cancelled: false })).toBe(true);
    });

    it('treats a missing cancelled field as not-cancelled (additive default)', () => {
        expect(shouldPersistUsedScan({ truncated: false })).toBe(true);
        expect(shouldPersistUsedScan({ truncated: true })).toBe(false);
    });
});
