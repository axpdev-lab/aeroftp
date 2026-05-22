// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { buildRemoteSyncInput, buildMirrorSyncInput } from './presetToSyncRun';
import { adaptFileComparisons } from './recursiveCompare';
import { derivePresetPlan } from './syncPresets';
import type { CompareResultEntry } from './compareEndpoints';
import type { BucketAction, BucketPlan, PresetPlan } from './syncPresets';
import type { FileComparison, FileInfo } from '../types';

const entry = (
    name: string,
    over: Partial<CompareResultEntry> = {},
): CompareResultEntry => ({
    name,
    bucket: 'same',
    ...over,
});

/** Build a PresetPlan carrying only the fields `buildRemoteSyncInput` reads. */
const planOf = (
    rows: Array<{ entry: CompareResultEntry; action: BucketAction }>,
): PresetPlan => {
    const bucket: BucketPlan = {
        bucket: 'only-left',
        action: rows[0]?.action ?? 'skip',
        entries: rows.map((r) => r.entry),
        entryActions: rows.map((r) => r.action),
        destructive: false,
        transferBytes: 0,
        versionedBackupBytes: 0,
        requiresVersionedBackup: false,
    };
    return {
        preset: 'mirror',
        direction: 'left-to-right',
        bucketPlans: [bucket],
    } as unknown as PresetPlan;
};

describe('buildRemoteSyncInput — direction mapping', () => {
    it('maps copy actions to upload/download for a local-left pair', () => {
        const plan = planOf([
            { entry: entry('up.txt', { leftSize: 10 }), action: 'copy-to-right' },
            { entry: entry('down.txt', { rightSize: 20 }), action: 'copy-to-left' },
        ]);
        const { files } = buildRemoteSyncInput(plan, true);
        expect(files).toHaveLength(2);
        expect(files[0]).toMatchObject({ relativePath: 'up.txt', action: 'upload', size: 10 });
        expect(files[1]).toMatchObject({ relativePath: 'down.txt', action: 'download', size: 20 });
    });

    it('inverts upload/download for a remote-left pair', () => {
        const plan = planOf([
            { entry: entry('a.txt'), action: 'copy-to-right' },
            { entry: entry('b.txt'), action: 'copy-to-left' },
        ]);
        const { files } = buildRemoteSyncInput(plan, false);
        // left is remote → copy-to-right lands on the local panel.
        expect(files[0]).toMatchObject({ relativePath: 'a.txt', action: 'download' });
        expect(files[1]).toMatchObject({ relativePath: 'b.txt', action: 'upload' });
    });
});

describe('buildRemoteSyncInput — deletes', () => {
    it('routes delete-right/left to the correct side', () => {
        const plan = planOf([
            { entry: entry('r.txt'), action: 'delete-right' },
            { entry: entry('l.txt'), action: 'delete-left' },
        ]);
        const { files } = buildRemoteSyncInput(plan, true);
        expect(files[0]).toMatchObject({ relativePath: 'r.txt', action: 'delete-remote' });
        expect(files[1]).toMatchObject({ relativePath: 'l.txt', action: 'delete-local' });
    });

    it('flags directory deletes with isDir', () => {
        const plan = planOf([
            { entry: entry('stale-dir', { rightIsDir: true }), action: 'delete-right' },
        ]);
        const { files } = buildRemoteSyncInput(plan, true);
        expect(files[0]).toMatchObject({ action: 'delete-remote', isDir: true });
    });
});

describe('buildRemoteSyncInput — overwrites and metadata', () => {
    it('marks overwrite actions with overwritesExisting', () => {
        const plan = planOf([
            { entry: entry('o.txt'), action: 'overwrite-right' },
            { entry: entry('c.txt'), action: 'copy-to-right' },
        ]);
        const { files } = buildRemoteSyncInput(plan, true);
        expect(files[0].overwritesExisting).toBe(true);
        expect(files[1].overwritesExisting).toBe(false);
    });

    it('converts the source-side mtime to an ISO string', () => {
        const ms = Date.UTC(2026, 4, 22, 10, 0, 0);
        const plan = planOf([
            { entry: entry('m.txt', { leftMtimeMs: ms }), action: 'copy-to-right' },
        ]);
        const { files } = buildRemoteSyncInput(plan, true);
        expect(files[0].mtime).toBe(new Date(ms).toISOString());
    });
});

describe('buildRemoteSyncInput — directories and skips', () => {
    it('routes a directory copy into dirs, not files', () => {
        const plan = planOf([
            { entry: entry('newfolder', { leftIsDir: true }), action: 'copy-to-right' },
        ]);
        const { files, dirs } = buildRemoteSyncInput(plan, true);
        expect(files).toHaveLength(0);
        expect(dirs.remote).toEqual(['newfolder']);
        expect(dirs.local).toEqual([]);
    });

    it('drops skip and conflict-skip entries entirely', () => {
        const plan = planOf([
            { entry: entry('s.txt'), action: 'skip' },
            { entry: entry('c.txt'), action: 'conflict-skip' },
        ]);
        const { files, dirs } = buildRemoteSyncInput(plan, true);
        expect(files).toHaveLength(0);
        expect(dirs.remote).toEqual([]);
        expect(dirs.local).toEqual([]);
    });
});

describe('buildRemoteSyncInput — GAP-7 keep-both rename execution', () => {
    it('resolves rename-to-right into a suffixed upload reading the source', () => {
        const plan = planOf([
            { entry: entry('report.txt', { leftSize: 12 }), action: 'rename-to-right' },
        ]);
        const { files } = buildRemoteSyncInput(plan, true, '20260522T143012');
        expect(files).toHaveLength(1);
        expect(files[0]).toMatchObject({
            relativePath: 'report.txt.20260522T143012.bak',
            sourcePath: 'report.txt',
            action: 'upload',
            overwritesExisting: false,
            size: 12,
        });
    });

    it('resolves rename-to-left into a suffixed download', () => {
        const plan = planOf([
            { entry: entry('notes.md', { rightSize: 5 }), action: 'rename-to-left' },
        ]);
        const { files } = buildRemoteSyncInput(plan, true, '20260522T143012');
        expect(files[0]).toMatchObject({
            relativePath: 'notes.md.20260522T143012.bak',
            sourcePath: 'notes.md',
            action: 'download',
        });
    });

    it('preserves a nested path in the rename source and destination', () => {
        const plan = planOf([
            {
                entry: entry('c.txt', { relativePath: 'a/b/c.txt', leftSize: 1 }),
                action: 'rename-to-right',
            },
        ]);
        const { files } = buildRemoteSyncInput(plan, true, 'TS');
        expect(files[0]).toMatchObject({
            sourcePath: 'a/b/c.txt',
            relativePath: 'a/b/c.txt.TS.bak',
        });
    });
});

describe('buildRemoteSyncInput — GAP-5 recursive paths', () => {
    it('carries a nested relativePath into the runner file', () => {
        const plan = planOf([
            {
                entry: entry('a.txt', { relativePath: 'docs/reports/a.txt', leftSize: 5 }),
                action: 'copy-to-right',
            },
        ]);
        const { files } = buildRemoteSyncInput(plan, true);
        expect(files[0].relativePath).toBe('docs/reports/a.txt');
    });

    it('routes a nested directory copy into dirs with the full path', () => {
        const plan = planOf([
            {
                entry: entry('reports', { relativePath: 'docs/reports', leftIsDir: true }),
                action: 'copy-to-right',
            },
        ]);
        const { dirs } = buildRemoteSyncInput(plan, true);
        expect(dirs.remote).toEqual(['docs/reports']);
    });

    it('carries a nested relativePath into a delete entry', () => {
        const plan = planOf([
            {
                entry: entry('old.txt', { relativePath: 'archive/2025/old.txt' }),
                action: 'delete-right',
            },
        ]);
        const { files } = buildRemoteSyncInput(plan, true);
        expect(files[0]).toMatchObject({
            relativePath: 'archive/2025/old.txt',
            action: 'delete-remote',
        });
    });

    it('falls back to name when relativePath is absent (flat compare)', () => {
        const plan = planOf([
            { entry: entry('flat.txt', { leftSize: 1 }), action: 'copy-to-right' },
        ]);
        const { files } = buildRemoteSyncInput(plan, true);
        expect(files[0].relativePath).toBe('flat.txt');
    });
});

describe('buildRemoteSyncInput — GAP-10 delete ordering', () => {
    it('emits deletes deepest-first, after the copies', () => {
        const plan = planOf([
            { entry: entry('foo', { relativePath: 'foo', rightIsDir: true }), action: 'delete-right' },
            { entry: entry('a.txt', { relativePath: 'foo/a.txt' }), action: 'delete-right' },
            { entry: entry('b.txt', { relativePath: 'foo/bar/b.txt' }), action: 'delete-right' },
            { entry: entry('new.txt', { relativePath: 'new.txt', leftSize: 4 }), action: 'copy-to-right' },
        ]);
        const { files } = buildRemoteSyncInput(plan, true);
        const order = files.map((f) => `${f.action}:${f.relativePath}`);
        // The copy runs before any delete.
        expect(order[0]).toBe('upload:new.txt');
        // Deletes follow, deepest path first so a recursive parent delete
        // never strands its now-missing children as NotFound errors.
        expect(order.slice(1)).toEqual([
            'delete-remote:foo/bar/b.txt',
            'delete-remote:foo/a.txt',
            'delete-remote:foo',
        ]);
    });

    it('keeps bucket order for deletes at the same depth (stable sort)', () => {
        const plan = planOf([
            { entry: entry('z.txt', { relativePath: 'z.txt' }), action: 'delete-right' },
            { entry: entry('a.txt', { relativePath: 'a.txt' }), action: 'delete-left' },
        ]);
        const { files } = buildRemoteSyncInput(plan, true);
        expect(files.map((f) => f.relativePath)).toEqual(['z.txt', 'a.txt']);
    });
});

describe('buildMirrorSyncInput', () => {
    it('mirrors left-side entries to remote uploads for a local-left pair', () => {
        const entries: CompareResultEntry[] = [
            entry('a.txt', { bucket: 'only-left', relativePath: 'a.txt', leftSize: 3 }),
            entry('b.txt', { bucket: 'newer-left', relativePath: 'sub/b.txt', leftSize: 7 }),
        ];
        const { files } = buildMirrorSyncInput(entries, 'left', true);
        expect(files[0]).toMatchObject({ relativePath: 'a.txt', action: 'upload', overwritesExisting: false });
        expect(files[1]).toMatchObject({ relativePath: 'sub/b.txt', action: 'upload', overwritesExisting: true });
    });

    it('mirrors right-side entries to remote uploads for a remote-left pair', () => {
        const entries: CompareResultEntry[] = [
            entry('c.txt', { bucket: 'only-right', relativePath: 'c.txt', rightSize: 2 }),
        ];
        // sourceSide right, leftIsLocal false → destination is the (remote) left.
        const { files } = buildMirrorSyncInput(entries, 'right', false);
        expect(files[0]).toMatchObject({ relativePath: 'c.txt', action: 'upload' });
    });

    it('routes directory entries into dirs and never deletes', () => {
        const entries: CompareResultEntry[] = [
            entry('dir', { bucket: 'only-left', relativePath: 'nested/dir', leftIsDir: true }),
        ];
        const { files, dirs } = buildMirrorSyncInput(entries, 'left', true);
        expect(files).toHaveLength(0);
        expect(dirs.remote).toEqual(['nested/dir']);
        expect(files.every((f) => f.action !== 'delete-remote' && f.action !== 'delete-local')).toBe(true);
    });
});

describe('GAP-5 end-to-end — recursive compare survives the whole chain', () => {
    const finfo = (over: Partial<FileInfo> = {}): FileInfo => ({
        name: 'f', path: '/f', size: 100, modified: '2026-05-22T10:00:00Z',
        is_dir: false, checksum: null, ...over,
    });

    it('a 3-level nested file flows adapt → derivePresetPlan → buildRemoteSyncInput', () => {
        const comparisons: FileComparison[] = [
            {
                relative_path: 'a/b/c/deep.txt',
                status: 'local_only',
                local_info: finfo({ size: 42 }),
                remote_info: null,
                is_dir: false,
                sync_reason: '',
            },
        ];
        const compareResult = adaptFileComparisons(comparisons, true);
        // derivePresetPlan is depth-agnostic: it never reads relativePath.
        const plan = derivePresetPlan(compareResult, { preset: 'mirror', direction: 'left-to-right' });
        const { files } = buildRemoteSyncInput(plan, true);
        expect(files).toHaveLength(1);
        expect(files[0]).toMatchObject({
            relativePath: 'a/b/c/deep.txt',
            action: 'upload',
            size: 42,
        });
    });

    it('nested orphans on the remote resolve to delete-remote under mirror', () => {
        const comparisons: FileComparison[] = [
            {
                relative_path: 'logs/2025/stale.log',
                status: 'remote_only',
                local_info: null,
                remote_info: finfo({ size: 9 }),
                is_dir: false,
                sync_reason: '',
            },
        ];
        const compareResult = adaptFileComparisons(comparisons, true);
        const plan = derivePresetPlan(compareResult, { preset: 'mirror', direction: 'left-to-right' });
        const { files } = buildRemoteSyncInput(plan, true);
        expect(files[0]).toMatchObject({
            relativePath: 'logs/2025/stale.log',
            action: 'delete-remote',
        });
    });
});
