// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { MIN_GRACE_MS, graceMs, liveEta, nextFrameClock, staleFactor, type FrameClock } from './liveSpeed';

/** Frames arriving at the given milliseconds, folded in order. */
function framesAt(times: number[]): FrameClock {
    let clock: FrameClock | null = null;
    for (const at of times) clock = nextFrameClock(clock, at);
    return clock!;
}

/** Frames every `step` ms from 0 to `until` included. */
function cadence(step: number, until: number): FrameClock {
    const times: number[] = [];
    for (let at = 0; at <= until; at += step) times.push(at);
    return framesAt(times);
}

describe('staleFactor', () => {
    it('keeps the reported speed while frames arrive at their cadence', () => {
        const clock = cadence(100, 5_000);
        for (const gap of [0, 100, 500, MIN_GRACE_MS]) {
            expect(staleFactor(clock, clock.at + gap)).toBe(1);
        }
    });

    it('lowers the speed once no frame has come for longer than the grace', () => {
        // The #1157 test: 10 Hz frames, then the server accepted nothing for 6.5 s.
        const clock = cadence(100, 5_000);
        const after = (gap: number) => staleFactor(clock, clock.at + gap);
        expect(after(1_500)).toBeLessThan(1);
        expect(after(6_500)).toBeLessThan(0.2);
        expect(after(30_000)).toBeLessThan(0.05);
        // Falls continuously, never jumps to a zero no frame proved.
        expect(after(2_000)).toBeLessThan(after(1_500));
        expect(after(60_000)).toBeGreaterThan(0);
    });

    it('waits longer on a transfer whose frames are naturally seconds apart', () => {
        // A slow link with large chunks: one frame every 2 s while bytes flow.
        const slow = cadence(2_000, 20_000);
        expect(graceMs(slow)).toBeGreaterThanOrEqual(5_000);
        expect(staleFactor(slow, slow.at + 2_500)).toBe(1);
        expect(staleFactor(slow, slow.at + 30_000)).toBeLessThan(0.5);
    });

    it('learns a sparse cadence from the first gap, so the speed does not fall between frames', () => {
        // A cross-profile copy reports once per file: a frame every 8 s while bytes flow.
        const second = framesAt([0, 8_000]);
        expect(staleFactor(second, second.at + 8_000)).toBe(1);
    });

    it('shrinks the wait back within a couple of seconds of frames after a stall', () => {
        const steady = cadence(100, 5_000);
        let clock = nextFrameClock(steady, steady.at + 60_000);
        // Right after a 60 s stall the wait is longer (capped, not 36 s)...
        expect(graceMs(clock)).toBeLessThanOrEqual(30_000);
        // ...and 2 s of 10 Hz frames bring it back near the minimum.
        for (let i = 1; i <= 20; i++) clock = nextFrameClock(clock, clock.at + 100);
        expect(graceMs(clock)).toBeLessThan(1_500);
    });

    it('starts from the minimum grace before it knows the cadence', () => {
        const first = nextFrameClock(null, 0);
        expect(graceMs(first)).toBe(MIN_GRACE_MS);
        expect(staleFactor(first, MIN_GRACE_MS)).toBe(1);
        expect(staleFactor(first, 2 * MIN_GRACE_MS)).toBe(0.5);
    });
});

describe('liveEta', () => {
    it('stretches the ETA by the same factor the speed fell', () => {
        expect(liveEta(60, 1)).toBe(60);
        expect(liveEta(60, 0.5)).toBe(120);
    });

    it('leaves an unknown ETA alone', () => {
        expect(liveEta(0, 0.5)).toBe(0);
    });
});
