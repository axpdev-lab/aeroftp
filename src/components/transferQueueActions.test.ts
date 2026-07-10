// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { TransferItem, TransferStatus, TransferType } from './TransferQueue';
import {
    addItem,
    computeFooterPercentage,
    filterSurvivingBatchEntries,
    removeItem,
    reorder,
    stagedCount,
    startAll,
    startStaged,
    statusCounts,
    updateTransferStatus,
} from './transferQueueActions';

const makeItem = (
    id: string,
    status: TransferStatus,
    extra?: Partial<TransferItem>,
): TransferItem => ({
    id,
    filename: extra?.filename ?? `${id}.txt`,
    path: extra?.path ?? `/${id}.txt`,
    size: extra?.size ?? 1024,
    type: (extra?.type ?? 'upload') as TransferType,
    status,
    startTime: extra?.startTime,
});

describe('addItem', () => {
    it('defaults new entries to pending (legacy auto-start behaviour)', () => {
        const next = addItem([], 't1', 'a.txt', '/a.txt', 100, 'upload');
        expect(next).toHaveLength(1);
        expect(next[0].status).toBe('pending');
    });
    it('parks new entries in staged when options.staged=true', () => {
        const next = addItem([], 't1', 'a.txt', '/a.txt', 100, 'upload', { staged: true });
        expect(next[0].status).toBe('staged');
    });
    it('appends without mutating the input array', () => {
        const input = [makeItem('t0', 'completed')];
        const next = addItem(input, 't1', 'a.txt', '/a.txt', 100, 'download');
        expect(next).not.toBe(input);
        expect(input).toHaveLength(1);
        expect(next).toHaveLength(2);
    });
});

describe('completion guard', () => {
    it('does not turn an errored same-name item into a completed transfer', () => {
        const input = [
            makeItem('failed', 'error', {
                filename: 'report.csv',
                path: '/first/report.csv',
                error: 'Permission denied',
            }),
            makeItem('active', 'transferring', {
                filename: 'report.csv',
                path: '/second/report.csv',
            }),
        ];

        const next = updateTransferStatus(input, 'failed', 'completed', 100);
        expect(next.find(item => item.id === 'failed')?.status).toBe('error');
        expect(next.find(item => item.id === 'active')?.status).toBe('transferring');
    });

    it('completes the tracked active item without changing an errored namesake', () => {
        const input = [
            makeItem('failed', 'error', {
                filename: 'report.csv',
                error: 'Permission denied',
            }),
            makeItem('active', 'transferring', {
                filename: 'report.csv',
            }),
        ];

        const next = updateTransferStatus(input, 'active', 'completed', 100);
        expect(next.find(item => item.id === 'failed')?.status).toBe('error');
        expect(next.find(item => item.id === 'active')?.status).toBe('completed');
    });
});

describe('startAll', () => {
    it('promotes every staged entry to pending', () => {
        const input = [
            makeItem('a', 'staged'),
            makeItem('b', 'staged'),
            makeItem('c', 'completed'),
        ];
        const next = startAll(input);
        expect(next.find(i => i.id === 'a')!.status).toBe('pending');
        expect(next.find(i => i.id === 'b')!.status).toBe('pending');
        expect(next.find(i => i.id === 'c')!.status).toBe('completed');
    });
    it('is a no-op when no staged entries exist', () => {
        const input = [makeItem('a', 'completed'), makeItem('b', 'pending')];
        const next = startAll(input);
        expect(next).toBe(input);
    });
    it('preserves order of all entries', () => {
        const input = [
            makeItem('a', 'staged'),
            makeItem('b', 'completed'),
            makeItem('c', 'staged'),
        ];
        const next = startAll(input);
        expect(next.map(i => i.id)).toEqual(['a', 'b', 'c']);
    });
});

describe('startStaged', () => {
    it('promotes the matching staged entry to pending', () => {
        const input = [makeItem('a', 'staged'), makeItem('b', 'staged')];
        const next = startStaged(input, 'a');
        expect(next.find(i => i.id === 'a')!.status).toBe('pending');
        expect(next.find(i => i.id === 'b')!.status).toBe('staged');
    });
    it('ignores entries that are not staged (no accidental retry)', () => {
        const input = [makeItem('a', 'completed'), makeItem('b', 'error')];
        const next = startStaged(input, 'a');
        expect(next.find(i => i.id === 'a')!.status).toBe('completed');
    });
    it('returns the same shape when id is unknown', () => {
        const input = [makeItem('a', 'staged')];
        const next = startStaged(input, 'zzz');
        expect(next.map(i => i.status)).toEqual(['staged']);
    });
});

describe('removeItem', () => {
    it('drops the matching id regardless of status', () => {
        const input = [
            makeItem('a', 'staged'),
            makeItem('b', 'transferring'),
            makeItem('c', 'completed'),
        ];
        expect(removeItem(input, 'a').map(i => i.id)).toEqual(['b', 'c']);
        expect(removeItem(input, 'b').map(i => i.id)).toEqual(['a', 'c']);
        expect(removeItem(input, 'c').map(i => i.id)).toEqual(['a', 'b']);
    });
    it('returns a fresh array when id is unknown', () => {
        const input = [makeItem('a', 'staged')];
        const next = removeItem(input, 'zzz');
        expect(next).not.toBe(input);
        expect(next).toEqual(input);
    });
});

describe('reorder', () => {
    it('moves a staged entry to the requested index', () => {
        const input = [
            makeItem('a', 'staged'),
            makeItem('b', 'staged'),
            makeItem('c', 'staged'),
        ];
        // Move c to position 0
        const next = reorder(input, 'c', 0);
        expect(next.map(i => i.id)).toEqual(['c', 'a', 'b']);
    });
    it('refuses to reorder a non-staged entry (pinned by the executor)', () => {
        const input = [
            makeItem('a', 'transferring'),
            makeItem('b', 'staged'),
        ];
        const next = reorder(input, 'a', 1);
        expect(next).toBe(input);
    });
    it('clamps the destination index to array bounds', () => {
        const input = [
            makeItem('a', 'staged'),
            makeItem('b', 'staged'),
            makeItem('c', 'staged'),
        ];
        const next = reorder(input, 'a', 99);
        expect(next.map(i => i.id)).toEqual(['b', 'c', 'a']);
    });
    it('is a no-op when target index equals current index', () => {
        const input = [makeItem('a', 'staged'), makeItem('b', 'staged')];
        const next = reorder(input, 'a', 0);
        expect(next).toBe(input);
    });
    it('returns the input when fromId is unknown', () => {
        const input = [makeItem('a', 'staged')];
        const next = reorder(input, 'zzz', 0);
        expect(next).toBe(input);
    });
});

describe('filterSurvivingBatchEntries (TQ-6 pruned-set)', () => {
    type FakeEntry = { display_name: string; size: number };
    const fakeEntries: Array<[string, FakeEntry]> = [
        ['t1', { display_name: 'a.bin', size: 100 }],
        ['t2', { display_name: 'b.bin', size: 200 }],
        ['t3', { display_name: 'c.bin', size: 300 }],
        ['t4', { display_name: 'd.bin', size: 400 }],
        ['t5', { display_name: 'e.bin', size: 500 }],
    ];

    it('returns every entry when nothing was pruned', () => {
        const idToEntry = new Map(fakeEntries);
        const currentItems = fakeEntries.map(([id]) => ({ id }));
        const remaining = filterSurvivingBatchEntries(idToEntry, currentItems);
        expect(remaining.map(e => e.display_name)).toEqual(['a.bin', 'b.bin', 'c.bin', 'd.bin', 'e.bin']);
    });

    it("drops user-removed entries (ironhussar's 5-subdir example: prune 2 of 5)", () => {
        const idToEntry = new Map(fakeEntries);
        // Simulate the user removing t2 and t4 from the staged panel
        const currentItems = [{ id: 't1' }, { id: 't3' }, { id: 't5' }];
        const remaining = filterSurvivingBatchEntries(idToEntry, currentItems);
        expect(remaining.map(e => e.display_name)).toEqual(['a.bin', 'c.bin', 'e.bin']);
    });

    it('returns an empty array when every entry was pruned', () => {
        const idToEntry = new Map(fakeEntries);
        const remaining = filterSurvivingBatchEntries(idToEntry, []);
        expect(remaining).toEqual([]);
    });

    it('preserves the original insertion order, not the queue order', () => {
        const idToEntry = new Map(fakeEntries);
        // Even if the queue reports a reordered set, the entries come back in
        // their original idToEntry insertion order (the backend expects the
        // batch in the order the entries were enumerated, not in user-shuffled
        // priority order).
        const currentItems = [{ id: 't5' }, { id: 't3' }, { id: 't1' }];
        const remaining = filterSurvivingBatchEntries(idToEntry, currentItems);
        expect(remaining.map(e => e.display_name)).toEqual(['a.bin', 'c.bin', 'e.bin']);
    });

    it('ignores spurious ids in the queue that are not in the batch map', () => {
        const idToEntry = new Map(fakeEntries);
        // 'foreign' is a queue id that belongs to a different operation
        // (e.g. an unrelated direct upload that races with the batch). The
        // filter must not pick it up.
        const currentItems = [{ id: 't1' }, { id: 'foreign' }, { id: 't2' }];
        const remaining = filterSurvivingBatchEntries(idToEntry, currentItems);
        expect(remaining.map(e => e.display_name)).toEqual(['a.bin', 'b.bin']);
    });
});

describe('staged lifecycle scenario (TQ-6)', () => {
    // End-to-end check via the pure helpers: stage 5, prune 2, start all.
    // Mirrors ironhussar's reported flow on #180.
    it('stage 5 -> prune 2 -> startAll -> 3 pending in original order', () => {
        let items: TransferItem[] = [];
        items = addItem(items, 't1', 'a.bin', '/a.bin', 100, 'upload', { staged: true });
        items = addItem(items, 't2', 'b.bin', '/b.bin', 200, 'upload', { staged: true });
        items = addItem(items, 't3', 'c.bin', '/c.bin', 300, 'upload', { staged: true });
        items = addItem(items, 't4', 'd.bin', '/d.bin', 400, 'upload', { staged: true });
        items = addItem(items, 't5', 'e.bin', '/e.bin', 500, 'upload', { staged: true });

        // User prunes t2 and t4 from the panel.
        items = removeItem(items, 't2');
        items = removeItem(items, 't4');
        expect(stagedCount(items)).toBe(3);

        // User clicks Start all.
        items = startAll(items);
        const finalIds = items.map(i => i.id);
        expect(finalIds).toEqual(['t1', 't3', 't5']);
        expect(items.every(i => i.status === 'pending')).toBe(true);
    });
});

describe('counters', () => {
    it('stagedCount only counts staged entries', () => {
        const input = [
            makeItem('a', 'staged'),
            makeItem('b', 'pending'),
            makeItem('c', 'staged'),
            makeItem('d', 'completed'),
        ];
        expect(stagedCount(input)).toBe(2);
    });
    it('statusCounts gives a per-status snapshot', () => {
        const input = [
            makeItem('a', 'staged'),
            makeItem('b', 'pending'),
            makeItem('c', 'transferring'),
            makeItem('d', 'completed'),
            makeItem('e', 'error'),
            makeItem('f', 'staged'),
        ];
        expect(statusCounts(input)).toEqual({
            staged: 2,
            pending: 1,
            transferring: 1,
            completed: 1,
            error: 1,
        });
    });
});

describe('computeFooterPercentage (#364 folder-batch aggregate bar)', () => {
    const snap = (bytes_transferred: number, bytes_total: number, completed = 0, total = 0) => ({
        bytes_transferred,
        bytes_total,
        completed,
        total,
    });

    it('drives the bar from real bytes when a batch snapshot is live and items transfer', () => {
        // The bug: a folder upload enqueues items lazily on file_start, so early
        // on only 2 of ~100 files exist in the queue. The item-count wave would
        // read (waveDone / waveTotal) ~= 100% while barely any bytes have moved.
        // With the real snapshot (300MB of 1GB done) the bar must read ~30%.
        const wave = computeFooterPercentage(
            /* waveTotal */ 2,
            /* waveDone  */ 2, // item-count wave alone => 100% (the pegged bug)
            /* transferringCount */ 1,
            snap(300_000_000, 1_000_000_000),
        );
        expect(wave).toBeCloseTo(30, 5);
    });

    it('reaches 100 only when the real bytes are fully transferred', () => {
        expect(computeFooterPercentage(2, 2, 1, snap(1_000_000_000, 1_000_000_000))).toBe(100);
    });

    it('starts near 0 at the top of a fresh batch (not pegged high)', () => {
        const pct = computeFooterPercentage(1, 1, 1, snap(0, 1_000_000_000));
        expect(pct).toBe(0);
    });

    it('falls back to the item-count wave when no snapshot is present', () => {
        // Single-file / non-batch transfers keep the historical wave behaviour.
        expect(computeFooterPercentage(4, 1, 1, null)).toBeCloseTo(25, 5);
        expect(computeFooterPercentage(4, 1, 1, undefined)).toBeCloseTo(25, 5);
    });

    it('ignores the snapshot once nothing is transferring (wave holds the finished state)', () => {
        // transferringCount === 0 => a finished/idle wave keeps its own value
        // rather than resurrecting a stale snapshot.
        expect(computeFooterPercentage(3, 3, 0, snap(100, 1_000_000_000))).toBe(100);
    });

    it('falls back to file counts when the backend reports no byte totals', () => {
        // A count-only batch (bytes_total === 0) uses completed/total files.
        expect(computeFooterPercentage(2, 2, 1, snap(0, 0, 3, 12))).toBeCloseTo(25, 5);
    });

    it('clamps a real percentage into 0..100', () => {
        // Defensive: a late completed-file snapshot could momentarily overshoot.
        expect(computeFooterPercentage(1, 1, 1, snap(1_200_000_000, 1_000_000_000))).toBe(100);
    });

    it('returns 0 when there is nothing to show', () => {
        expect(computeFooterPercentage(0, 0, 0, null)).toBe(0);
    });
});
