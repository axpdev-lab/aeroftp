// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { isTransferComplete } from './transferCompletion';

describe('isTransferComplete', () => {
    it('keeps a transfer that only rounds to 100 incomplete', () => {
        // The regression: the card and the chip asked Math.round(99.6), got 100,
        // unlocked, and offered a close button wired to cancel.
        expect(Math.round(99.6)).toBe(100);
        expect(isTransferComplete(99.6)).toBe(false);
        expect(isTransferComplete(99.5)).toBe(false);
        expect(isTransferComplete(99.999)).toBe(false);
    });

    it('is complete at 100 with no lanes', () => {
        expect(isTransferComplete(100)).toBe(true);
        expect(isTransferComplete(100, [])).toBe(true);
        expect(isTransferComplete(120)).toBe(true);
    });

    it('stays incomplete while any lane is still active', () => {
        expect(isTransferComplete(100, [{ state: 'completed' }, { state: 'active' }])).toBe(false);
        expect(isTransferComplete(100, [{ state: 'completed' }, { state: 'error' }])).toBe(true);
        expect(isTransferComplete(100, [{}])).toBe(true);
    });

    it('treats a non-finite percentage as incomplete', () => {
        expect(isTransferComplete(Number.NaN)).toBe(false);
        expect(isTransferComplete(Number.POSITIVE_INFINITY)).toBe(false);
    });
});
