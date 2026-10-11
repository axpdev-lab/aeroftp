// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The speed to show for a live transfer when progress frames stop arriving.
 *
 * Every speed on screen comes from the last progress frame, and a frame only
 * exists when bytes moved: the backend reports from the transfer callbacks,
 * which do not fire while the server accepts nothing. So a stalled transfer
 * sent no event at all and the toast kept the last speed for as long as the
 * stall lasted (6.5 s with the SSH window closed, #1157 test, 2026-10-10).
 * The absence of frames has to read as a falling speed.
 *
 * The rule: frames of one transfer come at a cadence (10 Hz from the GUI
 * governor, ~150 ms from the archive cadence, seconds apart on a slow link
 * with large chunks). While the gap since the last frame stays within a grace
 * of {@link GRACE_GAPS} typical gaps (at least {@link MIN_GRACE_MS}), the speed
 * is the reported one. Past the grace it falls as `grace / gap`: had bytes
 * kept moving at the reported speed, a frame would have come within the grace,
 * so over a longer gap at most about a grace's worth of them can have moved.
 * The curve is continuous at the grace and never claims a hard zero, which no
 * frame proved either.
 */

import { useEffect, useReducer, useState } from 'react';

/** Shortest wait before a missing frame lowers the speed. */
export const MIN_GRACE_MS = 1000;
/** Typical frame gaps to wait before a missing frame lowers the speed. */
export const GRACE_GAPS = 3;
/** Redraw interval while the speed is falling. */
export const TICK_MS = 500;
/** Weight of the newest gap in the typical gap. */
const GAP_WEIGHT = 0.2;

/** When the last frame arrived and the typical gap between frames. */
export interface FrameClock {
    /** Arrival of the last frame, milliseconds on a monotonic clock. */
    at: number;
    /** Typical gap between frames in milliseconds, 0 until a second frame. */
    gapMs: number;
}

/** Wait after the last frame before the speed starts to fall. */
export function graceMs(clock: FrameClock): number {
    return Math.max(MIN_GRACE_MS, GRACE_GAPS * clock.gapMs);
}

/** A gap enters the typical gap capped at this many graces. */
const GAP_CAP_GRACES = 10;

/**
 * Fold a frame arriving at `at` into the clock. The typical gap must be
 * learnt in a frame or two, because some transfers report seconds apart while
 * bytes flow (an S3 part at a time, a cross-profile copy at each file), and
 * until it is learnt their speed would fall between frames for nothing. A very
 * long gap (a stall) enters it capped at {@link GAP_CAP_GRACES} graces, and
 * the frames that follow a stall shrink it back quickly.
 */
export function nextFrameClock(prev: FrameClock | null, at: number): FrameClock {
    if (!prev) return { at, gapMs: 0 };
    const gap = Math.max(0, at - prev.at);
    const sample = Math.min(gap, GAP_CAP_GRACES * graceMs(prev));
    const gapMs = prev.gapMs === 0 ? sample : prev.gapMs * (1 - GAP_WEIGHT) + sample * GAP_WEIGHT;
    return { at, gapMs };
}

/** Share of the reported speed to show at `now`: 1 within the grace, then `grace / gap`. */
export function staleFactor(clock: FrameClock, now: number): number {
    const gap = now - clock.at;
    const grace = graceMs(clock);
    return gap <= grace ? 1 : grace / gap;
}

/** The ETA that goes with a speed scaled by `factor`: the same bytes at the lower speed. */
export function liveEta(etaSeconds: number, factor: number): number {
    if (!(etaSeconds > 0) || !(factor > 0)) return etaSeconds;
    return factor >= 1 ? etaSeconds : Math.round(etaSeconds / factor);
}

export interface LiveSpeed {
    /** Speed to show, bytes per second. */
    bps: number;
    /** Share of the reported speed it is: 1 while frames arrive. */
    factor: number;
}

const now = (): number => (typeof performance !== 'undefined' ? performance.now() : Date.now());

/**
 * {@link staleFactor} applied to a component's speed. `frame` is anything that
 * changes with each progress frame (the byte count, the percentage, the event
 * object); `active` is false once the transfer is over, when there is nothing
 * left to fall. Redraws every {@link TICK_MS} only while the speed is falling.
 */
export function useLiveSpeed(speedBps: number | undefined, frame: unknown, active = true): LiveSpeed {
    const [tracked, setTracked] = useState(() => ({ frame, clock: nextFrameClock(null, now()) }));
    let current = tracked;
    if (!Object.is(tracked.frame, frame)) {
        // A new frame: record it during render (the derived-state pattern), so
        // this render already shows the fresh speed instead of a stale factor.
        current = { frame, clock: nextFrameClock(tracked.clock, now()) };
        setTracked(current);
    }
    const reported = speedBps !== undefined && Number.isFinite(speedBps) && speedBps > 0 ? speedBps : 0;
    const falling = active && reported > 0;
    const [, redraw] = useReducer((n: number) => n + 1, 0);

    useEffect(() => {
        if (!falling) return;
        let timer: ReturnType<typeof setTimeout>;
        const schedule = (delay: number) => {
            timer = setTimeout(() => {
                redraw();
                schedule(TICK_MS);
            }, delay);
        };
        schedule(Math.max(0, current.clock.at + graceMs(current.clock) - now()));
        return () => clearTimeout(timer);
    }, [current.clock, falling]);

    const factor = falling ? staleFactor(current.clock, now()) : 1;
    return { bps: reported * factor, factor };
}
