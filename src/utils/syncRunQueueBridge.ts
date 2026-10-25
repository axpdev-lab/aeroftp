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
import type { SyncJournal } from '../types';
import type { NewQueueItem } from '../components/transferQueueActions';

/** The slice of `useTransferQueue` a sync run drives. */
export interface SyncRunQueue {
    addItems: (items: NewQueueItem[]) => string[];
    startTransfer: (id: string) => void;
    completeTransfer: (id: string) => void;
    failTransfer: (id: string, error: string) => void;
}

const basename = (p: string): string => p.split('/').pop() || p;

/**
 * Register every upload and download before the run starts so the queue knows
 * its real denominator. Deletes and created folders are not transfers. The
 * stable rows then follow each file through starts, retries and settlement,
 * including a cancellation skip before its first start.
 */
export function syncRunQueueBridge(
    files: SyncRunFile[],
    queue: SyncRunQueue,
    resumeJournal?: SyncJournal,
): (relativePath: string, status: SyncRunFileStatus, message?: string) => void {
    const transfers = files.filter((f) => f.action === 'upload' || f.action === 'download');
    const transferByPath = new Map(transfers.map((f) => [f.relativePath, f]));
    const ids = transfers.length > 0 ? queue.addItems(transfers.map((transfer) => ({
        filename: basename(transfer.sourcePath ?? transfer.relativePath),
        path: transfer.relativePath,
        size: transfer.size,
        type: transfer.action === 'upload' ? 'upload' : 'download',
        options: { plannedSync: true },
    }))) : [];
    const rowIds = new Map(transfers.map((transfer, index) => [transfer.relativePath, ids[index]]));
    for (const entry of resumeJournal?.entries ?? []) {
        if (entry.status !== 'completed') continue;
        const id = rowIds.get(entry.relative_path);
        if (id !== undefined) queue.completeTransfer(id);
    }
    return (relativePath, status, message) => {
        if (!transferByPath.has(relativePath)) return;
        const id = rowIds.get(relativePath);
        if (id === undefined) return;
        if (status === 'syncing') {
            queue.startTransfer(id);
            return;
        }
        if (status === 'retrying') queue.startTransfer(id);
        else if (status === 'success' || status === 'skipped') queue.completeTransfer(id);
        else if (status === 'error' || status === 'verify_failed') queue.failTransfer(id, message ?? '');
    };
}
