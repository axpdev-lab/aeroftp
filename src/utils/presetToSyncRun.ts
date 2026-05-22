// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// GAP-3 of the AeroSync connected-remote gap-closure filone.
//
// `buildRemoteSyncInput` translates a FreeFileSync-style `PresetPlan` (the
// Plan tab's per-bucket preview) into the `SyncRunFile[]` / `SyncRunDirs`
// shape the `remoteSyncRunner` executes. It is the bridge that lets the
// unified Plan tab run a real connected-remote sync — deletes and overwrites
// included — instead of dispatching copy legs only.

import type { CompareResultEntry } from './compareEndpoints';
import type { BucketAction, PresetPlan } from './syncPresets';
import type { SyncRunDirs, SyncRunFile } from './remoteSyncRunner';

export interface RemoteSyncInput {
    files: SyncRunFile[];
    dirs: SyncRunDirs;
    /**
     * Count of `keep-both` rename actions, which the runner does not execute
     * (no rename primitive). The caller surfaces them as deferred.
     */
    deferredRenames: number;
}

const toIso = (ms: number | null | undefined): string | null =>
    typeof ms === 'number' && Number.isFinite(ms) ? new Date(ms).toISOString() : null;

interface SideMeta {
    size: number;
    mtime: string | null;
    isDir: boolean;
}

const leftMeta = (e: CompareResultEntry): SideMeta => ({
    size: e.leftSize ?? 0,
    mtime: toIso(e.leftMtimeMs),
    isDir: e.leftIsDir === true,
});

const rightMeta = (e: CompareResultEntry): SideMeta => ({
    size: e.rightSize ?? 0,
    mtime: toIso(e.rightMtimeMs),
    isDir: e.rightIsDir === true,
});

/**
 * Translate a resolved `PresetPlan` into the runner's input.
 *
 * @param plan         The plan derived from the connected-remote compare.
 * @param leftIsLocal  true when the compare's left side is the local panel
 *                     (pairKind `local-remote`); false for `remote-local`.
 */
export const buildRemoteSyncInput = (
    plan: PresetPlan,
    leftIsLocal: boolean,
): RemoteSyncInput => {
    const files: SyncRunFile[] = [];
    const dirs: SyncRunDirs = { remote: [], local: [] };
    let deferredRenames = 0;

    // Right is remote iff left is local; left is remote otherwise.
    const rightIsRemote = leftIsLocal;
    const leftIsRemote = !leftIsLocal;

    const pushCopy = (
        entry: CompareResultEntry,
        sourceSide: 'left' | 'right',
        destIsRemote: boolean,
        overwrites: boolean,
    ): void => {
        const meta = sourceSide === 'left' ? leftMeta(entry) : rightMeta(entry);
        if (meta.isDir) {
            // Flat compare: a directory entry is created empty on the
            // destination; its contents are out of scope for this pass.
            (destIsRemote ? dirs.remote : dirs.local).push(entry.name);
            return;
        }
        files.push({
            relativePath: entry.name,
            action: destIsRemote ? 'upload' : 'download',
            size: meta.size,
            mtime: meta.mtime,
            overwritesExisting: overwrites,
            isDir: false,
        });
    };

    const pushDelete = (
        entry: CompareResultEntry,
        targetSide: 'left' | 'right',
        targetIsRemote: boolean,
    ): void => {
        const meta = targetSide === 'left' ? leftMeta(entry) : rightMeta(entry);
        files.push({
            relativePath: entry.name,
            action: targetIsRemote ? 'delete-remote' : 'delete-local',
            size: meta.size,
            mtime: null,
            isDir: meta.isDir,
        });
    };

    for (const bucket of plan.bucketPlans) {
        bucket.entries.forEach((entry, idx) => {
            const action: BucketAction = bucket.entryActions[idx] ?? bucket.action;
            switch (action) {
                case 'skip':
                case 'conflict-skip':
                    return;
                case 'rename-to-right':
                case 'rename-to-left':
                    deferredRenames += 1;
                    return;
                case 'copy-to-right':
                    pushCopy(entry, 'left', rightIsRemote, false);
                    return;
                case 'overwrite-right':
                    pushCopy(entry, 'left', rightIsRemote, true);
                    return;
                case 'copy-to-left':
                    pushCopy(entry, 'right', leftIsRemote, false);
                    return;
                case 'overwrite-left':
                    pushCopy(entry, 'right', leftIsRemote, true);
                    return;
                case 'delete-right':
                    pushDelete(entry, 'right', rightIsRemote);
                    return;
                case 'delete-left':
                    pushDelete(entry, 'left', leftIsRemote);
                    return;
                default:
                    return;
            }
        });
    }

    return { files, dirs, deferredRenames };
};
