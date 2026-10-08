// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import {
    SPLIT_DEFAULT_PERCENT,
    applySplitKey,
    clampSplitPercent,
    computeSplitBounds,
    dragPercentFromPointer,
    dualLocalFlexToPercent,
    parseStoredSplitPercent,
    percentToDualLocalFlex,
} from './usePanelSplit';

describe('parseStoredSplitPercent', () => {
    it('returns null for missing or invalid values', () => {
        expect(parseStoredSplitPercent(null)).toBeNull();
        expect(parseStoredSplitPercent('')).toBeNull();
        expect(parseStoredSplitPercent('abc')).toBeNull();
        expect(parseStoredSplitPercent('NaN')).toBeNull();
        expect(parseStoredSplitPercent('Infinity')).toBeNull();
    });

    it('returns null for out-of-range values', () => {
        expect(parseStoredSplitPercent('5')).toBeNull();
        expect(parseStoredSplitPercent('95')).toBeNull();
        expect(parseStoredSplitPercent('-20')).toBeNull();
    });

    it('accepts values inside the 10..90 range', () => {
        expect(parseStoredSplitPercent('10')).toBe(10);
        expect(parseStoredSplitPercent('50')).toBe(50);
        expect(parseStoredSplitPercent('90')).toBe(90);
        expect(parseStoredSplitPercent('33.5')).toBe(33.5);
    });
});

describe('dual-local legacy storage adapters', () => {
    it('maps the legacy 0.2..1.8 flex range onto 10..90 percent', () => {
        expect(dualLocalFlexToPercent('0.2')).toBe(10);
        expect(dualLocalFlexToPercent('1')).toBe(50);
        expect(dualLocalFlexToPercent('1.8')).toBe(90);
        expect(dualLocalFlexToPercent('1.35')).toBeCloseTo(67.5);
    });

    it('rejects missing, invalid or out-of-range legacy values', () => {
        expect(dualLocalFlexToPercent(null)).toBeNull();
        expect(dualLocalFlexToPercent('abc')).toBeNull();
        expect(dualLocalFlexToPercent('0.1')).toBeNull();
        expect(dualLocalFlexToPercent('2')).toBeNull();
    });

    it('writes back the same flex format the old code persisted', () => {
        expect(percentToDualLocalFlex(50)).toBe('1');
        expect(percentToDualLocalFlex(10)).toBe('0.2');
        expect(percentToDualLocalFlex(90)).toBe('1.8');
    });

    it('round-trips a legacy saved value unchanged', () => {
        const legacy = '1.35';
        const percent = dualLocalFlexToPercent(legacy);
        expect(percent).not.toBeNull();
        expect(Number.parseFloat(percentToDualLocalFlex(percent!))).toBeCloseTo(1.35);
    });
});

describe('computeSplitBounds', () => {
    it('keeps the global 10..90 bounds on wide containers', () => {
        expect(computeSplitBounds(2004, 4, 160)).toEqual({ minPercent: 10, maxPercent: 90 });
    });

    it('excludes the separator width from the usable width', () => {
        // usable = 1004 - 4 = 1000 -> 160 px is exactly 16 %
        expect(computeSplitBounds(1004, 4, 160)).toEqual({ minPercent: 16, maxPercent: 84 });
    });

    it('raises the minimum when panels would fall below the pixel minimum', () => {
        // usable = 640 -> 160 px is 25 %
        expect(computeSplitBounds(644, 4, 160)).toEqual({ minPercent: 25, maxPercent: 75 });
    });

    it('falls back to a defined 50/50 when both minima do not fit', () => {
        // usable = 300 < 2 * 160: equal fallback, no negative widths
        expect(computeSplitBounds(304, 4, 160)).toEqual({ minPercent: 50, maxPercent: 50 });
    });

    it('handles exactly-fitting and degenerate containers', () => {
        expect(computeSplitBounds(324, 4, 160)).toEqual({ minPercent: 50, maxPercent: 50 });
        expect(computeSplitBounds(0, 4, 160)).toEqual({ minPercent: 50, maxPercent: 50 });
        expect(computeSplitBounds(-100, 4, 160)).toEqual({ minPercent: 50, maxPercent: 50 });
        expect(computeSplitBounds(Number.NaN, 4, 160)).toEqual({ minPercent: 50, maxPercent: 50 });
    });
});

describe('clampSplitPercent', () => {
    const bounds = { minPercent: 25, maxPercent: 75 };

    it('clamps to the live bounds', () => {
        expect(clampSplitPercent(10, bounds)).toBe(25);
        expect(clampSplitPercent(90, bounds)).toBe(75);
        expect(clampSplitPercent(50, bounds)).toBe(50);
    });

    it('preserves the saved preference across a narrow-then-wide resize', () => {
        // This is the restore semantic: the desired ratio is only clamped for
        // rendering, so widening the container restores the preference.
        const desired = 90;
        const narrow = computeSplitBounds(644, 4, 160); // {25, 75}
        const wide = computeSplitBounds(2004, 4, 160); // {10, 90}
        expect(clampSplitPercent(desired, narrow)).toBe(75);
        expect(clampSplitPercent(desired, wide)).toBe(desired);
    });
});

describe('applySplitKey', () => {
    const bounds = { minPercent: 10, maxPercent: 90 };

    it('steps left and right by 5 percent from the rendered value', () => {
        expect(applySplitKey(50, 'ArrowLeft', bounds)).toBe(45);
        expect(applySplitKey(50, 'ArrowRight', bounds)).toBe(55);
    });

    it('clamps arrow steps at the bounds', () => {
        expect(applySplitKey(12, 'ArrowLeft', bounds)).toBe(10);
        expect(applySplitKey(88, 'ArrowRight', bounds)).toBe(90);
    });

    it('moves visibly even when the rendered value is clamped below the saved one', () => {
        // Saved 90 but rendered at 75 in a narrow container: ArrowLeft must
        // move from the rendered position, not appear dead.
        expect(applySplitKey(75, 'ArrowLeft', { minPercent: 25, maxPercent: 75 })).toBe(70);
    });

    it('jumps to the live extremes with Home and End', () => {
        expect(applySplitKey(50, 'Home', { minPercent: 25, maxPercent: 75 })).toBe(25);
        expect(applySplitKey(50, 'End', { minPercent: 25, maxPercent: 75 })).toBe(75);
    });

    it('resets to 50/50 on Enter and Space', () => {
        expect(applySplitKey(30, 'Enter', bounds)).toBe(SPLIT_DEFAULT_PERCENT);
        expect(applySplitKey(70, ' ', bounds)).toBe(SPLIT_DEFAULT_PERCENT);
    });

    it('ignores unrelated keys', () => {
        expect(applySplitKey(50, 'a', bounds)).toBeNull();
        expect(applySplitKey(50, 'Tab', bounds)).toBeNull();
        expect(applySplitKey(50, 'ArrowUp', bounds)).toBeNull();
    });

    it('stays at the equal fallback in a container too narrow for both minima', () => {
        const fallback = { minPercent: 50, maxPercent: 50 };
        expect(applySplitKey(50, 'ArrowLeft', fallback)).toBe(50);
        expect(applySplitKey(50, 'ArrowRight', fallback)).toBe(50);
        expect(applySplitKey(50, 'Home', fallback)).toBe(50);
    });
});

describe('dragPercentFromPointer', () => {
    const bounds = { minPercent: 10, maxPercent: 90 };

    it('maps the pointer position to a left-panel percent', () => {
        expect(dragPercentFromPointer(500, 0, 1000, bounds)).toBe(50);
        expect(dragPercentFromPointer(250, 100, 1000, bounds)).toBe(15);
    });

    it('clamps drags past the edges to the bounds', () => {
        expect(dragPercentFromPointer(-50, 0, 1000, bounds)).toBe(10);
        expect(dragPercentFromPointer(2000, 0, 1000, bounds)).toBe(90);
    });

    it('follows tighter bounds from a narrow container', () => {
        const narrow = { minPercent: 25, maxPercent: 75 };
        expect(dragPercentFromPointer(100, 0, 1000, narrow)).toBe(25);
        expect(dragPercentFromPointer(900, 0, 1000, narrow)).toBe(75);
    });

    it('returns null when the container has no measurable width', () => {
        expect(dragPercentFromPointer(500, 0, 0, bounds)).toBeNull();
        expect(dragPercentFromPointer(500, 0, Number.NaN, bounds)).toBeNull();
    });
});
