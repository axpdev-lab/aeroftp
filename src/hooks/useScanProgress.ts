// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// The backend has emitted `sync_scan_progress` from every compare walker since
// the recursive scan landed, and nothing on the frontend ever listened: the
// Compare tab showed a bare spinner while the counters it needed were already
// on the wire (discussion #347).
//
// The two sides report independently and out of order — local and remote scan
// concurrently — so the reducer keeps the latest tick per side and totals them,
// rather than trusting whichever event arrived last.

import { useEffect, useRef, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

/** One `sync_scan_progress` tick. `dirs_found`/`bytes_found` are absent from
 *  the DAG-observer emitter, which only knows how many entries it has seen. */
export interface ScanProgressEvent {
    phase: 'local' | 'remote' | 'comparing';
    files_found?: number;
    dirs_found?: number;
    bytes_found?: number;
}

export interface ScanProgressSide {
    files: number;
    dirs: number;
    bytes: number;
}

export interface ScanProgressState {
    local: ScanProgressSide;
    remote: ScanProgressSide;
    /** True once a walker has reported the comparison phase. */
    comparing: boolean;
}

const EMPTY_SIDE: ScanProgressSide = { files: 0, dirs: 0, bytes: 0 };
export const EMPTY_SCAN_PROGRESS: ScanProgressState = {
    local: EMPTY_SIDE,
    remote: EMPTY_SIDE,
    comparing: false,
};

/**
 * Fold one tick into the running state.
 *
 * `files_found` on the remote side is emitted as "local + remote so far", which
 * would double-count if it were simply added to the local side's own number, so
 * each side is *replaced* by its latest tick and the totals are derived. A tick
 * that omits dirs/bytes keeps the last known value for that side instead of
 * resetting it to zero.
 */
export function reduceScanProgress(
    state: ScanProgressState,
    event: ScanProgressEvent,
): ScanProgressState {
    if (event.phase === 'comparing') return { ...state, comparing: true };
    const side = event.phase === 'local' ? state.local : state.remote;
    const next: ScanProgressSide = {
        files: event.files_found ?? side.files,
        dirs: event.dirs_found ?? side.dirs,
        bytes: event.bytes_found ?? side.bytes,
    };
    return event.phase === 'local'
        ? { ...state, local: next }
        : { ...state, remote: next };
}

/** Totals across both sides, which is what the dialog shows. */
export function scanProgressTotals(state: ScanProgressState): ScanProgressSide {
    // The remote side reports a cumulative file count that already includes the
    // local map, so the larger of the two IS the total rather than their sum.
    return {
        files: Math.max(state.local.files, state.remote.files),
        dirs: state.local.dirs + state.remote.dirs,
        bytes: state.local.bytes + state.remote.bytes,
    };
}

/**
 * Subscribe while `active`, and keep a clock running alongside. Returns the
 * folded state, the totals, and how long the scan has been going.
 */
export function useScanProgress(active: boolean) {
    const [state, setState] = useState<ScanProgressState>(EMPTY_SCAN_PROGRESS);
    const [elapsedMs, setElapsedMs] = useState(0);
    const startedAt = useRef<number>(0);

    useEffect(() => {
        if (!active) return;
        setState(EMPTY_SCAN_PROGRESS);
        setElapsedMs(0);
        startedAt.current = Date.now();

        let unlisten: UnlistenFn | null = null;
        let cancelled = false;
        listen<ScanProgressEvent>('sync_scan_progress', (e) => {
            setState((prev) => reduceScanProgress(prev, e.payload));
        }).then((fn) => {
            if (cancelled) fn();
            else unlisten = fn;
        });

        const clock = setInterval(() => setElapsedMs(Date.now() - startedAt.current), 250);
        return () => {
            cancelled = true;
            if (unlisten) unlisten();
            clearInterval(clock);
        };
    }, [active]);

    return { progress: state, totals: scanProgressTotals(state), elapsedMs };
}

/** m:ss, or h:mm:ss once a scan has been running that long. */
export function formatElapsed(ms: number): string {
    const total = Math.floor(ms / 1000);
    const s = total % 60;
    const m = Math.floor(total / 60) % 60;
    const h = Math.floor(total / 3600);
    const mm = String(m).padStart(2, '0');
    const ss = String(s).padStart(2, '0');
    return h > 0 ? `${h}:${mm}:${ss}` : `${m}:${ss}`;
}
