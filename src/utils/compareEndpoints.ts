// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

// Z.3.7 — Pure compare classifier for the dual-panel Compare view.
//
// Takes two flat directory listings (left + right) and bucketizes each
// distinct name into the six classic FreeFileSync-style outcomes:
//
//   only-left   — present in left, absent in right
//   only-right  — present in right, absent in left
//   newer-left  — present in both, left mtime newer than right
//   newer-right — present in both, right mtime newer than left
//   same        — present in both, considered equal per the active policy
//   conflict    — present in both but ambiguous (e.g. mtime within
//                 tolerance but sizes diverge, or one side is a directory
//                 and the other is a file)
//
// The classifier is intentionally synchronous, side-effect free and
// framework-agnostic so unit tests can pin every transition without
// mocking IPC. Directory recursion is out of scope for this slice (Z.3.7
// closure note covers the follow-up Z.3.7.2).

export type ComparePolicy =
    /** Two entries are "same" iff their sizes match (mtime ignored). */
    | 'size-only'
    /** Two entries are "same" iff sizes match AND mtime is within tolerance. */
    | 'size-and-mtime'
    /** Two entries are "same" iff mtime is within tolerance (size ignored). */
    | 'mtime-only';

export type CompareBucket =
    | 'only-left'
    | 'only-right'
    | 'newer-left'
    | 'newer-right'
    | 'same'
    | 'conflict';

export interface CompareInputEntry {
    name: string;
    isDir: boolean;
    size?: number | null;
    mtimeMs?: number | null;
}

export interface CompareResultEntry {
    name: string;
    bucket: CompareBucket;
    leftIsDir?: boolean;
    rightIsDir?: boolean;
    leftSize?: number | null;
    rightSize?: number | null;
    leftMtimeMs?: number | null;
    rightMtimeMs?: number | null;
}

export interface CompareOptions {
    /** Defaults to `'size-and-mtime'` (FreeFileSync default). */
    policy?: ComparePolicy;
    /** Allowed clock skew when comparing mtimes; defaults to 2000 ms. */
    mtimeToleranceMs?: number;
    /** When true, treat directory↔directory pairs as `same` regardless of size/mtime. */
    skipDirectoryComparison?: boolean;
}

export interface CompareBucketStats {
    count: number;
    bytes: number;
}

export interface CompareResult {
    entries: CompareResultEntry[];
    buckets: Record<CompareBucket, CompareResultEntry[]>;
    stats: Record<CompareBucket, CompareBucketStats>;
    totals: { count: number; bytes: number };
    /** Echo of the options actually applied (after defaults). */
    appliedOptions: Required<CompareOptions>;
}

const BUCKETS: CompareBucket[] = [
    'only-left',
    'only-right',
    'newer-left',
    'newer-right',
    'same',
    'conflict',
];

const sizeFor = (entry: CompareInputEntry | undefined): number => {
    if (!entry || entry.isDir) return 0;
    if (typeof entry.size !== 'number' || !Number.isFinite(entry.size)) return 0;
    return Math.max(0, entry.size);
};

const mtimeFor = (entry: CompareInputEntry | undefined): number | null => {
    if (!entry) return null;
    if (typeof entry.mtimeMs !== 'number' || !Number.isFinite(entry.mtimeMs)) return null;
    return entry.mtimeMs;
};

const emptyStats = (): CompareBucketStats => ({ count: 0, bytes: 0 });

const indexByName = (entries: CompareInputEntry[]): Map<string, CompareInputEntry> => {
    const map = new Map<string, CompareInputEntry>();
    for (const entry of entries) {
        if (!entry || typeof entry.name !== 'string' || entry.name === '') continue;
        // First-wins: defensive against duplicate listings from providers
        // that surface the same name twice (e.g. case-folding on macOS
        // returned by some FTP servers).
        if (!map.has(entry.name)) map.set(entry.name, entry);
    }
    return map;
};

/**
 * Classify two sibling file listings into Compare buckets.
 *
 * The function never throws: malformed entries (missing name, NaN size,
 * etc.) are skipped silently so a partial provider response can still be
 * compared. Returns deterministic, sorted bucket arrays.
 */
export const compareEntries = (
    left: CompareInputEntry[],
    right: CompareInputEntry[],
    options: CompareOptions = {},
): CompareResult => {
    const policy: ComparePolicy = options.policy ?? 'size-and-mtime';
    const mtimeToleranceMs = Math.max(0, options.mtimeToleranceMs ?? 2000);
    const skipDirectoryComparison = options.skipDirectoryComparison ?? true;

    const leftMap = indexByName(left);
    const rightMap = indexByName(right);
    const allNames = new Set<string>([...leftMap.keys(), ...rightMap.keys()]);

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

    const push = (bucket: CompareBucket, row: CompareResultEntry, bytes: number) => {
        buckets[bucket].push(row);
        stats[bucket].count += 1;
        stats[bucket].bytes += bytes;
    };

    for (const name of allNames) {
        const left = leftMap.get(name);
        const right = rightMap.get(name);

        if (left && !right) {
            push(
                'only-left',
                {
                    name,
                    bucket: 'only-left',
                    leftIsDir: left.isDir,
                    leftSize: left.size ?? null,
                    leftMtimeMs: left.mtimeMs ?? null,
                },
                sizeFor(left),
            );
            continue;
        }
        if (!left && right) {
            push(
                'only-right',
                {
                    name,
                    bucket: 'only-right',
                    rightIsDir: right.isDir,
                    rightSize: right.size ?? null,
                    rightMtimeMs: right.mtimeMs ?? null,
                },
                sizeFor(right),
            );
            continue;
        }
        if (!left || !right) continue;

        const row: CompareResultEntry = {
            name,
            bucket: 'same',
            leftIsDir: left.isDir,
            rightIsDir: right.isDir,
            leftSize: left.size ?? null,
            rightSize: right.size ?? null,
            leftMtimeMs: left.mtimeMs ?? null,
            rightMtimeMs: right.mtimeMs ?? null,
        };
        const meanBytes = Math.max(sizeFor(left), sizeFor(right));

        if (left.isDir !== right.isDir) {
            row.bucket = 'conflict';
            push('conflict', row, meanBytes);
            continue;
        }

        if (left.isDir && right.isDir) {
            if (skipDirectoryComparison) {
                push('same', row, 0);
                continue;
            }
            // Fallthrough: caller asked for a "deep" compare but we treat
            // dirs as same here too — recursion belongs to Z.3.7.2.
            push('same', row, 0);
            continue;
        }

        const leftMtime = mtimeFor(left);
        const rightMtime = mtimeFor(right);
        const sizesEqual = sizeFor(left) === sizeFor(right);

        if (policy === 'size-only') {
            if (sizesEqual) {
                row.bucket = 'same';
                push('same', row, meanBytes);
            } else {
                row.bucket = 'conflict';
                push('conflict', row, meanBytes);
            }
            continue;
        }

        if (leftMtime === null || rightMtime === null) {
            // Missing mtime on at least one side: degrade gracefully.
            if (sizesEqual) {
                row.bucket = 'same';
                push('same', row, meanBytes);
            } else {
                row.bucket = 'conflict';
                push('conflict', row, meanBytes);
            }
            continue;
        }

        const mtimeDelta = leftMtime - rightMtime;
        const mtimeWithinTol = Math.abs(mtimeDelta) <= mtimeToleranceMs;

        if (policy === 'mtime-only') {
            if (mtimeWithinTol) {
                row.bucket = 'same';
                push('same', row, meanBytes);
            } else if (mtimeDelta > 0) {
                row.bucket = 'newer-left';
                push('newer-left', row, sizeFor(left));
            } else {
                row.bucket = 'newer-right';
                push('newer-right', row, sizeFor(right));
            }
            continue;
        }

        // size-and-mtime (default)
        if (mtimeWithinTol && sizesEqual) {
            row.bucket = 'same';
            push('same', row, meanBytes);
        } else if (mtimeWithinTol && !sizesEqual) {
            row.bucket = 'conflict';
            push('conflict', row, meanBytes);
        } else if (mtimeDelta > 0) {
            row.bucket = 'newer-left';
            push('newer-left', row, sizeFor(left));
        } else {
            row.bucket = 'newer-right';
            push('newer-right', row, sizeFor(right));
        }
    }

    // Stable alphabetical order inside each bucket for predictable UI.
    for (const bucket of BUCKETS) {
        buckets[bucket].sort((a, b) => a.name.localeCompare(b.name));
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
        appliedOptions: { policy, mtimeToleranceMs, skipDirectoryComparison },
    };
};

/**
 * Convenience selector: pick the names of every entry that falls in the
 * "interesting" buckets for a mirror-left→right transfer (i.e. things the
 * right panel does NOT have yet, or that left holds a newer copy of).
 */
export const namesToMirrorLeftToRight = (result: CompareResult): string[] => [
    ...result.buckets['only-left'].map((entry) => entry.name),
    ...result.buckets['newer-left'].map((entry) => entry.name),
];

export const namesToMirrorRightToLeft = (result: CompareResult): string[] => [
    ...result.buckets['only-right'].map((entry) => entry.name),
    ...result.buckets['newer-right'].map((entry) => entry.name),
];

export const __TEST_ONLY__ = {
    BUCKETS,
};
