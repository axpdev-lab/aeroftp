// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// Pure helpers for TQ-7b: map UI transfer-queue state to the TQ-7a journal
// DTO (snake_case, matching transfer_queue_journal.rs) and back. No I/O.

import type { TransferItem, TransferStatus, TransferType } from '../components/TransferQueue';

/** Journal direction values (serde rename_all = "snake_case"). */
export type JournalDirection = 'upload' | 'download';

/** Journal status values (serde rename_all = "snake_case"). */
export type JournalStatus =
    | 'pending'
    | 'in_progress'
    | 'completed'
    | 'failed'
    | 'cancelled';

/**
 * Side-map fields captured at enqueue time. TransferItem only has a single
 * `path`, so local/remote/profile must live here for re-execution after restart.
 */
export interface JournalDescriptorFields {
    direction: JournalDirection;
    local_path: string;
    remote_path: string;
    profile_id: string | null;
    filename: string;
    size: number;
    is_folder: boolean;
}

/** Full journal entry as accepted by save_transfer_queue_journal_cmd. */
export interface TransferQueueJournalEntryDto {
    id: string;
    direction: JournalDirection;
    local_path: string;
    remote_path: string;
    profile_id: string | null;
    filename: string;
    size: number;
    is_folder: boolean;
    status: JournalStatus;
    attempts: number;
    last_error?: string;
}

/** Payload of load_transfer_queue_journal_cmd (null when no file). */
export interface TransferQueueJournalDto {
    updated_at: string;
    entries: TransferQueueJournalEntryDto[];
}

/** True when the error message is a user cancel (matches TransferQueue retry filter). */
export function isCancelledTransferError(error: string | undefined): boolean {
    if (!error) return false;
    const lower = error.toLowerCase();
    return lower.includes('stopped by user') || lower.includes('cancel');
}

/**
 * Map UI TransferStatus (+ optional error) to journal QueueItemStatus.
 * staged|pending -> pending; transferring -> in_progress; completed -> completed;
 * error that looks cancelled -> cancelled; other error -> failed.
 */
export function mapUiStatusToJournalStatus(
    status: TransferStatus,
    error?: string,
): JournalStatus {
    switch (status) {
        case 'staged':
        case 'pending':
            return 'pending';
        case 'transferring':
            return 'in_progress';
        case 'completed':
            return 'completed';
        case 'error':
            return isCancelledTransferError(error) ? 'cancelled' : 'failed';
        default:
            return 'pending';
    }
}

/** Entries that should reappear after restart (not completed/cancelled). */
export function isRestorableJournalStatus(status: JournalStatus | string): boolean {
    return status !== 'completed' && status !== 'cancelled';
}

/**
 * Build journal entries by joining live queue items with the side descriptor map.
 * Items without a descriptor are skipped (cannot re-execute).
 * Prunes descriptor map keys that no longer exist in the items list when
 * `pruneMissing` is true (mutates the map).
 */
export function buildJournalEntries(
    items: TransferItem[],
    descriptors: Map<string, JournalDescriptorFields>,
    options?: { pruneMissing?: boolean; attempts?: number },
): TransferQueueJournalEntryDto[] {
    const prune = options?.pruneMissing ?? false;
    const attempts = options?.attempts ?? 0;
    const liveIds = new Set(items.map((i) => i.id));

    if (prune) {
        for (const id of [...descriptors.keys()]) {
            if (!liveIds.has(id)) descriptors.delete(id);
        }
    }

    const entries: TransferQueueJournalEntryDto[] = [];
    for (const item of items) {
        const desc = descriptors.get(item.id);
        if (!desc) continue;
        const status = mapUiStatusToJournalStatus(item.status, item.error);
        const entry: TransferQueueJournalEntryDto = {
            id: item.id,
            direction: desc.direction,
            local_path: desc.local_path,
            remote_path: desc.remote_path,
            profile_id: desc.profile_id,
            filename: desc.filename,
            size: desc.size,
            is_folder: desc.is_folder,
            status,
            attempts,
        };
        if (item.error) {
            entry.last_error = item.error;
        }
        entries.push(entry);
    }
    return entries;
}

/**
 * Parent directory of a path. Accepts `/` and `\` separators (local Windows paths).
 * Returns `/` for absolute single-segment paths, `''` for bare filenames.
 */
export function parentDir(path: string): string {
    if (!path) return '';
    const normalized = path.replace(/\\/g, '/');
    // Strip trailing slash (except root)
    const trimmed =
        normalized.length > 1 && normalized.endsWith('/')
            ? normalized.slice(0, -1)
            : normalized;
    const idx = trimmed.lastIndexOf('/');
    if (idx < 0) return '';
    if (idx === 0) return '/';
    return trimmed.slice(0, idx);
}

/** Join remote dir + filename with a single slash. */
export function joinRemotePath(dir: string, filename: string): string {
    if (!dir || dir === '/') return `/${filename}`.replace(/\/+/g, '/');
    return `${dir}${dir.endsWith('/') ? '' : '/'}${filename}`;
}

/** Display path for a restored item (local for upload, remote for download). */
export function displayPathForRestore(
    direction: JournalDirection | TransferType,
    localPath: string,
    remotePath: string,
): string {
    return direction === 'upload' ? localPath : remotePath;
}
