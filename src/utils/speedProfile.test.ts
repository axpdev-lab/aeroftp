// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
    RECENT_SAMPLES,
    SPEED_PROFILE_BUCKETS,
    bucketSpeeds,
    emptySpeedProfile,
    hasProgressAxis,
    hasSpeedData,
    recordProgress,
    smoothedSpeeds,
    speedStats,
    type SpeedProfile,
} from './speedProfile';

const MB = 1_000_000;
const TOTAL = 1_000 * MB;

/** Feed a constant rate in 100 ms frames from `fromMs`, for `seconds`, from `bytes`. */
function run(profile: SpeedProfile, fromMs: number, seconds: number, bytes: number, mbPerSecond: number) {
    let p = profile;
    let b = bytes;
    let at = fromMs;
    const step = mbPerSecond * MB / 10;
    for (let i = 0; i < seconds * 10; i++) {
        at += 100;
        b += step;
        p = recordProgress(p, { transferred: b, total: TOTAL, speedBps: mbPerSecond * MB, at });
    }
    return { profile: p, at, bytes: b };
}

const near = (value: number | null, expected: number) => {
    expect(value).not.toBeNull();
    expect(Math.abs((value as number) - expected) / expected).toBeLessThan(0.02);
};

describe('recordProgress', () => {
    it('keeps the speed of the whole transfer, not the last 30 seconds', () => {
        // 100 MB/s for the first 30 %, then 25 MB/s: the fast start must still be there.
        let p = recordProgress(emptySpeedProfile(), { transferred: 0, total: TOTAL, speedBps: 0, at: 0 });
        const fast = run(p, 0, 3, 0, 100);
        const slow = run(fast.profile, fast.at, 20, fast.bytes, 25);
        p = slow.profile;
        const speeds = bucketSpeeds(p);
        near(speeds[10], 100 * MB);
        near(speeds[50], 100 * MB);
        near(speeds[100], 25 * MB);
        expect(p.reached).toBeCloseTo(0.8, 5);
        // Not reached yet: nothing drawn past 80 %.
        expect(speeds[SPEED_PROFILE_BUCKETS - 1]).toBeNull();
    });

    it('puts a stall in the slice where it happened, as a dip', () => {
        let p = recordProgress(emptySpeedProfile(), { transferred: 0, total: TOTAL, speedBps: 0, at: 0 });
        const before = run(p, 0, 2, 0, 100);
        // No frame for 5 s (the server accepts nothing), then the transfer moves on.
        const after = run(before.profile, before.at + 5_000, 2, before.bytes, 100);
        p = after.profile;
        const speeds = bucketSpeeds(p);
        const stallSlice = Math.floor(0.2 * SPEED_PROFILE_BUCKETS);
        expect(speeds[stallSlice]!).toBeLessThan(10 * MB);
        near(speeds[stallSlice - 5], 100 * MB);
        near(speeds[stallSlice + 5], 100 * MB);
        // The average counts the stall: 400 MB in 9 s.
        near(speedStats(p).avg, (400 * MB) / 9);
        near(speedStats(p).peak, 100 * MB);
    });

    it('smooths frame jitter for the graph but keeps a stall as a dip', () => {
        // Frames alternate 5 MB and 15 MB per 100 ms: 100 MB/s on average,
        // 50 and 150 frame by frame.
        let p = recordProgress(emptySpeedProfile(), { transferred: 0, total: TOTAL, speedBps: 0, at: 0 });
        let bytes = 0;
        let at = 0;
        for (let i = 0; i < 40; i++) {
            at += 100;
            bytes += (i % 2 === 0 ? 5 : 15) * MB;
            p = recordProgress(p, { transferred: bytes, total: TOTAL, speedBps: 100 * MB, at });
        }
        // Then 5 s with no frame, and the transfer moves on at 100 MB/s.
        p = run(p, at + 5_000, 2, bytes, 100).profile;
        const raw = bucketSpeeds(p).slice(10, 70).filter((v): v is number => v !== null);
        const smooth = smoothedSpeeds(p).slice(10, 70).filter((v): v is number => v !== null);
        const spread = (values: number[]) => Math.max(...values) - Math.min(...values);
        expect(spread(raw)).toBeGreaterThanOrEqual(90 * MB);
        expect(spread(smooth)).toBeLessThan(40 * MB);
        const stallSlice = Math.floor(0.4 * SPEED_PROFILE_BUCKETS);
        expect(smoothedSpeeds(p)[stallSlice]!).toBeLessThan(20 * MB);
    });

    it('leaves the bytes before the first frame unmeasured (a resumed transfer)', () => {
        let p = recordProgress(emptySpeedProfile(), { transferred: 500 * MB, total: TOTAL, speedBps: 0, at: 0 });
        p = run(p, 0, 1, 500 * MB, 100).profile;
        const speeds = bucketSpeeds(p);
        expect(speeds[0]).toBeNull();
        expect(speeds[SPEED_PROFILE_BUCKETS / 2 - 1]).toBeNull();
        near(speeds[SPEED_PROFILE_BUCKETS / 2 + 2], 100 * MB);
    });

    it('starts again when the progress goes back to the start', () => {
        let p = recordProgress(emptySpeedProfile(), { transferred: 0, total: TOTAL, speedBps: 0, at: 0 });
        p = run(p, 0, 5, 0, 100).profile;
        p = recordProgress(p, { transferred: 0, total: TOTAL, speedBps: 0, at: 10_000 });
        expect(p.reached).toBe(0);
        expect(hasProgressAxis(p)).toBe(false);
    });

    it('spreads one large step over every slice it crosses', () => {
        let p = recordProgress(emptySpeedProfile(), { transferred: 0, total: TOTAL, speedBps: 0, at: 0 });
        p = recordProgress(p, { transferred: TOTAL, total: TOTAL, speedBps: 0, at: 10_000 });
        const speeds = bucketSpeeds(p);
        expect(speeds.every((speed) => speed !== null)).toBe(true);
        near(speeds[0], 100 * MB);
        near(speeds[SPEED_PROFILE_BUCKETS - 1], 100 * MB);
    });

    it('never modifies the profile it was given', () => {
        const start = recordProgress(emptySpeedProfile(), { transferred: 0, total: TOTAL, speedBps: 0, at: 0 });
        const snapshot = JSON.stringify(start);
        recordProgress(start, { transferred: 10 * MB, total: TOTAL, speedBps: MB, at: 100 });
        expect(JSON.stringify(start)).toBe(snapshot);
    });

    it('keeps recent reported speeds when the size is unknown', () => {
        let p = emptySpeedProfile();
        for (let i = 1; i <= RECENT_SAMPLES + 10; i++) {
            p = recordProgress(p, { transferred: i * MB, total: 0, speedBps: i * MB, at: i * 100 });
        }
        expect(hasProgressAxis(p)).toBe(false);
        expect(hasSpeedData(p)).toBe(true);
        expect(p.recent).toHaveLength(RECENT_SAMPLES);
        expect(p.recent[p.recent.length - 1]).toBe((RECENT_SAMPLES + 10) * MB);
    });
});
