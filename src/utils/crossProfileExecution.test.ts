// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Tests for the cross-profile execution helper (Z.3.5).
// Covers the path joiner, the entry list builder and the sequential
// plan/execute driver including cancel + failure isolation.

import { describe, expect, it, vi, beforeEach } from 'vitest';

const mockInvoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
    invoke: (cmd: string, args?: unknown) => mockInvoke(cmd, args),
}));

import {
    buildCrossProfileEntries,
    joinRemotePath,
    runCrossProfileTransfer,
} from './crossProfileExecution';

describe('joinRemotePath', () => {
    it('joins simple posix paths without doubling slashes', () => {
        expect(joinRemotePath('/home/data', 'file.bin')).toBe('/home/data/file.bin');
    });

    it('preserves leading slash on a root path', () => {
        expect(joinRemotePath('/', 'file.bin')).toBe('/file.bin');
    });

    it('strips trailing slash on the directory', () => {
        expect(joinRemotePath('/home/data/', 'sub/file.bin')).toBe('/home/data/sub/file.bin');
    });

    it('rewrites Windows-style backslashes', () => {
        expect(joinRemotePath('C:\\Users\\axp', 'file.bin')).toBe('C:/Users/axp/file.bin');
    });

    it('drops a leading slash on the entry name to avoid double-slash', () => {
        expect(joinRemotePath('/backup', '/keep.tar.gz')).toBe('/backup/keep.tar.gz');
    });

    it('treats an empty dir as root', () => {
        expect(joinRemotePath('', 'file.bin')).toBe('/file.bin');
    });
});

describe('buildCrossProfileEntries', () => {
    it('returns [] for empty selections', () => {
        expect(
            buildCrossProfileEntries({
                sourceDir: '/src',
                destDir: '/dst',
                selections: [],
                skipExisting: false,
            }),
        ).toEqual([]);
    });

    it('maps file selections without recursion', () => {
        const entries = buildCrossProfileEntries({
            sourceDir: '/src',
            destDir: '/dst',
            selections: [
                { name: 'a.txt', isDir: false },
                { name: 'b.txt', isDir: false },
            ],
            skipExisting: true,
        });
        expect(entries).toEqual([
            { sourcePath: '/src/a.txt', destPath: '/dst/a.txt', recursive: false, skipExisting: true },
            { sourcePath: '/src/b.txt', destPath: '/dst/b.txt', recursive: false, skipExisting: true },
        ]);
    });

    it('flags directory selections as recursive', () => {
        const entries = buildCrossProfileEntries({
            sourceDir: '/src',
            destDir: '/dst',
            selections: [
                { name: 'sub', isDir: true },
                { name: 'file.bin', isDir: false },
            ],
            skipExisting: false,
        });
        expect(entries[0]).toEqual({
            sourcePath: '/src/sub',
            destPath: '/dst/sub',
            recursive: true,
            skipExisting: false,
        });
        expect(entries[1].recursive).toBe(false);
    });

    it('accepts the snake_case is_dir spelling from RemoteFile', () => {
        const entries = buildCrossProfileEntries({
            sourceDir: '/src',
            destDir: '/dst',
            selections: [{ name: 'sub', is_dir: true }],
            skipExisting: false,
        });
        expect(entries[0].recursive).toBe(true);
    });

    it('skips entries with missing or non-string names', () => {
        const entries = buildCrossProfileEntries({
            sourceDir: '/src',
            destDir: '/dst',
            // @ts-expect-error -- intentionally malformed input
            selections: [{ name: '' }, { name: null }, { name: 'ok.txt' }],
            skipExisting: false,
        });
        expect(entries.map((entry) => entry.sourcePath)).toEqual(['/src/ok.txt']);
    });
});

const buildPlanResponse = (planId: string) => ({
    plan_id: planId,
    source_profile_id: 'src-id',
    dest_profile_id: 'dst-id',
    source_profile: 'src-name',
    dest_profile: 'dst-name',
    entries: [],
    total_files: 0,
    total_bytes: 0,
});

const buildExecuteResponse = (overrides: Partial<{ transferred_files: number; total_bytes: number }> = {}) => ({
    transfer_id: 'transfer-id',
    planned_files: 1,
    transferred_files: overrides.transferred_files ?? 1,
    skipped_files: 0,
    failed_files: 0,
    total_bytes: overrides.total_bytes ?? 0,
    duration_ms: 0,
});

describe('runCrossProfileTransfer', () => {
    beforeEach(() => {
        mockInvoke.mockReset();
    });

    it('plan-and-executes every entry sequentially', async () => {
        mockInvoke
            .mockResolvedValueOnce(buildPlanResponse('plan-1'))
            .mockResolvedValueOnce(buildExecuteResponse({ transferred_files: 3, total_bytes: 100 }))
            .mockResolvedValueOnce(buildPlanResponse('plan-2'))
            .mockResolvedValueOnce(buildExecuteResponse({ transferred_files: 1, total_bytes: 25 }));

        const summary = await runCrossProfileTransfer('src-id', 'dst-id', [
            { sourcePath: '/src/a', destPath: '/dst/a', recursive: false, skipExisting: false },
            { sourcePath: '/src/b', destPath: '/dst/b', recursive: false, skipExisting: false },
        ]);

        expect(summary.succeeded).toBe(2);
        expect(summary.failed).toBe(0);
        expect(summary.cancelled).toBe(0);
        expect(summary.transferredFiles).toBe(4);
        expect(summary.transferredBytes).toBe(125);
        expect(mockInvoke).toHaveBeenNthCalledWith(1, 'cross_profile_plan', expect.anything());
        expect(mockInvoke).toHaveBeenNthCalledWith(2, 'cross_profile_execute', { request: { plan_id: 'plan-1' } });
        expect(mockInvoke).toHaveBeenNthCalledWith(4, 'cross_profile_execute', { request: { plan_id: 'plan-2' } });
    });

    it('records failures and continues with the next entry', async () => {
        mockInvoke
            .mockRejectedValueOnce(new Error('plan blew up'))
            .mockResolvedValueOnce(buildPlanResponse('plan-2'))
            .mockResolvedValueOnce(buildExecuteResponse({ transferred_files: 1 }));

        const errors: Array<{ index: number; message: string }> = [];
        const summary = await runCrossProfileTransfer(
            'src-id',
            'dst-id',
            [
                { sourcePath: '/src/a', destPath: '/dst/a', recursive: false, skipExisting: false },
                { sourcePath: '/src/b', destPath: '/dst/b', recursive: false, skipExisting: false },
            ],
            {
                onEntryError: (_entry, message, index) => errors.push({ index, message }),
            },
        );

        expect(summary.succeeded).toBe(1);
        expect(summary.failed).toBe(1);
        expect(summary.failures).toHaveLength(1);
        expect(summary.failures[0]?.entry.sourcePath).toBe('/src/a');
        expect(summary.failures[0]?.error).toContain('plan blew up');
        expect(errors).toEqual([{ index: 0, message: expect.stringContaining('plan blew up') as unknown as string }]);
    });

    it('aborts remaining entries when shouldCancel returns true', async () => {
        mockInvoke
            .mockResolvedValueOnce(buildPlanResponse('plan-1'))
            .mockResolvedValueOnce(buildExecuteResponse({ transferred_files: 1 }));

        let entriesProcessed = 0;
        const summary = await runCrossProfileTransfer(
            'src-id',
            'dst-id',
            [
                { sourcePath: '/src/a', destPath: '/dst/a', recursive: false, skipExisting: false },
                { sourcePath: '/src/b', destPath: '/dst/b', recursive: false, skipExisting: false },
                { sourcePath: '/src/c', destPath: '/dst/c', recursive: false, skipExisting: false },
            ],
            {
                onEntryDone: () => {
                    entriesProcessed += 1;
                },
                shouldCancel: () => entriesProcessed >= 1,
            },
        );

        expect(summary.succeeded).toBe(1);
        expect(summary.cancelled).toBe(2);
        // Two invokes total: plan + execute for the first entry only.
        expect(mockInvoke).toHaveBeenCalledTimes(2);
    });

    it('returns an empty aggregate when there are no entries', async () => {
        const summary = await runCrossProfileTransfer('src-id', 'dst-id', []);
        expect(summary.succeeded).toBe(0);
        expect(summary.failed).toBe(0);
        expect(summary.cancelled).toBe(0);
        expect(mockInvoke).not.toHaveBeenCalled();
    });
});
