// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// GAP-5 of the AeroSync residual-gap closure filone.
//
// `adaptFileComparisons` bridges the recursive backend tree scan
// (`compare_directories` / `provider_compare_directories`, which return a
// `FileComparison[]` carrying `/`-separated `relative_path` values) into the
// flat `CompareResult` shape the unified Compare/Plan tabs already consume.
//
// The unified Compare tab used to run the pure, flat `compareEntries()` over
// the already-loaded top-level listings, so a connected-remote preset against
// a remote with subdirectories only ever acted on the top level. This adapter
// lets the same Compare/Plan/runner pipeline operate on a full recursive tree:
// every `CompareResultEntry` it produces carries `relativePath`, and
// `presetToSyncRun` + `remoteSyncRunner` already honour nested paths.

import type { FileComparison, FileInfo, SyncStatus } from '../types';
import type {
    CompareBucket,
    CompareBucketStats,
    CompareResult,
    CompareResultEntry,
} from './compareEndpoints';

const BUCKETS: CompareBucket[] = [
    'only-left',
    'only-right',
    'newer-left',
    'newer-right',
    'same',
    'conflict',
];

const emptyStats = (): CompareBucketStats => ({ count: 0, bytes: 0 });

const basename = (relativePath: string): string => {
    const parts = relativePath.split('/').filter(Boolean);
    return parts.length > 0 ? parts[parts.length - 1] : relativePath;
};

const parseMtime = (modified: string | null | undefined): number | null => {
    if (!modified) return null;
    const ms = Date.parse(modified);
    return Number.isFinite(ms) ? ms : null;
};

const sizeOf = (info: FileInfo | null | undefined): number => {
    if (!info || info.is_dir) return 0;
    return typeof info.size === 'number' && Number.isFinite(info.size)
        ? Math.max(0, info.size)
        : 0;
};

/**
 * Map a backend `SyncStatus` onto a Compare bucket. The mapping depends on
 * which visual side holds the local panel: `local_*` statuses become
 * `*-left` when the left panel is local, `*-right` otherwise.
 *
 * `size_mismatch` (same timestamp, diverging size) collapses to `conflict`,
 * matching the flat classifier's treatment of an ambiguous pair.
 */
const statusToBucket = (
    status: SyncStatus,
    leftIsLocal: boolean,
): CompareBucket | null => {
    switch (status) {
        case 'identical':
            return 'same';
        case 'conflict':
        case 'size_mismatch':
            return 'conflict';
        case 'local_only':
            return leftIsLocal ? 'only-left' : 'only-right';
        case 'remote_only':
            return leftIsLocal ? 'only-right' : 'only-left';
        case 'local_newer':
            return leftIsLocal ? 'newer-left' : 'newer-right';
        case 'remote_newer':
            return leftIsLocal ? 'newer-right' : 'newer-left';
        default:
            return null;
    }
};

/**
 * Translate a recursive `FileComparison[]` into the flat `CompareResult`
 * structure consumed by `CompareTabContent`, `derivePresetPlan` and
 * `buildRemoteSyncInput`.
 *
 * @param comparisons  Raw rows from `compare_directories`.
 * @param leftIsLocal  true for the `local-remote` pair kind (left panel is
 *                     local), false for `remote-local`.
 *
 * Directory rows are kept only when one side is missing entirely
 * (`only-left` / `only-right`) so empty directories still get created; a
 * directory present on both sides carries no meaningful size/mtime delta and
 * is dropped, mirroring the legacy `SyncPanel.handleCompare` filter.
 */
export const adaptFileComparisons = (
    comparisons: FileComparison[],
    leftIsLocal: boolean,
): CompareResult => {
    const buckets: Record<CompareBucket, CompareResultEntry[]> = {
        'only-left': [],
        'only-right': [],
        'newer-left': [],
        'newer-right': [],
        same: [],
        conflict: [],
    };
    const stats: Record<CompareBucket, CompareBucketStats> = {
        'only-left': emptyStats(),
        'only-right': emptyStats(),
        'newer-left': emptyStats(),
        'newer-right': emptyStats(),
        same: emptyStats(),
        conflict: emptyStats(),
    };

    for (const fc of comparisons) {
        if (!fc || typeof fc.relative_path !== 'string' || fc.relative_path === '') {
            continue;
        }
        const bucket = statusToBucket(fc.status, leftIsLocal);
        if (!bucket) continue;

        // Drop directory rows that are present on both sides: their size and
        // mtime are filesystem block metadata, not content, so any compare
        // verdict on them is noise. Keep only genuinely new directories.
        if (fc.is_dir && bucket !== 'only-left' && bucket !== 'only-right') {
            continue;
        }

        const leftInfo = leftIsLocal ? fc.local_info : fc.remote_info;
        const rightInfo = leftIsLocal ? fc.remote_info : fc.local_info;

        const row: CompareResultEntry = {
            name: basename(fc.relative_path),
            relativePath: fc.relative_path,
            bucket,
            leftIsDir: leftInfo?.is_dir ?? fc.is_dir,
            rightIsDir: rightInfo?.is_dir ?? fc.is_dir,
            leftSize: leftInfo ? leftInfo.size : null,
            rightSize: rightInfo ? rightInfo.size : null,
            leftMtimeMs: parseMtime(leftInfo?.modified),
            rightMtimeMs: parseMtime(rightInfo?.modified),
            leftChecksum: leftInfo?.checksum ?? null,
            leftChecksumAlg: leftInfo?.checksum_alg ?? null,
            rightChecksum: rightInfo?.checksum ?? null,
            rightChecksumAlg: rightInfo?.checksum_alg ?? null,
        };

        // Bytes at stake: the larger side, mirroring `compareEndpoints`.
        const bytes = Math.max(sizeOf(leftInfo), sizeOf(rightInfo));
        buckets[bucket].push(row);
        stats[bucket].count += 1;
        stats[bucket].bytes += bytes;
    }

    // Stable order inside each bucket by full relative path so nested files
    // group under their directory in the UI.
    for (const bucket of BUCKETS) {
        buckets[bucket].sort((a, b) =>
            (a.relativePath ?? a.name).localeCompare(b.relativePath ?? b.name),
        );
    }

    const entries: CompareResultEntry[] = BUCKETS.flatMap((bucket) => buckets[bucket]);
    const totals = BUCKETS.reduce(
        (acc, bucket) => ({
            count: acc.count + stats[bucket].count,
            bytes: acc.bytes + stats[bucket].bytes,
        }),
        { count: 0, bytes: 0 },
    );

    return {
        entries,
        buckets,
        stats,
        totals,
        // The recursive scan already classified every entry server-side;
        // the echoed options exist only for the Compare tab's policy label.
        appliedOptions: {
            policy: 'size-and-mtime',
            mtimeToleranceMs: 2000,
            skipDirectoryComparison: true,
        },
    };
};
