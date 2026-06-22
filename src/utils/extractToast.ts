// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { dispatchTransferToast } from '../components/Transfer/TransferToastContainer';
import type { TransferProgress } from '../components/Transfer';

/**
 * Archives at or above this size get the global transfer toast as a progress
 * indicator during a headless (no-modal) context-menu extract. Matches the
 * backend `archive_progress` / `vault_progress` 10 MiB emit threshold so the
 * bar and the emitter agree on when a substantial extract is worth surfacing.
 */
export const EXTRACT_TOAST_THRESHOLD_BYTES = 10 * 1024 * 1024;

/** Raw payload of `archive_progress` (phase/indeterminate) and `vault_progress`
 *  (always determinate) frames; the union is normalized below. */
interface ArchiveFrame {
    phase?: string;
    percentage: number;
    transferred: number;
    total: number;
    indeterminate?: boolean;
}

export interface ExtractToastResult<T> {
    result: T;
    /**
     * The largest determinate `total` observed across progress frames, i.e. the
     * extracted byte total. `0` when no determinate frame arrived (compressed
     * tar, RAR, or a sub-threshold extract), in which case the size is unknown
     * and the caller should not report an output figure.
     */
    extractedTotal: number;
}

let extractToastSeq = 0;

/**
 * Run a headless (context-menu) extract command, surfacing the global transfer
 * toast as a progress indicator for archives at or above the size threshold.
 *
 * The indicator is byte-true while the backend emits determinate frames
 * (`archive_progress` / `vault_progress` with a known total > 0) and honestly
 * indeterminate otherwise: a `total: 0` summary makes `ProgressCard` render a
 * spinner ("working, size unknown") rather than a fake moving percentage.
 * Sub-threshold extracts run with no toast at all (instant, no flicker).
 *
 * Both progress events are listened to because a given command emits only one
 * of them (`aerovz_extract_all` -> `vault_progress`, the zip/7z/tar lane ->
 * `archive_progress`); the listeners attach BEFORE `invoke` so the first frame
 * can never be lost to an attach race. Returns the command result plus the
 * extracted byte total (for activity-log size reporting).
 */
export async function runExtractWithToast<T>(
    cmd: string,
    args: Record<string, unknown>,
    opts: { filename: string; archiveBytes: number | null | undefined },
): Promise<ExtractToastResult<T>> {
    const archiveBytes = opts.archiveBytes ?? 0;
    if (archiveBytes < EXTRACT_TOAST_THRESHOLD_BYTES) {
        const result = await invoke<T>(cmd, args);
        return { result, extractedTotal: 0 };
    }

    const transferId = `extract-${++extractToastSeq}`;
    let extractedTotal = 0;
    let lastT = performance.now();
    let lastBytes = 0;
    let speed = 0; // EMA bytes/sec, derived from successive determinate frames

    const indeterminateSummary = (): TransferProgress => ({
        transfer_id: transferId,
        filename: opts.filename,
        total: 0,
        transferred: 0,
        percentage: 0,
        speed_bps: 0,
        eta_seconds: 0,
        direction: 'download',
    });

    // Immediate indeterminate frame so the indicator appears before the first
    // backend frame (which, for a single huge file, arrives only at completion).
    dispatchTransferToast({ summary: indeterminateSummary() });

    const onFrame = (f: ArchiveFrame) => {
        const determinate = f.total > 0 && !f.indeterminate;
        if (!determinate) {
            dispatchTransferToast({ summary: indeterminateSummary() });
            return;
        }
        extractedTotal = Math.max(extractedTotal, f.total);
        const now = performance.now();
        const dt = (now - lastT) / 1000;
        if (dt > 0.05 && f.transferred >= lastBytes) {
            const inst = (f.transferred - lastBytes) / dt;
            speed = speed === 0 ? inst : speed * 0.6 + inst * 0.4;
        }
        lastT = now;
        lastBytes = f.transferred;
        const remaining = Math.max(0, f.total - f.transferred);
        const eta = speed > 0 && remaining > 0 ? remaining / speed : 0;
        dispatchTransferToast({
            summary: {
                transfer_id: transferId,
                filename: opts.filename,
                total: f.total,
                transferred: f.transferred,
                percentage: f.percentage,
                speed_bps: speed > 0 ? speed : 0,
                eta_seconds: eta,
                direction: 'download',
            },
        });
    };

    const unlisten: UnlistenFn[] = [];
    try {
        unlisten.push(await listen<ArchiveFrame>('archive_progress', (e) => onFrame(e.payload)));
        unlisten.push(await listen<ArchiveFrame>('vault_progress', (e) => onFrame(e.payload)));
        const result = await invoke<T>(cmd, args);
        return { result, extractedTotal };
    } finally {
        unlisten.forEach((u) => { try { u(); } catch { /* already detached */ } });
        dispatchTransferToast(null);
    }
}
