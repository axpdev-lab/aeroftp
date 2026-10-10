// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useRef, useState } from 'react';
import { useTauriListener } from './useTauriListener';
import { nextFrameSpeed, type FrameSpeed } from '../utils/frameSpeed';

/**
 * One progress frame, enriched with frontend-derived speed/ETA.
 */
export interface ArchiveProgressState {
    /** Phase key, mapped to i18n `progress.*` (scanning/compressing/extracting/decrypting). */
    phase: string;
    percentage: number;
    transferred: number;
    total: number;
    /** True for opaque ops (RAR extract): show an indeterminate bar, no %/ETA. */
    indeterminate: boolean;
    speedBps?: number;
    etaSeconds?: number;
}

/** Raw payload of the backend `archive_progress` event. */
interface RawFrame {
    phase: string;
    percentage: number;
    transferred: number;
    total: number;
    indeterminate: boolean;
}

/**
 * Subscribe to the backend `archive_progress` event and surface the latest frame,
 * with speed and ETA derived locally from successive frames.
 *
 * The backend only emits for operations at or above the 10 MB threshold, so this
 * returns `null` for small/instant ops (the caller therefore renders no bar). The
 * listener is always attached while the component is mounted (so the first frame of a
 * just-started operation is never missed to an attach race); `active` gates what is
 * returned and resets the derived state when the operation ends.
 */
export function useArchiveProgress(active: boolean): ArchiveProgressState | null {
    const [state, setState] = useState<ArchiveProgressState | null>(null);
    const speedRef = useRef<FrameSpeed | null>(null);
    // The listener is always attached (so the first frame of a just-started op is
    // never lost to an attach race), but it must only act while THIS hook's operation
    // is running. `archive_progress` is a single global event, so without this gate a
    // second mounted modal (or a previous op's late frames) could leak another
    // operation's bytes into this bar. Read through a ref so the always-attached
    // handler sees the live value without re-subscribing.
    const activeRef = useRef(active);

    useTauriListener<RawFrame>('archive_progress', (e) => {
        if (!activeRef.current) return; // ignore frames while idle / from other ops
        const f = e.payload;
        // Current speed from the byte delta between frames, EMA-smoothed.
        speedRef.current = nextFrameSpeed(speedRef.current, performance.now(), f.transferred);
        const speedBps = speedRef.current.bps > 0 ? speedRef.current.bps : undefined;
        const remaining = Math.max(0, f.total - f.transferred);
        const etaSeconds =
            !f.indeterminate && speedBps && speedBps > 0 && remaining > 0
                ? remaining / speedBps
                : undefined;
        setState({
            phase: f.phase,
            percentage: f.percentage,
            transferred: f.transferred,
            total: f.total,
            indeterminate: f.indeterminate,
            speedBps,
            etaSeconds,
        });
    });

    // Start every operation from a clean slate: reset derived state on BOTH edges of
    // `active` (rising too), so a freshly-started op can never expose a stale frame
    // accumulated earlier and a finished op never lingers.
    useEffect(() => {
        activeRef.current = active;
        setState(null);
        speedRef.current = null;
    }, [active]);

    return active ? state : null;
}
