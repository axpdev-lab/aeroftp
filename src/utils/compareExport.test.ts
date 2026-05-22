// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// GAP-9c — tests for the Compare-tab dry-run export.

import { describe, expect, it } from 'vitest';
import { compareEntries } from './compareEndpoints';
import type { CompareResult, CompareResultEntry } from './compareEndpoints';
import {
    buildCompareExportRows,
    compareExportFilename,
    compareRowsToCsv,
    compareRowsToJson,
    COMPARE_CSV_HEADER,
} from './compareExport';

const sampleResult = (): CompareResult => compareEntries(
    [
        { name: 'only-left.txt', isDir: false, size: 10, mtimeMs: 1_000 },
        { name: 'shared.txt', isDir: false, size: 200, mtimeMs: 50_000 },
        { name: 'sub', isDir: true, size: 0, mtimeMs: 0 },
    ],
    [
        { name: 'shared.txt', isDir: false, size: 100, mtimeMs: 10_000 },
        { name: 'only-right.txt', isDir: false, size: 30, mtimeMs: 2_000 },
    ],
);

describe('compareExport — buildCompareExportRows', () => {
    it('flattens every compare entry into a row', () => {
        const rows = buildCompareExportRows(sampleResult());
        expect(rows).toHaveLength(4); // only-left, shared, sub, only-right
        const byPath = new Map(rows.map((r) => [r.path, r]));
        expect(byPath.get('only-left.txt')?.status).toBe('only-left');
        expect(byPath.get('only-right.txt')?.status).toBe('only-right');
        expect(byPath.get('sub')?.is_dir).toBe(true);
    });

    it('carries left/right sizes and ISO mtimes', () => {
        const rows = buildCompareExportRows(sampleResult());
        const shared = rows.find((r) => r.path === 'shared.txt');
        expect(shared?.left_size).toBe(200);
        expect(shared?.right_size).toBe(100);
        expect(shared?.left_modified).toBe(new Date(50_000).toISOString());
        expect(shared?.right_modified).toBe(new Date(10_000).toISOString());
    });

    it('emits null modified for a zero / missing mtime', () => {
        const rows = buildCompareExportRows(sampleResult());
        const dir = rows.find((r) => r.path === 'sub');
        expect(dir?.left_modified).toBeNull();
    });

    it('uses the recursive relativePath when present', () => {
        const nested: CompareResultEntry = {
            name: 'deep.txt',
            relativePath: 'a/b/deep.txt',
            bucket: 'only-left',
            leftIsDir: false,
            leftSize: 5,
            leftMtimeMs: 1_000,
        };
        const result: CompareResult = {
            entries: [nested],
            buckets: {
                'only-left': [nested], 'only-right': [], 'newer-left': [],
                'newer-right': [], same: [], conflict: [],
            },
            stats: {
                'only-left': { count: 1, bytes: 5 }, 'only-right': { count: 0, bytes: 0 },
                'newer-left': { count: 0, bytes: 0 }, 'newer-right': { count: 0, bytes: 0 },
                same: { count: 0, bytes: 0 }, conflict: { count: 0, bytes: 0 },
            },
            totals: { count: 1, bytes: 5 },
            appliedOptions: {
                policy: 'size-and-mtime', mtimeToleranceMs: 2_000,
                skipDirectoryComparison: false,
            },
        };
        expect(buildCompareExportRows(result)[0].path).toBe('a/b/deep.txt');
    });
});

describe('compareExport — serialisers', () => {
    it('compareRowsToJson round-trips through JSON.parse', () => {
        const rows = buildCompareExportRows(sampleResult());
        const parsed = JSON.parse(compareRowsToJson(rows));
        expect(parsed).toEqual(rows);
    });

    it('compareRowsToCsv emits a header plus one line per row', () => {
        const rows = buildCompareExportRows(sampleResult());
        const lines = compareRowsToCsv(rows).split('\n');
        expect(lines[0]).toBe(COMPARE_CSV_HEADER);
        expect(lines).toHaveLength(rows.length + 1);
    });

    it('compareRowsToCsv quotes and escapes the path cell', () => {
        const tricky: CompareResultEntry = {
            name: 'q.txt',
            relativePath: 'a,b/"quote".txt',
            bucket: 'only-left',
            leftSize: 1,
            leftMtimeMs: 1_000,
        };
        const csv = compareRowsToCsv(buildCompareExportRows({
            entries: [tricky],
            buckets: {
                'only-left': [tricky], 'only-right': [], 'newer-left': [],
                'newer-right': [], same: [], conflict: [],
            },
            stats: {
                'only-left': { count: 1, bytes: 1 }, 'only-right': { count: 0, bytes: 0 },
                'newer-left': { count: 0, bytes: 0 }, 'newer-right': { count: 0, bytes: 0 },
                same: { count: 0, bytes: 0 }, conflict: { count: 0, bytes: 0 },
            },
            totals: { count: 1, bytes: 1 },
            appliedOptions: {
                policy: 'size-and-mtime', mtimeToleranceMs: 2_000,
                skipDirectoryComparison: false,
            },
        }));
        expect(csv).toContain('"a,b/""quote"".txt"');
    });

    it('compareExportFilename stamps the extension and a sortable timestamp', () => {
        const name = compareExportFilename('json', new Date('2026-05-22T14:30:12.000Z'));
        expect(name).toBe('aerosync-dryrun-2026-05-22T14-30-12.json');
        expect(compareExportFilename('csv', new Date('2026-05-22T14:30:12.000Z')))
            .toMatch(/\.csv$/);
    });
});
