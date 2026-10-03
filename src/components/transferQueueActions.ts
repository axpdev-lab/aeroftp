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
/** #591: a finished row stays finished. `completed` and `error` change only
 *  through an explicit retry, which resets the row to `pending` on its own
 *  path (`retryItem`, `retryAllFailed`). Without this a late event (a progress
 *  sample, a second start, an error for a file that already landed) reopened a
 *  settled row, and the queue showed a finished batch as still transferring. */
function isSettled(status: TransferStatus): boolean {
    return status === 'completed' || status === 'error';
}

export function updateTransferStatus(
    items: TransferItem[],
    id: string,
    status: TransferStatus,
    progress?: number,
    error?: string,
): TransferItem[] {
    const current = items.find(item => item.id === id);
    if (current && isSettled(current.status)) {
        // completed -> completed refreshes nothing worth a render; anything
        // else (reopen, flip to the other outcome, overwrite the first error
        // message) is refused.
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

/** Live progress for one row: only a row still pending or transferring takes
 *  it (see `isSettled`). */
export function applyItemProgress(
    items: TransferItem[],
    id: string,
    progress: number,
    speedBps?: number,
): TransferItem[] {
    const current = items.find(item => item.id === id);
    if (!current || isSettled(current.status) || current.status === 'staged') return items;
    return items.map(item =>
        item.id === id
            ? { ...item, status: 'transferring' as TransferStatus, progress, speedBps, error: undefined }
            : item,
    );
}

/** #591: the queue row a `file_start` event belongs to.
 *
 *  A file-list batch registers its rows before the backend starts, keyed by
 *  the per-file event id (`<batchId>-<index>`): that row wins, so it is never
 *  duplicated or missed, whether the event arrives before the queue re-rendered
 *  the new rows or after the row settled. Without a registration (folder
 *  batches enqueue their rows lazily, on this very event) it is the pending row
 *  with the same name, path and direction, or `null` to create one.
 *
 *  A registered row is returned without checking it still exists: if the user
 *  removed it between registration and this event, the queue updates are
 *  no-ops and no row is recreated for a file the user dropped from view. That
 *  is deliberate; the transfer itself still runs and is logged. */
export function rowForFileStart(
    registered: string | undefined,
    items: ReadonlyArray<Pick<TransferItem, 'id' | 'filename' | 'path' | 'status' | 'type'>>,
    file: { filename: string; path: string; type: TransferType },
): string | null {
    if (registered) return registered;
    const pending = items.find(item =>
        item.status === 'pending'
        && item.type === file.type
        && item.filename === file.filename
        && item.path === file.path);
    return pending ? pending.id : null;
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

// ---- TQ-7c: startup resume-queue prompt helpers ----

/** Pending items restored from the journal after restart (banner target). */
export function listRestoredPendingIds(items: ReadonlyArray<TransferItem>): string[] {
    const ids: string[] = [];
    for (const item of items) {
        if (item.restored && item.status === 'pending') ids.push(item.id);
    }
    return ids;
}

/** Clear the `restored` badge so the startup banner hides after Resume all. */
export function clearRestoredFlags(
    items: TransferItem[],
    ids?: ReadonlyArray<string>,
): TransferItem[] {
    const only = ids ? new Set(ids) : null;
    let changed = false;
    const next = items.map((item) => {
        if (!item.restored) return item;
        if (only && !only.has(item.id)) return item;
        changed = true;
        return { ...item, restored: false };
    });
    return changed ? next : items;
}

/**
 * Drop restored-and-still-pending items (Discard on the startup banner).
 * Returns the pruned queue and the removed ids (for descriptor/callback cleanup).
 */
export function removeRestoredPending(items: TransferItem[]): {
    next: TransferItem[];
    removedIds: string[];
} {
    const removedIds: string[] = [];
    const next = items.filter((item) => {
        if (item.restored && item.status === 'pending') {
            removedIds.push(item.id);
            return false;
        }
        return true;
    });
    return removedIds.length === 0 ? { next: items, removedIds } : { next, removedIds };
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

/** #591: the queue callbacks of ONE file batch (several plain files sent to
 *  the backend together).
 *
 *  With auto-start OFF every row is staged, and pressing Start flips them all
 *  to pending in one update: the App effect then calls every row's callback in
 *  the same tick. Those calls must produce ONE backend batch, not one batch
 *  plus a second copy of each file. A row's later Retry re-sends just that
 *  file. */
export interface FileBatchDispatcher {
    /** Send every row still in the queue as one batch. */
    launchAll(): Promise<void>;
    /** The callback to register for a queue row. */
    callbackFor(id: string): () => void;
}

/**
 * A Start of staged rows or a Retry is a new request, as a new transfer is: a
 * Stop pressed before it must not cancel it. The flags are re-armed when the
 * user acts, not inside the runner, so a Stop pressed during this run still
 * keeps the files after it from starting.
 */
export function rearmedOnUserAction(
    cancel: { batchCancelled: { current: boolean }; cancelLevel: { current: number } },
    callback: () => void,
): () => void {
    return () => {
        cancel.batchCancelled.current = false;
        cancel.cancelLevel.current = 0;
        callback();
    };
}

export function createFileBatchDispatcher<E>(options: {
    ids: readonly string[];
    entries: readonly E[];
    /** Runs the given entries; resolves with the ids of the files that landed. */
    run: (entries: E[], ids: string[]) => Promise<ReadonlySet<string>>;
    /** Current queue status of a row, `undefined` once the row was removed. */
    statusOf: (id: string) => string | undefined;
    /** Auto-start OFF: the first callback sends the whole batch. */
    firstStartRunsAll: boolean;
}): FileBatchDispatcher {
    const index = new Map(options.ids.map((id, i) => [id, i]));
    const fired = { all: !options.firstStartRunsAll };
    // Rows whose file is being sent right now, and rows whose callback came
    // while they were (a same-tick Start, or a Retry pressed mid-batch).
    const inFlight = new Set<string>();
    const deferred = new Set<string>();

    const launch = async (ids: string[]): Promise<void> => {
        if (ids.length === 0) return;
        // Marked before the first await, so a callback in the same tick sees it.
        for (const id of ids) inFlight.add(id);
        let landed: ReadonlySet<string> = new Set();
        try {
            landed = await options.run(ids.map(id => options.entries[index.get(id)!]), ids);
        } finally {
            for (const id of ids) inFlight.delete(id);
            // A deferred row goes again only if its file did NOT land and the
            // row still waits: that is a Retry pressed while the batch ran.
            // A file that landed is never sent twice.
            const again = ids.filter(id =>
                deferred.has(id) && !landed.has(id) && options.statusOf(id) === 'pending');
            for (const id of ids) deferred.delete(id);
            for (const id of again) void launch([id]);
        }
    };

    const launchAll = () => {
        fired.all = true;
        // Pruning applies to staged rows only: with auto-start ON the rows were
        // added a moment ago and the queue snapshot may not list them yet.
        const ids = options.firstStartRunsAll
            ? options.ids.filter(id => options.statusOf(id) !== undefined)
            : options.ids;
        return launch(ids.filter(id => !inFlight.has(id)));
    };

    return {
        launchAll,
        callbackFor: (id: string) => () => {
            if (!index.has(id)) return;
            if (!fired.all) {
                void launchAll();
                return;
            }
            if (inFlight.has(id)) {
                deferred.add(id);
                return;
            }
            void launch([id]);
        },
    };
}
