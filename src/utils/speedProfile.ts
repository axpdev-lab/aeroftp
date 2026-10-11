// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Transfer speed over the WHOLE transfer, for the speed graph: the chart of the
 * Windows copy dialog, where the horizontal axis is the progress (0-100 % of
 * the bytes) and the area shows the speed at each point reached so far.
 *
 * It replaces a rolling list of the last 60 speed samples, one per toast
 * update: that showed the last ~30 s only, so a drop earlier in a 20-minute
 * transfer was gone, and on a batch it sampled the same summary speed on every
 * lane update.
 *
 * The progress axis is cut into {@link SPEED_PROFILE_BUCKETS} equal slices of
 * bytes. Each slice keeps the bytes and the time the transfer spent crossing
 * it, so its speed is bytes over time: the real average over that slice, not
 * a mean of reported speeds. A stall lands in the slice where it happened
 * (the time grows, the bytes do not) and stays visible as a dip once the
 * transfer moves again; the whole-transfer average is total bytes over total
 * time.
 *
 * A transfer whose size is unknown has no progress axis: it keeps the recent
 * reported speeds instead ({@link RECENT_SAMPLES}), drawn as a time series.
 */

/** Slices of the progress axis. */
export const SPEED_PROFILE_BUCKETS = 200;
/** Reported speeds kept for a transfer of unknown size. */
export const RECENT_SAMPLES = 60;

export interface SpeedProfile {
    /** Share of the transfer crossed inside each slice (0 to 1 / BUCKETS). */
    readonly fractions: readonly number[];
    /** Milliseconds spent inside each slice. */
    readonly millis: readonly number[];
    /** Furthest progress recorded, 0 to 1. */
    readonly reached: number;
    /** Arrival of the frame that set `reached`, null before the first frame. */
    readonly at: number | null;
    /** Size of the transfer in bytes, 0 when unknown. */
    readonly totalBytes: number;
    /** Reported speeds, newest last, for a transfer of unknown size. */
    readonly recent: readonly number[];
}

export function emptySpeedProfile(): SpeedProfile {
    return {
        fractions: new Array(SPEED_PROFILE_BUCKETS).fill(0),
        millis: new Array(SPEED_PROFILE_BUCKETS).fill(0),
        reached: 0,
        at: null,
        totalBytes: 0,
        recent: [],
    };
}

export interface ProgressFrame {
    /** Bytes moved so far. */
    transferred: number;
    /** Size of the transfer in bytes; 0 or less when unknown. */
    total: number;
    /** Speed the frame reports, bytes per second. */
    speedBps: number;
    /** Arrival of the frame, milliseconds on a monotonic clock. */
    at: number;
}

const clamp01 = (value: number): number => Math.max(0, Math.min(1, value));

/**
 * Fold one progress frame into the profile, returning a new profile (the input
 * is never modified, so it can sit in React state).
 *
 * - The first frame is the reference: the bytes before it were not measured,
 *   so their slices stay empty (a resumed transfer starts mid-axis).
 * - A frame with no new bytes changes nothing: the time keeps running from
 *   the last frame and lands in the next segment, where the stall happened.
 * - Progress that goes back by more than one slice is another transfer or a
 *   restart from zero: the profile starts again from that frame.
 */
export function recordProgress(profile: SpeedProfile, frame: ProgressFrame): SpeedProfile {
    const speed = Number.isFinite(frame.speedBps) && frame.speedBps > 0 ? frame.speedBps : 0;
    if (!(frame.total > 0)) {
        const recent = [...profile.recent, speed].slice(-RECENT_SAMPLES);
        return { ...profile, recent };
    }

    const fraction = clamp01(frame.transferred / frame.total);
    const restart = profile.at !== null && fraction < profile.reached - 1 / SPEED_PROFILE_BUCKETS;
    if (profile.at === null || restart) {
        const fresh = emptySpeedProfile();
        return { ...fresh, reached: fraction, at: frame.at, totalBytes: frame.total };
    }

    const span = fraction - profile.reached;
    const elapsed = frame.at - profile.at;
    if (span <= 0 || elapsed <= 0) {
        return frame.total === profile.totalBytes ? profile : { ...profile, totalBytes: frame.total };
    }

    const fractions = profile.fractions.slice();
    const millis = profile.millis.slice();
    const width = 1 / SPEED_PROFILE_BUCKETS;
    const first = Math.min(SPEED_PROFILE_BUCKETS - 1, Math.floor(profile.reached / width));
    const last = Math.min(SPEED_PROFILE_BUCKETS - 1, Math.floor(fraction / width));
    for (let i = first; i <= last; i++) {
        const overlap = Math.min(fraction, (i + 1) * width) - Math.max(profile.reached, i * width);
        if (overlap <= 0) continue;
        fractions[i] += overlap;
        millis[i] += elapsed * (overlap / span);
    }
    return { fractions, millis, reached: fraction, at: frame.at, totalBytes: frame.total, recent: profile.recent };
}

/** Whether the profile has a progress axis to draw (a known size and some measured bytes). */
export function hasProgressAxis(profile: SpeedProfile): boolean {
    return profile.totalBytes > 0 && profile.millis.some((ms) => ms > 0);
}

/** Speed of each slice in bytes per second, null for a slice not measured. */
export function bucketSpeeds(profile: SpeedProfile): Array<number | null> {
    return profile.fractions.map((fraction, i) => {
        const ms = profile.millis[i];
        return ms > 0 ? (fraction * profile.totalBytes * 1000) / ms : null;
    });
}

/** Slices on each side that {@link smoothedSpeeds} folds in. */
export const SMOOTH_RADIUS = 2;

/**
 * Speed of each slice measured together with its neighbours ({@link
 * SMOOTH_RADIUS} on each side): their bytes over their time, so it is still
 * a real average, over 2.5 % of the transfer instead of 0.5 %. One slice is
 * often a single 100 ms frame, whose rate jitters with chunk timing and with
 * the pipeline filling at the start; a stall keeps its dip, because the stall
 * time dominates the sum. Null for a slice not measured.
 */
export function smoothedSpeeds(profile: SpeedProfile, radius = SMOOTH_RADIUS): Array<number | null> {
    return profile.millis.map((ms, i) => {
        if (!(ms > 0)) return null;
        let fraction = 0;
        let millis = 0;
        for (let j = Math.max(0, i - radius); j <= Math.min(SPEED_PROFILE_BUCKETS - 1, i + radius); j++) {
            if (profile.millis[j] > 0) {
                fraction += profile.fractions[j];
                millis += profile.millis[j];
            }
        }
        return (fraction * profile.totalBytes * 1000) / millis;
    });
}

export interface SpeedStats {
    /** Average over the measured part: bytes over time, 0 when nothing is measured. */
    avg: number;
    /** Fastest stretch of the transfer ({@link smoothedSpeeds}), or the
     *  highest reported speed without a progress axis. */
    peak: number;
}

export function speedStats(profile: SpeedProfile): SpeedStats {
    if (hasProgressAxis(profile)) {
        let fraction = 0;
        let ms = 0;
        let peak = 0;
        for (let i = 0; i < SPEED_PROFILE_BUCKETS; i++) {
            fraction += profile.fractions[i];
            ms += profile.millis[i];
        }
        for (const speed of smoothedSpeeds(profile)) {
            if (speed !== null && speed > peak) peak = speed;
        }
        return { avg: ms > 0 ? (fraction * profile.totalBytes * 1000) / ms : 0, peak };
    }
    const samples = profile.recent.filter((speed) => speed > 0);
    if (samples.length === 0) return { avg: 0, peak: 0 };
    return {
        avg: samples.reduce((sum, speed) => sum + speed, 0) / samples.length,
        peak: Math.max(...samples),
    };
}

/** Whether there is anything to draw yet. */
export function hasSpeedData(profile: SpeedProfile): boolean {
    return hasProgressAxis(profile) || profile.recent.filter((speed) => speed > 0).length > 1;
}
