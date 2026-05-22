// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// GAP-9c of the AeroSync connected-remote gap-closure filone.
//
// Serialises a `CompareResult` into the JSON / CSV dry-run export migrated
// from the legacy `SyncPanel` (#149). Kept DOM- and Tauri-free so it is
// unit-testable; `CompareTabContent` wraps it with the `save()` dialog and
// `writeTextFile`. The unified compare carries a `/`-separated
// `relativePath`, so the export is recursive where the legacy flat one was
// not.

import type { CompareResult } from './compareEndpoints';

/** One exported row — left/right oriented, matching the Compare tab model. */
export interface CompareExportRow {
    path: string;
    status: string;
    is_dir: boolean;
    left_size: number | null;
    left_modified: string | null;
    right_size: number | null;
    right_modified: string | null;
}

/** Epoch-ms mtime → ISO string, or null when absent / invalid. */
const toIso = (ms?: number | null): string | null => {
    if (typeof ms !== 'number' || !Number.isFinite(ms) || ms <= 0) return null;
    try {
        return new Date(ms).toISOString();
    } catch {
        return null;
    }
};

/** Flatten every compare entry into export rows (recursive `relativePath`). */
export const buildCompareExportRows = (result: CompareResult): CompareExportRow[] =>
    result.entries.map((e) => ({
        path: e.relativePath ?? e.name,
        status: e.bucket,
        is_dir: !!(e.leftIsDir || e.rightIsDir),
        left_size: e.leftSize ?? null,
        left_modified: toIso(e.leftMtimeMs),
        right_size: e.rightSize ?? null,
        right_modified: toIso(e.rightMtimeMs),
    }));

/** Pretty-printed JSON document. */
export const compareRowsToJson = (rows: CompareExportRow[]): string =>
    JSON.stringify(rows, null, 2);

/** RFC 4180 minimal quoting for a CSV cell. */
const csvCell = (value: string): string => `"${value.replace(/"/g, '""')}"`;

export const COMPARE_CSV_HEADER =
    'path,status,is_dir,left_size,left_modified,right_size,right_modified';

/** CSV document with a header row; free-text cells are quoted. */
export const compareRowsToCsv = (rows: CompareExportRow[]): string => {
    const lines = rows.map((r) => [
        csvCell(r.path),
        r.status,
        r.is_dir,
        r.left_size ?? '',
        r.left_modified ?? '',
        r.right_size ?? '',
        r.right_modified ?? '',
    ].join(','));
    return [COMPARE_CSV_HEADER, ...lines].join('\n');
};

/** `aerosync-dryrun-<timestamp>.<ext>`, matching the legacy filename. */
export const compareExportFilename = (
    ext: 'json' | 'csv',
    now: Date = new Date(),
): string => {
    const ts = now.toISOString().replace(/[:.]/g, '-').slice(0, 19);
    return `aerosync-dryrun-${ts}.${ext}`;
};
