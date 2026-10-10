// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Speed derived on the frontend from successive progress frames that carry only
 * a byte count (the backend `archive_progress` event): the byte rate between two
 * frames, smoothed with an exponential moving average.
 *
 * The backend throttles those frames to one per ~150 ms OR one per 2 % step, so
 * on a fast operation several frames arrive a few milliseconds apart. A frame
 * closer than {@link MIN_FRAME_GAP_S} to the anchor is too close to measure, and
 * it must NOT become the new anchor: moving the anchor on every frame while
 * skipping the measurement left the speed at 0 for a whole operation whose
 * frames all came less than 50 ms apart.
 */

/** Below this gap between the anchor and a frame there is no rate to measure. */
export const MIN_FRAME_GAP_S = 0.05;

/** Weight of the newest measurement in the moving average. */
const NEW_SAMPLE_WEIGHT = 0.4;

export interface FrameSpeed {
    /** Time of the anchor frame, `performance.now()` milliseconds. */
    t: number;
    /** Bytes reported by the anchor frame. */
    bytes: number;
    /** Smoothed bytes per second, 0 until the first measurement. */
    bps: number;
}

/** Fold one frame (`now` in ms, `bytes` so far) into the running speed. */
export function nextFrameSpeed(prev: FrameSpeed | null, now: number, bytes: number): FrameSpeed {
    if (!prev) return { t: now, bytes, bps: 0 };
    // A counter that went backwards is another operation or a restart: start
    // measuring again from this frame and keep the last known speed meanwhile.
    if (bytes < prev.bytes) return { t: now, bytes, bps: prev.bps };
    const dt = (now - prev.t) / 1000;
    if (dt <= MIN_FRAME_GAP_S) return prev;
    const inst = (bytes - prev.bytes) / dt;
    const bps = prev.bps === 0 ? inst : prev.bps * (1 - NEW_SAMPLE_WEIGHT) + inst * NEW_SAMPLE_WEIGHT;
    return { t: now, bytes, bps };
}
