// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { FolderSizeResult } from '../types/aerofile';

/** One `folder-size-progress` event of a remote count. */
export interface RemoteSizeProgress {
    scan_id?: string | null;
    total_bytes: number;
    file_count: number;
    dir_count: number;
    partial: boolean;
}

/** What a remote count reads and writes, so that it runs without the app. */
export interface RemoteFolderSizeDeps {
    /** Registers a handler for progress events; resolves to its removal. */
    listen: (handler: (progress: RemoteSizeProgress) => void) => Promise<() => void>;
    /** Counts `path` on the provider, tagging its progress with `scanId`. */
    count: (path: string, scanId: string) => Promise<FolderSizeResult>;
    newScanId: () => string;
    /** Replaces the figure under `key`; `undefined` removes it. */
    setFigure: (key: string, update: (prev: FolderSizeResult | undefined) => FolderSizeResult | undefined) => void;
    setCalculating: (key: string, calculating: boolean) => void;
    onError: (err: unknown) => void;
}

export interface RemoteFolderSizes {
    /** Counts the folder `path` under `key`, unless it is being counted. */
    count: (path: string, key: string) => Promise<void>;
    /** Ends the count in progress: its dialog closed. */
    stop: () => void;
}

/**
 * Remote folder sizes for the Properties dialog.
 *
 * The provider counts one folder at a time, and a new count cancels the last,
 * which may still report, and return, after the next one started. So one
 * count is kept: its progress events carry its id, and only the count kept
 * writes a figure, marks it partial on an error, or ends the spinner. A count
 * ended (its dialog closed, another started) leaves no figure: a stopped walk
 * is not the folder's size, and the next open counts the folder afresh.
 */
export function remoteFolderSizes(deps: RemoteFolderSizeDeps): RemoteFolderSizes {
    let kept: { key: string; scanId: string } | null = null;

    const stop = () => {
        if (!kept) return;
        const { key } = kept;
        kept = null;
        deps.setCalculating(key, false);
        deps.setFigure(key, () => undefined);
    };

    const count = async (path: string, key: string) => {
        if (kept?.key === key) return;
        stop();
        const scanId = deps.newScanId();
        kept = { key, scanId };
        const current = () => kept?.scanId === scanId;
        deps.setCalculating(key, true);
        let unlisten: (() => void) | undefined;
        try {
            unlisten = await deps.listen((p) => {
                if (p.scan_id !== scanId || !current()) return;
                deps.setFigure(key, () => ({
                    total_bytes: p.total_bytes,
                    file_count: p.file_count,
                    dir_count: p.dir_count,
                    partial: p.partial,
                }));
            });
            // The dialog may have closed while the listener was set up.
            if (!current()) return;
            const result = await deps.count(path, scanId);
            if (current()) deps.setFigure(key, () => result);
        } catch (err) {
            if (current()) {
                deps.setFigure(key, (prev) => (prev ? { ...prev, partial: true } : prev));
                deps.onError(err);
            }
        } finally {
            unlisten?.();
            if (current()) {
                kept = null;
                deps.setCalculating(key, false);
            }
        }
    };

    return { count, stop };
}
