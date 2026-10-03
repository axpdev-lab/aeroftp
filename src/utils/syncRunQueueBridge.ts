// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// #364: an AeroSync run transfers file by file through `runRemoteSync`, whose
// single-file commands never create a Transfer Queue row of their own, so the
// panel stayed empty for the whole run. This bridge turns the runner's
// per-file status into queue rows; once a row exists, the backend's
// single-file start / progress events attach to it by name as they do for a
// plain upload or download.

import type { SyncRunFile, SyncRunFileStatus } from './remoteSyncRunner';

/** The slice of `useTransferQueue` a sync run drives. */
export interface SyncRunQueue {
    addItem: (filename: string, path: string, size: number, type: 'upload' | 'download') => string;
    startTransfer: (id: string) => void;
    completeTransfer: (id: string) => void;
    failTransfer: (id: string, error: string) => void;
}

const basename = (p: string): string => p.split('/').pop() || p;

/**
 * The `onFileStatus` callback for one run of `files`. Only uploads and
 * downloads get a row: deletes and the folders the run creates are not
 * transfers. A row is added when its file starts syncing, so a file a cancel
 * skipped before it started never shows, and a retry keeps the same row and
 * puts it back in progress.
 */
export function syncRunQueueBridge(
    files: SyncRunFile[],
    queue: SyncRunQueue,
): (relativePath: string, status: SyncRunFileStatus, message?: string) => void {
    const transfers = new Map(
        files
            .filter((f) => f.action === 'upload' || f.action === 'download')
            .map((f) => [f.relativePath, f]),
    );
    const rowIds = new Map<string, string>();
    return (relativePath, status, message) => {
        const transfer = transfers.get(relativePath);
        if (!transfer) return;
        let id = rowIds.get(relativePath);
        if (status === 'syncing') {
            if (id === undefined) {
                // Named after the source file: the backend events carry that
                // name, which differs from `relativePath` for a keep-both copy.
                id = queue.addItem(
                    basename(transfer.sourcePath ?? relativePath),
                    relativePath,
                    transfer.size,
                    transfer.action === 'upload' ? 'upload' : 'download',
                );
                rowIds.set(relativePath, id);
            }
            queue.startTransfer(id);
            return;
        }
        if (id === undefined) return;
        // A retry runs the transfer again without a new `syncing`, and the
        // failed attempt's error event already turned the row red.
        if (status === 'retrying') queue.startTransfer(id);
        else if (status === 'success' || status === 'skipped') queue.completeTransfer(id);
        else if (status === 'error' || status === 'verify_failed') queue.failTransfer(id, message ?? '');
    };
}
