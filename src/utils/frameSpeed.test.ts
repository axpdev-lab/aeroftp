// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { nextFrameSpeed, type FrameSpeed } from './frameSpeed';

const MB = 1_000_000;

/** Feed frames `[ms, bytes]` in order and return the final state. */
function run(frames: Array<[number, number]>): FrameSpeed | null {
    let s: FrameSpeed | null = null;
    for (const [ms, bytes] of frames) s = nextFrameSpeed(s, ms, bytes);
    return s;
}

describe('nextFrameSpeed', () => {
    it('measures frames that all arrive closer than 50 ms apart', () => {
        // A fast extract: 2 % steps every 10 ms, 1 MB each, i.e. 100 MB/s.
        const frames: Array<[number, number]> = [];
        for (let i = 0; i <= 20; i++) frames.push([i * 10, i * MB]);
        const s = run(frames)!;
        expect(s.bps).toBeGreaterThan(90 * MB);
        expect(s.bps).toBeLessThan(110 * MB);
    });

    it('reports the byte rate between frames spaced normally', () => {
        const s = run([[0, 0], [150, 15 * MB], [300, 30 * MB]])!;
        expect(Math.round(s.bps / MB)).toBe(100);
    });

    it('follows a change of pace instead of averaging from the start', () => {
        const frames: Array<[number, number]> = [];
        let bytes = 0;
        for (let ms = 0; ms <= 3000; ms += 150) {
            frames.push([ms, bytes]);
            bytes += ms < 1500 ? 15 * MB : 1.5 * MB; // 100 MB/s, then 10 MB/s
        }
        const s = run(frames)!;
        expect(s.bps).toBeLessThan(15 * MB);
    });

    it('starts again from a counter that went backwards and keeps the last speed', () => {
        const before = run([[0, 0], [150, 15 * MB]])!;
        const after = nextFrameSpeed(before, 300, 1 * MB);
        expect(after).toEqual({ t: 300, bytes: 1 * MB, bps: before.bps });
        expect(Math.round(nextFrameSpeed(after, 450, 16 * MB).bps / MB)).toBe(100);
    });

    it('has no speed before the first measurement', () => {
        expect(run([[0, 0]])!.bps).toBe(0);
        expect(run([[0, 0], [20, 5 * MB]])!.bps).toBe(0);
    });
});
