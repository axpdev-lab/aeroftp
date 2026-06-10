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
}

/**
 * GAP-7: timestamp suffix for keep-both renames. `2026-05-22T14:30:12.123Z`
 * collapses to `20260522T143012` — the legacy `SyncPanel` convention.
 */
export const defaultRenameSuffix = (): string =>
    new Date().toISOString().replace(/[-:]/g, '').replace(/\.\d+Z$/, '');

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
 * The path the runner resolves against both roots. GAP-5: recursive
 * connected-remote compares carry a `/`-separated `relativePath`; the flat
 * classifier sets it equal to `name`, so the fallback keeps both cases sound.
 */
const relPath = (e: CompareResultEntry): string => e.relativePath ?? e.name;

const sourceSha256 = (
    entry: CompareResultEntry,
    sourceSide: 'left' | 'right',
): string | null => {
    const checksum = sourceSide === 'left' ? entry.leftChecksum : entry.rightChecksum;
    const alg = sourceSide === 'left' ? entry.leftChecksumAlg : entry.rightChecksumAlg;
    return checksum && alg?.toLowerCase() === 'sha256' ? checksum : null;
};

/**
 * Translate a resolved `PresetPlan` into the runner's input.
 *
 * @param plan         The plan derived from the connected-remote compare.
 * @param leftIsLocal  true when the compare's left side is the local panel
 *                     (pairKind `local-remote`); false for `remote-local`.
 * @param renameSuffix Timestamp suffix used for keep-both renames; injected
 *                     so tests stay deterministic.
 */
export const buildRemoteSyncInput = (
    plan: PresetPlan,
    leftIsLocal: boolean,
    renameSuffix: string = defaultRenameSuffix(),
): RemoteSyncInput => {
    const files: SyncRunFile[] = [];
    const dirs: SyncRunDirs = { remote: [], local: [] };

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
            // A directory entry is created (recursively, via the runner's
            // parent-dir pre-pass) on the destination; nested files surface
            // as their own rows in a recursive compare.
            (destIsRemote ? dirs.remote : dirs.local).push(relPath(entry));
            return;
        }
        files.push({
            relativePath: relPath(entry),
            action: destIsRemote ? 'upload' : 'download',
            size: meta.size,
            mtime: meta.mtime,
            overwritesExisting: overwrites,
            isDir: false,
            expectedSha256: destIsRemote ? null : sourceSha256(entry, sourceSide),
        });
    };

    const pushDelete = (
        entry: CompareResultEntry,
        targetSide: 'left' | 'right',
        targetIsRemote: boolean,
    ): void => {
        const meta = targetSide === 'left' ? leftMeta(entry) : rightMeta(entry);
        files.push({
            relativePath: relPath(entry),
            action: targetIsRemote ? 'delete-remote' : 'delete-local',
            size: meta.size,
            mtime: null,
            isDir: meta.isDir,
        });
    };

    // GAP-7: keep-both rename. Copy the source-side file to the destination
    // under a timestamped suffix (`file.txt.20260522T143012.bak`) so neither
    // diverging copy is lost. `sourcePath` carries the original path; the
    // runner reads the source and writes the suffixed destination.
    const pushRename = (
        entry: CompareResultEntry,
        sourceSide: 'left' | 'right',
        destIsRemote: boolean,
    ): void => {
        const meta = sourceSide === 'left' ? leftMeta(entry) : rightMeta(entry);
        if (meta.isDir) return; // keep-both applies to files only
        const src = relPath(entry);
        files.push({
            relativePath: `${src}.${renameSuffix}.bak`,
            sourcePath: src,
            action: destIsRemote ? 'upload' : 'download',
            size: meta.size,
            mtime: meta.mtime,
            overwritesExisting: false,
            isDir: false,
            expectedSha256: destIsRemote ? null : sourceSha256(entry, sourceSide),
        });
    };

    const isDelete = (f: SyncRunFile): boolean =>
        f.action === 'delete-remote' || f.action === 'delete-local';

    for (const bucket of plan.bucketPlans) {
        bucket.entries.forEach((entry, idx) => {
            const action: BucketAction = bucket.entryActions[idx] ?? bucket.action;
            switch (action) {
                case 'skip':
                case 'conflict-skip':
                    return;
                case 'rename-to-right':
                    pushRename(entry, 'left', rightIsRemote);
                    return;
                case 'rename-to-left':
                    pushRename(entry, 'right', leftIsRemote);
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

    // GAP-10 — deletes must run deepest-first. A recursive compare flags a
    // pruned directory and every descendant inside it; if the directory is
    // deleted before its children the child deletes strand as `NotFound`
    // (the local `delete_local_file` and a recursive provider delete both
    // remove the subtree). Sorting the delete entries by descending path
    // depth — and moving them after the copies — guarantees a directory is
    // only ever removed once it is already empty. The sort is stable, so
    // entries at the same depth keep their bucket order.
    const depthOf = (p: string): number => p.split('/').filter(Boolean).length;
    const copies = files.filter((f) => !isDelete(f));
    const deletes = files
        .filter(isDelete)
        .sort((a, b) => depthOf(b.relativePath) - depthOf(a.relativePath));

    return { files: [...copies, ...deletes], dirs };
};

/**
 * GAP-5 — translate a Compare-tab mirror selection into the runner's input.
 *
 * The Compare tab's "Mirror left to right" / "Mirror right to left" buttons
 * hand over a flat list of `only-*` + `newer-*` entries. This builds a
 * copy-only `RemoteSyncInput` (no deletes) so a connected-remote mirror runs
 * through the same recursive-capable `runRemoteSync` engine as the Plan tab,
 * instead of the flat top-level transfer planner.
 *
 * @param entries     The `only-<side>` + `newer-<side>` rows to copy.
 * @param sourceSide  Which compare side is the copy source.
 * @param leftIsLocal true for the `local-remote` pair kind.
 */
export const buildMirrorSyncInput = (
    entries: CompareResultEntry[],
    sourceSide: 'left' | 'right',
    leftIsLocal: boolean,
): RemoteSyncInput => {
    const files: SyncRunFile[] = [];
    const dirs: SyncRunDirs = { remote: [], local: [] };

    // Right is remote iff left is local. The destination is the side
    // opposite the source.
    const destIsRemote = sourceSide === 'left' ? leftIsLocal : !leftIsLocal;

    for (const entry of entries) {
        const meta = sourceSide === 'left' ? leftMeta(entry) : rightMeta(entry);
        const path = relPath(entry);
        if (meta.isDir) {
            (destIsRemote ? dirs.remote : dirs.local).push(path);
            continue;
        }
        files.push({
            relativePath: path,
            action: destIsRemote ? 'upload' : 'download',
            size: meta.size,
            mtime: meta.mtime,
            // `newer-*` rows overwrite an existing destination copy.
            overwritesExisting: entry.bucket === 'newer-left' || entry.bucket === 'newer-right',
            isDir: false,
        });
    }

    return { files, dirs };
};
