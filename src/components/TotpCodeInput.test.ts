// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import {
    normalizeCode,
    splitCode,
    joinBoxes,
    writeDigits,
    clearBoxAt,
} from './TotpCodeInput';

// #369 (Ehud): the per-box TOTP input holds state PER BOX, not derived from the
// joined string, so clearing a middle box corrects it in place instead of
// shifting every later digit one box left. These cover the pure helpers the
// component is built from; the box-shift bug is the first (regression) case.

describe('TotpCodeInput helpers', () => {
    it('REGRESSION: clearing a middle box keeps later digits in place', () => {
        // Bug was: clearing box 3 of "123456" shifted "456" left to "45 6".
        const boxes = splitCode('123456', 6);
        expect(boxes).toEqual(['1', '2', '3', '4', '5', '6']);

        const cleared = clearBoxAt(boxes, 2);
        // 4, 5, 6 stay in boxes 3, 4, 5 (indices 3-5) — no left shift.
        expect(cleared).toEqual(['1', '2', '', '4', '5', '6']);
        // The emitted wire value compacts the hole away (an incomplete code).
        expect(joinBoxes(cleared)).toBe('12456');

        // Retyping the middle box restores the full code in place.
        const { boxes: restored, landed } = writeDigits(cleared, 2, '3');
        expect(restored).toEqual(['1', '2', '3', '4', '5', '6']);
        expect(joinBoxes(restored)).toBe('123456');
        // Focus advances to the slot past the written digit.
        expect(landed).toBe(3);
    });

    it('backspace-to-previous clears the prior box, positions preserved', () => {
        // Backspace on an empty box clears idx-1 (component handleKeyDown).
        const boxes = splitCode('123456', 6);
        const cleared = clearBoxAt(boxes, 1);
        expect(cleared).toEqual(['1', '', '3', '4', '5', '6']);
        // 3, 4, 5, 6 keep their box positions (indices 2-5).
        expect(joinBoxes(cleared)).toBe('13456');
    });

    it('writeDigits from box 0 overwrites every box, landed clamps to last', () => {
        const empty = splitCode('', 6);
        const { boxes, landed } = writeDigits(empty, 0, '987654');
        expect(boxes).toEqual(['9', '8', '7', '6', '5', '4']);
        expect(joinBoxes(boxes)).toBe('987654');
        // p advances to 6, landed clamps to length-1 so focus stays in range.
        expect(landed).toBe(5);
    });

    it('paste longer than the box count is capped', () => {
        const empty = splitCode('', 6);
        const { boxes, landed } = writeDigits(empty, 0, '123456789');
        expect(boxes).toEqual(['1', '2', '3', '4', '5', '6']);
        expect(joinBoxes(boxes)).toBe('123456');
        expect(landed).toBe(5);

        // A paste that starts mid-way only fills the remaining boxes.
        const midEmpty = splitCode('', 6);
        const { boxes: mid } = writeDigits(midEmpty, 4, '999');
        expect(mid).toEqual(['', '', '', '', '9', '9']);
    });

    it('normalizeCode strips non-digits and caps at length', () => {
        expect(normalizeCode('1a2b3c4d5e6f7', 6)).toBe('123456');
        expect(normalizeCode('12-34 56', 6)).toBe('123456');
        expect(normalizeCode('123456789', 6)).toBe('123456');
        expect(normalizeCode('', 6)).toBe('');
        // A non-default length (some pages use a different digit count).
        expect(normalizeCode('12345678', 8)).toBe('12345678');
    });

    it('splitCode/joinBoxes round-trip; holes compact and re-expand left', () => {
        // Full code round-trips exactly.
        const full = ['1', '2', '3', '4', '5', '6'];
        expect(splitCode(joinBoxes(full), 6)).toEqual(full);

        // A left-aligned partial round-trips exactly.
        const partial = ['1', '2', '3', '', '', ''];
        expect(splitCode(joinBoxes(partial), 6)).toEqual(partial);

        // A holed array compacts on join, then splitCode left-aligns it: the
        // holes move to the end (the documented emitted-value behaviour).
        const holed = ['1', '', '3', '', '5', ''];
        expect(joinBoxes(holed)).toBe('135');
        expect(splitCode('135', 6)).toEqual(['1', '3', '5', '', '', '']);
    });

    it('clearBoxAt is a no-op for out-of-range indices', () => {
        const boxes = splitCode('123456', 6);
        expect(clearBoxAt(boxes, -1)).toEqual(boxes);
        expect(clearBoxAt(boxes, 6)).toEqual(boxes);
        // It never mutates the input array.
        expect(boxes).toEqual(['1', '2', '3', '4', '5', '6']);
    });
});
