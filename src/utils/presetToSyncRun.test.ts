// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { buildRemoteSyncInput } from './presetToSyncRun';
import type { CompareResultEntry } from './compareEndpoints';
import type { BucketAction, BucketPlan, PresetPlan } from './syncPresets';

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

    it('counts keep-both renames as deferred and never queues them', () => {
        const plan = planOf([
            { entry: entry('k1.txt'), action: 'rename-to-right' },
            { entry: entry('k2.txt'), action: 'rename-to-left' },
            { entry: entry('real.txt'), action: 'copy-to-right' },
        ]);
        const { files, deferredRenames } = buildRemoteSyncInput(plan, true);
        expect(deferredRenames).toBe(2);
        expect(files).toHaveLength(1);
        expect(files[0].relativePath).toBe('real.txt');
    });

    it('drops skip and conflict-skip entries entirely', () => {
        const plan = planOf([
            { entry: entry('s.txt'), action: 'skip' },
            { entry: entry('c.txt'), action: 'conflict-skip' },
        ]);
        const { files, dirs, deferredRenames } = buildRemoteSyncInput(plan, true);
        expect(files).toHaveLength(0);
        expect(dirs.remote).toEqual([]);
        expect(dirs.local).toEqual([]);
        expect(deferredRenames).toBe(0);
    });
});
