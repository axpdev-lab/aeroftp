// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { TransferItem, TransferStatus, TransferType } from './TransferQueue';
import {
    addItem,
    removeItem,
    reorder,
    stagedCount,
    startAll,
    startStaged,
    statusCounts,
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
