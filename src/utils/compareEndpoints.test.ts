// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Tests for the compare classifier (Z.3.7). Covers each bucket transition
// under every policy plus the degraded-input paths (missing mtime, dir vs
// file mismatch, malformed entries).

import { describe, expect, it } from 'vitest';
import {
    compareEntries,
    namesToMirrorLeftToRight,
    namesToMirrorRightToLeft,
} from './compareEndpoints';

describe('compareEntries — bucket coverage', () => {
    it('returns empty result on empty inputs', () => {
        const result = compareEntries([], []);
        expect(result.totals).toEqual({ count: 0, bytes: 0 });
        for (const stats of Object.values(result.stats)) {
            expect(stats).toEqual({ count: 0, bytes: 0 });
        }
        expect(result.entries).toEqual([]);
    });

    it('classifies only-left and only-right', () => {
        const result = compareEntries(
            [{ name: 'a.txt', isDir: false, size: 10, mtimeMs: 1000 }],
            [{ name: 'b.txt', isDir: false, size: 20, mtimeMs: 1000 }],
        );
        expect(result.buckets['only-left'].map((entry) => entry.name)).toEqual(['a.txt']);
        expect(result.buckets['only-right'].map((entry) => entry.name)).toEqual(['b.txt']);
        expect(result.stats['only-left']).toEqual({ count: 1, bytes: 10 });
        expect(result.stats['only-right']).toEqual({ count: 1, bytes: 20 });
        expect(result.totals).toEqual({ count: 2, bytes: 30 });
    });

    it('classifies newer-left vs newer-right with size-and-mtime policy', () => {
        const result = compareEntries(
            [
                { name: 'fresh.txt', isDir: false, size: 100, mtimeMs: 10_000 },
                { name: 'stale.txt', isDir: false, size: 100, mtimeMs: 1_000 },
            ],
            [
                { name: 'fresh.txt', isDir: false, size: 100, mtimeMs: 1_000 },
                { name: 'stale.txt', isDir: false, size: 100, mtimeMs: 10_000 },
            ],
        );
        expect(result.buckets['newer-left'].map((entry) => entry.name)).toEqual(['fresh.txt']);
        expect(result.buckets['newer-right'].map((entry) => entry.name)).toEqual(['stale.txt']);
        expect(result.stats['newer-left']).toEqual({ count: 1, bytes: 100 });
        expect(result.stats['newer-right']).toEqual({ count: 1, bytes: 100 });
    });

    it('classifies same when sizes and mtimes match', () => {
        const result = compareEntries(
            [{ name: 'same.bin', isDir: false, size: 50, mtimeMs: 1_000 }],
            [{ name: 'same.bin', isDir: false, size: 50, mtimeMs: 1_500 }],
        );
        // 500ms drift is within the default 2000ms tolerance.
        expect(result.buckets.same.map((entry) => entry.name)).toEqual(['same.bin']);
        expect(result.stats.same).toEqual({ count: 1, bytes: 50 });
    });

    it('emits conflict when mtime is within tolerance but sizes differ', () => {
        const result = compareEntries(
            [{ name: 'tricky.bin', isDir: false, size: 50, mtimeMs: 1_000 }],
            [{ name: 'tricky.bin', isDir: false, size: 90, mtimeMs: 1_500 }],
        );
        expect(result.buckets.conflict.map((entry) => entry.name)).toEqual(['tricky.bin']);
        expect(result.stats.conflict.count).toBe(1);
    });

    it('treats dir↔file pairs as conflict', () => {
        const result = compareEntries(
            [{ name: 'mix', isDir: true }],
            [{ name: 'mix', isDir: false, size: 100, mtimeMs: 1_000 }],
        );
        expect(result.buckets.conflict.map((entry) => entry.name)).toEqual(['mix']);
    });

    it('treats dir↔dir pairs as same regardless of metadata (default)', () => {
        const result = compareEntries(
            [{ name: 'sub', isDir: true, size: 10 }],
            [{ name: 'sub', isDir: true, size: 999 }],
        );
        expect(result.buckets.same.map((entry) => entry.name)).toEqual(['sub']);
        expect(result.stats.same.bytes).toBe(0); // dirs do not contribute bytes
    });
});

describe('compareEntries — policy switches', () => {
    it('size-only ignores mtime drift', () => {
        const result = compareEntries(
            [{ name: 'a.bin', isDir: false, size: 100, mtimeMs: 1_000 }],
            [{ name: 'a.bin', isDir: false, size: 100, mtimeMs: 999_999 }],
            { policy: 'size-only' },
        );
        expect(result.buckets.same.map((entry) => entry.name)).toEqual(['a.bin']);
    });

    it('size-only flags conflict when sizes differ even with same mtime', () => {
        const result = compareEntries(
            [{ name: 'a.bin', isDir: false, size: 100, mtimeMs: 1_000 }],
            [{ name: 'a.bin', isDir: false, size: 101, mtimeMs: 1_000 }],
            { policy: 'size-only' },
        );
        expect(result.buckets.conflict.map((entry) => entry.name)).toEqual(['a.bin']);
    });

    it('mtime-only ignores size differences when mtime is close', () => {
        const result = compareEntries(
            [{ name: 'a.bin', isDir: false, size: 100, mtimeMs: 1_000 }],
            [{ name: 'a.bin', isDir: false, size: 9_000, mtimeMs: 1_000 }],
            { policy: 'mtime-only' },
        );
        expect(result.buckets.same.map((entry) => entry.name)).toEqual(['a.bin']);
    });

    it('mtime-only routes to newer-left/newer-right when mtime drifts', () => {
        const result = compareEntries(
            [
                { name: 'fresh.bin', isDir: false, size: 100, mtimeMs: 10_000 },
                { name: 'stale.bin', isDir: false, size: 100, mtimeMs: 1_000 },
            ],
            [
                { name: 'fresh.bin', isDir: false, size: 100, mtimeMs: 1_000 },
                { name: 'stale.bin', isDir: false, size: 100, mtimeMs: 10_000 },
            ],
            { policy: 'mtime-only' },
        );
        expect(result.buckets['newer-left'].map((entry) => entry.name)).toEqual(['fresh.bin']);
        expect(result.buckets['newer-right'].map((entry) => entry.name)).toEqual(['stale.bin']);
    });

    it('honours custom mtimeToleranceMs', () => {
        const result = compareEntries(
            [{ name: 'a.bin', isDir: false, size: 100, mtimeMs: 1_000 }],
            [{ name: 'a.bin', isDir: false, size: 100, mtimeMs: 2_500 }],
            { mtimeToleranceMs: 100 },
        );
        // 1500ms drift now exceeds the 100ms tolerance.
        expect(result.buckets['newer-right'].map((entry) => entry.name)).toEqual(['a.bin']);
    });
});

describe('compareEntries — degraded inputs', () => {
    it('falls back to size match when mtime is missing on at least one side', () => {
        const result = compareEntries(
            [{ name: 'a.bin', isDir: false, size: 100 }],
            [{ name: 'a.bin', isDir: false, size: 100, mtimeMs: 5_000 }],
        );
        expect(result.buckets.same.map((entry) => entry.name)).toEqual(['a.bin']);
    });

    it('flags conflict when mtime is missing and sizes differ', () => {
        const result = compareEntries(
            [{ name: 'a.bin', isDir: false, size: 100 }],
            [{ name: 'a.bin', isDir: false, size: 200, mtimeMs: 5_000 }],
        );
        expect(result.buckets.conflict.map((entry) => entry.name)).toEqual(['a.bin']);
    });

    it('skips malformed entries silently', () => {
        const result = compareEntries(
            [
                { name: '', isDir: false, size: 100 },
                // @ts-expect-error -- intentionally bad input
                { name: null, isDir: false },
                { name: 'ok.bin', isDir: false, size: 10, mtimeMs: 1_000 },
            ],
            [],
        );
        expect(result.totals.count).toBe(1);
        expect(result.buckets['only-left'].map((entry) => entry.name)).toEqual(['ok.bin']);
    });

    it('deduplicates first-wins on case-of-twice provider listings', () => {
        const result = compareEntries(
            [
                { name: 'dup.bin', isDir: false, size: 100, mtimeMs: 1_000 },
                { name: 'dup.bin', isDir: false, size: 9_999, mtimeMs: 9_999 },
            ],
            [],
        );
        // The second entry must be ignored.
        expect(result.buckets['only-left']).toHaveLength(1);
        expect(result.buckets['only-left'][0].leftSize).toBe(100);
    });
});

describe('mirror selectors', () => {
    it('selects only-left + newer-left for left→right mirror', () => {
        const result = compareEntries(
            [
                { name: 'a.bin', isDir: false, size: 1, mtimeMs: 10 },
                { name: 'b.bin', isDir: false, size: 1, mtimeMs: 10 },
                // 10s drift puts c.bin firmly above the 2000ms default tolerance.
                { name: 'c.bin', isDir: false, size: 1, mtimeMs: 10_000 },
            ],
            [
                { name: 'b.bin', isDir: false, size: 1, mtimeMs: 10 }, // same
                { name: 'c.bin', isDir: false, size: 1, mtimeMs: 10 }, // newer-left
                { name: 'd.bin', isDir: false, size: 1, mtimeMs: 10 }, // only-right
            ],
        );
        expect(namesToMirrorLeftToRight(result).sort()).toEqual(['a.bin', 'c.bin']);
        expect(namesToMirrorRightToLeft(result).sort()).toEqual(['d.bin']);
    });
});

describe('compareEntries — output ordering', () => {
    it('returns buckets sorted alphabetically inside each bucket', () => {
        const result = compareEntries(
            [
                { name: 'zeta.bin', isDir: false, size: 1, mtimeMs: 1 },
                { name: 'alpha.bin', isDir: false, size: 1, mtimeMs: 1 },
                { name: 'mid.bin', isDir: false, size: 1, mtimeMs: 1 },
            ],
            [],
        );
        expect(result.buckets['only-left'].map((entry) => entry.name)).toEqual([
            'alpha.bin',
            'mid.bin',
            'zeta.bin',
        ]);
    });

    it('echoes the applied options after defaults', () => {
        const result = compareEntries([], [], { policy: 'mtime-only' });
        expect(result.appliedOptions).toEqual({
            policy: 'mtime-only',
            mtimeToleranceMs: 2000,
            skipDirectoryComparison: true,
        });
    });
});
