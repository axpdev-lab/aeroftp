// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { adaptFileComparisons } from './recursiveCompare';
import type { CompareSummary, FileComparison, FileInfo, SyncStatus } from '../types';

const info = (over: Partial<FileInfo> = {}): FileInfo => ({
    name: 'f',
    path: '/f',
    size: 100,
    modified: '2026-05-22T10:00:00Z',
    is_dir: false,
    checksum: null,
    ...over,
});

const fc = (
    relativePath: string,
    status: SyncStatus,
    over: Partial<FileComparison> = {},
): FileComparison => ({
    relative_path: relativePath,
    status,
    local_info: status === 'remote_only' ? null : info(),
    remote_info: status === 'local_only' ? null : info(),
    is_dir: false,
    sync_reason: '',
    ...over,
});

describe('adaptFileComparisons — status to bucket (left is local)', () => {
    it('maps every status onto the local-left orientation', () => {
        const rows: FileComparison[] = [
            fc('same.txt', 'identical'),
            fc('lonly.txt', 'local_only'),
            fc('ronly.txt', 'remote_only'),
            fc('lnew.txt', 'local_newer'),
            fc('rnew.txt', 'remote_newer'),
            fc('conf.txt', 'conflict'),
            fc('mism.txt', 'size_mismatch'),
        ];
        const result = adaptFileComparisons(rows, true);
        expect(result.buckets['same'].map((e) => e.name)).toEqual(['same.txt']);
        expect(result.buckets['only-left'].map((e) => e.name)).toEqual(['lonly.txt']);
        expect(result.buckets['only-right'].map((e) => e.name)).toEqual(['ronly.txt']);
        expect(result.buckets['newer-left'].map((e) => e.name)).toEqual(['lnew.txt']);
        expect(result.buckets['newer-right'].map((e) => e.name)).toEqual(['rnew.txt']);
        // conflict + size_mismatch collapse into the conflict bucket.
        expect(result.buckets['conflict'].map((e) => e.name).sort()).toEqual(['conf.txt', 'mism.txt']);
        expect(result.totals.count).toBe(7);
    });
});

describe('adaptFileComparisons — orientation', () => {
    it('inverts left/right when the left panel is the remote', () => {
        const rows: FileComparison[] = [
            fc('lonly.txt', 'local_only'),
            fc('ronly.txt', 'remote_only'),
        ];
        const result = adaptFileComparisons(rows, false);
        // left is remote → remote_only lands on only-left.
        expect(result.buckets['only-left'].map((e) => e.name)).toEqual(['ronly.txt']);
        expect(result.buckets['only-right'].map((e) => e.name)).toEqual(['lonly.txt']);
    });
});

describe('adaptFileComparisons — recursive paths', () => {
    it('carries the nested relative_path and derives the basename', () => {
        const result = adaptFileComparisons(
            [fc('docs/reports/q1.pdf', 'local_only')],
            true,
        );
        const entry = result.buckets['only-left'][0];
        expect(entry.relativePath).toBe('docs/reports/q1.pdf');
        expect(entry.name).toBe('q1.pdf');
    });

    it('sorts entries inside a bucket by full relative path', () => {
        const result = adaptFileComparisons(
            [
                fc('z.txt', 'local_only'),
                fc('a/deep.txt', 'local_only'),
                fc('a/aa.txt', 'local_only'),
            ],
            true,
        );
        expect(result.buckets['only-left'].map((e) => e.relativePath)).toEqual([
            'a/aa.txt',
            'a/deep.txt',
            'z.txt',
        ]);
    });
});

describe('adaptFileComparisons — directory filtering', () => {
    it('keeps a genuinely new directory but drops a both-sides directory', () => {
        const rows: FileComparison[] = [
            fc('newdir', 'local_only', { is_dir: true, local_info: info({ is_dir: true }) }),
            fc('shareddir', 'identical', {
                is_dir: true,
                local_info: info({ is_dir: true }),
                remote_info: info({ is_dir: true }),
            }),
        ];
        const result = adaptFileComparisons(rows, true);
        expect(result.buckets['only-left'].map((e) => e.name)).toEqual(['newdir']);
        // The directory present on both sides carries no content delta.
        expect(result.buckets['same']).toHaveLength(0);
    });
});

describe('adaptFileComparisons — side metadata', () => {
    it('threads size and mtime from the matching side', () => {
        const ms = Date.UTC(2026, 4, 22, 12, 0, 0);
        const rows: FileComparison[] = [
            fc('big.bin', 'conflict', {
                local_info: info({ size: 4096, modified: new Date(ms).toISOString() }),
                remote_info: info({ size: 2048, modified: new Date(ms).toISOString() }),
            }),
        ];
        const result = adaptFileComparisons(rows, true);
        const entry = result.buckets['conflict'][0];
        expect(entry.leftSize).toBe(4096);
        expect(entry.rightSize).toBe(2048);
        expect(entry.leftMtimeMs).toBe(ms);
        // Bytes at stake = the larger side.
        expect(result.stats['conflict'].bytes).toBe(4096);
    });
});

describe('adaptFileComparisons: backend summary', () => {
    // The user repro behind the "always 100% out of sync" report: try1 holds
    // a.txt (identical) and b.txt (300 bytes, missing from try2). The backend
    // rows carry only b.txt; the summary counts the identical file the rows
    // omit, so the Compare tab reports 50% out of sync by count.
    const summary: CompareSummary = {
        examined_count: 2,
        identical_count: 1,
        examined_bytes: 400,
        identical_bytes: 100,
    };

    it('uses the summary as the totals denominator instead of the rows', () => {
        const rows: FileComparison[] = [
            fc('b.txt', 'local_only', {
                local_info: info({ name: 'b.txt', size: 300 }),
                remote_info: null,
            }),
        ];
        const result = adaptFileComparisons(rows, true, summary);
        expect(result.totals.count).toBe(2);
        expect(result.totals.bytes).toBe(400);
        expect(result.stats.same.count).toBe(1);
        expect(result.stats.same.bytes).toBe(100);
        // The formula the Compare tab renders.
        const outOfSyncCount =
            (result.totals.count - result.stats.same.count) / result.totals.count;
        expect(outOfSyncCount).toBe(0.5);
        const outOfSyncBytes =
            (result.totals.bytes - result.stats.same.bytes) / result.totals.bytes;
        expect(outOfSyncBytes).toBe(0.75);
    });

    it('reports zero differences over the full examined denominator', () => {
        const full: CompareSummary = {
            examined_count: 3,
            identical_count: 3,
            examined_bytes: 600,
            identical_bytes: 600,
        };
        const result = adaptFileComparisons([], true, full);
        expect(result.entries).toHaveLength(0);
        expect(result.totals.count).toBe(3);
        expect(result.stats.same.count).toBe(3);
    });
});
