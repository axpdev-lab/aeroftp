// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Pure state-transition helpers for the Transfer Queue (TQ-3 slice).
// Extracted from `useTransferQueue()` so the staging lifecycle can be unit
// tested under the vitest "pure-TS" project (no jsdom, no testing-library).
// The hook in `TransferQueue.tsx` wraps these into setState calls; the
// helpers themselves never touch React.
//
// Staging semantics (APPENDIX-TRANSFER-QUEUE `tasks/TQ-1_UX-spec.md`):
//
//   add(staged: true)  ->  STAGED
//   startAll()         ->  every STAGED entry transitions to PENDING (= QUEUED in the spec)
//   startStaged(id)    ->  just that entry transitions to PENDING
//   reorder(id, idx)   ->  STAGED entries can be re-ordered as a priority hint
//
// Reorder is a priority hint, not a strict sequence. The executor still runs
// `max_concurrent` transfers in parallel from PENDING; the reorder just
// influences the pick order. Entries that have already left STAGED cannot be
// re-prioritised (they are pinned in place by the executor).
//
// `pending` is the current code's term for what the UX spec calls QUEUED;
// keeping the existing string avoids a ripple-rename across the codebase
// while TQ-3 is gated OFF behind the auto-start setting (TQ-4 wiring).

import type { TransferItem, TransferStatus, TransferType } from './TransferQueue';

export interface AddItemOptions {
    /** When true, the new entry lands in `staged` and waits for an explicit
     *  start (per-item or via `startAll`). Default false preserves the
     *  legacy "add and run" behaviour. */
    staged?: boolean;
    /** Optional explicit id; tests pass it in to keep assertions stable.
     *  Production code lets the hook generate one. */
    id?: string;
    /** TQ-7b: mark item as restored from a previous session journal. */
    restored?: boolean;
    /** Folder transfer flag (set at enqueue when known). */
    isFolder?: boolean;
}

/** Apply a queue status transition. Kept pure so event-order races can be
 *  reproduced against the same transition used by the React hook. */
export function updateTransferStatus(
    items: TransferItem[],
    id: string,
    status: TransferStatus,
    progress?: number,
    error?: string,
): TransferItem[] {
    if (status === 'completed' && items.some(item => item.id === id && item.status === 'error')) {
        return items;
    }
    return items.map(item =>
        item.id === id
            ? {
                ...item,
                status,
                progress,
                error,
                speedBps: undefined,
                endTime: status === 'completed' || status === 'error' ? Date.now() : item.endTime,
            }
            : item,
    );
}

/** Push a new item to the queue. Pure: returns a fresh array; never mutates
 *  the input. */
export function addItem(
    items: TransferItem[],
    id: string,
    filename: string,
    path: string,
    size: number,
    type: TransferType,
    options?: AddItemOptions,
): TransferItem[] {
    const status: TransferStatus = options?.staged ? 'staged' : 'pending';
    return [
        ...items,
        {
            id,
            filename,
            path,
            size,
            type,
            status,
            startTime: Date.now(),
            restored: options?.restored || undefined,
            isFolder: options?.isFolder || undefined,
        },
    ];
}

/** Transition every STAGED entry to PENDING in current order. No-op when no
 *  staged entries exist. */
export function startAll(items: TransferItem[]): TransferItem[] {
    if (!items.some(i => i.status === 'staged')) return items;
    return items.map(item =>
        item.status === 'staged'
            ? { ...item, status: 'pending' as TransferStatus, startTime: Date.now() }
            : item,
    );
}

/** Transition a single STAGED entry to PENDING. Other statuses are left
 *  alone (idempotent for already-pending; ignored for terminal states). */
export function startStaged(items: TransferItem[], id: string): TransferItem[] {
    return items.map(item =>
        item.id === id && item.status === 'staged'
            ? { ...item, status: 'pending' as TransferStatus, startTime: Date.now() }
            : item,
    );
}

/** Drop an item from the queue regardless of status. STAGED entries can be
 *  pruned freely; entries the executor has picked up still get removed from
 *  the in-memory list (cancellation of the in-flight transfer is the
 *  caller's responsibility via the cancel_flag). */
export function removeItem(items: TransferItem[], id: string): TransferItem[] {
    return items.filter(item => item.id !== id);
}

/** Move the item identified by `fromId` to `toIndex` (0-based, clamped to
 *  the array bounds). Reorder is only valid while the item is STAGED; once
 *  it has been picked up by the executor it stays in place (see UX spec
 *  section 8). When the item is not staged the queue is returned unchanged.
 *  The helper preserves the position of every non-staged entry. */
export function reorder(
    items: TransferItem[],
    fromId: string,
    toIndex: number,
): TransferItem[] {
    const fromIdx = items.findIndex(i => i.id === fromId);
    if (fromIdx === -1) return items;
    if (items[fromIdx].status !== 'staged') return items;

    const clamped = Math.max(0, Math.min(toIndex, items.length - 1));
    if (clamped === fromIdx) return items;

    const next = items.slice();
    const [moved] = next.splice(fromIdx, 1);
    next.splice(clamped, 0, moved);
    return next;
}

/** Count staged entries. Useful for the "Start all" button enable state and
 *  for header badges ("{N} staged"). */
export function stagedCount(items: TransferItem[]): number {
    let n = 0;
    for (const item of items) {
        if (item.status === 'staged') n++;
    }
    return n;
}

/** Snapshot of every status's population, for header badges and tests. */
export interface StatusCounts {
    staged: number;
    pending: number;
    transferring: number;
    completed: number;
    error: number;
}

export function statusCounts(items: TransferItem[]): StatusCounts {
    const counts: StatusCounts = {
        staged: 0,
        pending: 0,
        transferring: 0,
        completed: 0,
        error: 0,
    };
    for (const item of items) {
        counts[item.status]++;
    }
    return counts;
}

/** TQ-6 pruned-set filter. The auto-start OFF batch executors in App.tsx
 *  keep an `idToEntry` map alongside their entries; at fire time they
 *  filter to the queue ids that survived user-pruning. Extracted here so
 *  the behaviour can be unit tested independently of React state.
 *
 *  Returns the entries in the original `idToEntry` insertion order; ids
 *  not present in `currentItems` are dropped.  */
export function filterSurvivingBatchEntries<T>(
    idToEntry: Map<string, T>,
    currentItems: ReadonlyArray<{ id: string }>,
): T[] {
    const currentIds = new Set(currentItems.map(i => i.id));
    const out: T[] = [];
    for (const [id, entry] of idToEntry.entries()) {
        if (currentIds.has(id)) out.push(entry);
    }
    return out;
}

/** Minimal shape of the real aggregate byte snapshot the footer bar needs
 *  (#364). `BatchProgressSnapshot` from `useTransferEvents` is structurally
 *  assignable to it, but the helper stays free of any React/tauri import so it
 *  runs under the pure-TS vitest project. */
export interface FooterBatchBytes {
    bytes_total: number;
    bytes_transferred: number;
    total: number;
    completed: number;
}

/** #364: compute the Transfer Queue footer aggregate percentage.
 *
 *  The footer historically drove the bar from an item-count "wave"
 *  (`waveDone / waveTotal`). For a folder/batch transfer that denominator is
 *  wrong mid-batch: queue items are enqueued LAZILY on `file_start`, so
 *  `items.length` excludes not-yet-started files and the wave pegs the bar near
 *  100% almost immediately. The backend already knows the true total upfront
 *  (`bytes_total` is fixed from the pre-scan), so when a batch snapshot is live
 *  AND items are still transferring we drive the bar from real bytes. If the
 *  backend reports no byte totals (e.g. a count-only batch) we fall back to its
 *  completed/total file counts, then finally to the item-count wave for
 *  single-file / non-batch transfers.
 *
 *  Always returns a value clamped to 0..100. */
export function computeFooterPercentage(
    waveTotal: number,
    waveDone: number,
    transferringCount: number,
    batchSnapshot: FooterBatchBytes | null | undefined,
): number {
    const clamp = (n: number) => Math.max(0, Math.min(100, n));
    const snap = batchSnapshot;
    const realPct = snap && transferringCount > 0
        ? (snap.bytes_total > 0
            ? (snap.bytes_transferred / snap.bytes_total) * 100
            : (snap.total > 0 ? (snap.completed / snap.total) * 100 : null))
        : null;
    if (realPct !== null && Number.isFinite(realPct)) return clamp(realPct);
    return waveTotal > 0 ? clamp((waveDone / waveTotal) * 100) : 0;
}
