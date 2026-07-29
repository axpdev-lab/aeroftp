// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import {
    ArrowLeftRight,
    ArrowRight,
    ChevronDown,
    ChevronRight,
    Equal,
    FileJson,
    FileSpreadsheet,
    FileWarning,
    Loader2,
    type LucideIcon,
} from 'lucide-react';
import { pickSave } from '../../utils/pickPath';
import { writeTextFile } from '@tauri-apps/plugin-fs';
import type {
    CompareBucket,
    CompareResult,
    CompareResultEntry,
} from '../../utils/compareEndpoints';
import {
    buildCompareExportRows,
    compareExportFilename,
    compareRowsToCsv,
    compareRowsToJson,
} from '../../utils/compareExport';
import { formatBytes } from '../../utils/formatters';
import { useScanProgress, formatElapsed } from '../../hooks/useScanProgress';
import { useTranslation } from '../../i18n';

interface CompareTabContentProps {
    result: CompareResult | null;
    /** GAP-5: true while the recursive connected-remote scan is running. */
    loading?: boolean;
    leftLabel: string;
    rightLabel: string;
    pairKind?: string | null;
    canMirrorLeftToRight: boolean;
    canMirrorRightToLeft: boolean;
    onApplyMirrorLeftToRight: (entries: CompareResultEntry[]) => void;
    onApplyMirrorRightToLeft: (entries: CompareResultEntry[]) => void;
}

const BUCKET_ORDER: CompareBucket[] = [
    'only-left',
    'newer-left',
    'only-right',
    'newer-right',
    'conflict',
    'same',
];

const BUCKET_META: Record<CompareBucket, {
    label: string;
    description: string;
    accentClass: string;
    icon: LucideIcon;
}> = {
    'only-left': {
        label: 'Only on left',
        description: 'Present on the source panel, missing from the destination.',
        accentClass: 'border-l-emerald-400 bg-emerald-50/60 dark:border-l-emerald-500 dark:bg-emerald-900/20',
        icon: ArrowRight,
    },
    'newer-left': {
        label: 'Newer on left',
        description: 'Present on both sides, the source copy has the more recent timestamp.',
        accentClass: 'border-l-sky-400 bg-sky-50/60 dark:border-l-sky-500 dark:bg-sky-900/20',
        icon: ArrowRight,
    },
    'only-right': {
        label: 'Only on right',
        description: 'Present on the destination, missing from the source panel.',
        accentClass: 'border-l-amber-400 bg-amber-50/60 dark:border-l-amber-500 dark:bg-amber-900/20',
        icon: ArrowRight,
    },
    'newer-right': {
        label: 'Newer on right',
        description: 'Present on both sides, the destination copy has the more recent timestamp.',
        accentClass: 'border-l-orange-400 bg-orange-50/60 dark:border-l-orange-500 dark:bg-orange-900/20',
        icon: ArrowRight,
    },
    conflict: {
        label: 'Conflict',
        description: 'Both sides have the entry but they disagree on size with comparable timestamps.',
        accentClass: 'border-l-rose-400 bg-rose-50/60 dark:border-l-rose-500 dark:bg-rose-900/20',
        icon: FileWarning,
    },
    same: {
        label: 'Same',
        description: 'Entries match according to the active comparison policy.',
        accentClass: 'border-l-gray-300 bg-gray-50/60 dark:border-l-gray-600 dark:bg-gray-800/40',
        icon: Equal,
    },
};

const MAX_PREVIEW_ROWS = 80;

const formatTimestamp = (mtime?: number | null): string => {
    if (typeof mtime !== 'number' || !Number.isFinite(mtime) || mtime <= 0) return '-';
    try {
        return new Date(mtime).toLocaleString();
    } catch {
        return '-';
    }
};

const formatSize = (size?: number | null): string => {
    if (typeof size !== 'number' || !Number.isFinite(size)) return '-';
    return formatBytes(Math.max(0, size));
};

// EF-21 (#347): out-of-sync ratio for the Compare summary. Guards a zero
// denominator (empty comparison) by rendering an em dash. Shows an integer
// when the value is clean, otherwise one decimal place.
const formatOutOfSyncPct = (numerator: number, denominator: number): string => {
    if (!Number.isFinite(denominator) || denominator <= 0) return '—';
    const pct = (Math.max(0, numerator) / denominator) * 100;
    if (!Number.isFinite(pct)) return '—';
    const rounded = Math.round(pct * 10) / 10;
    return `${Number.isInteger(rounded) ? rounded.toFixed(0) : rounded.toFixed(1)}%`;
};

interface BucketSectionProps {
    bucket: CompareBucket;
    entries: CompareResultEntry[];
    bytes: number;
    initiallyOpen: boolean;
}

const BucketSection: React.FC<BucketSectionProps> = ({ bucket, entries, bytes, initiallyOpen }) => {
    const [open, setOpen] = React.useState(initiallyOpen);
    const meta = BUCKET_META[bucket];
    const Icon = meta.icon;
    const truncated = entries.length > MAX_PREVIEW_ROWS;
    const visible = truncated ? entries.slice(0, MAX_PREVIEW_ROWS) : entries;

    return (
        <div className={`rounded-md border-l-4 border border-gray-200 dark:border-gray-700 ${meta.accentClass}`}>
            <button
                type="button"
                onClick={() => setOpen((value) => !value)}
                className="flex w-full items-center justify-between gap-3 px-3 py-2 text-left"
                aria-expanded={open}
            >
                <div className="flex min-w-0 items-center gap-2">
                    <Icon size={14} className="shrink-0 text-gray-600 dark:text-gray-300" />
                    <span className="truncate text-sm font-semibold text-gray-800 dark:text-gray-100">
                        {meta.label}
                    </span>
                    <span className="rounded-full bg-white/70 px-2 py-0.5 text-[11px] font-medium text-gray-700 shadow-sm dark:bg-gray-900/40 dark:text-gray-200">
                        {entries.length}
                    </span>
                    <span className="text-[11px] text-gray-500 dark:text-gray-400">{formatBytes(bytes)}</span>
                </div>
                {open ? (
                    <ChevronDown size={14} className="text-gray-500 dark:text-gray-400" />
                ) : (
                    <ChevronRight size={14} className="text-gray-500 dark:text-gray-400" />
                )}
            </button>
            {open && (
                <div className="border-t border-gray-200 px-3 py-2 dark:border-gray-700">
                    <p className="mb-2 text-[11px] text-gray-500 dark:text-gray-400">{meta.description}</p>
                    {entries.length === 0 ? (
                        <p className="text-xs italic text-gray-400 dark:text-gray-500">No entries.</p>
                    ) : (
                        <div className="max-h-48 overflow-y-auto rounded-md border border-gray-200 bg-white/60 dark:border-gray-700 dark:bg-gray-900/30">
                            <table className="w-full text-[11px]">
                                <thead className="sticky top-0 bg-gray-100 text-left text-gray-600 dark:bg-gray-800 dark:text-gray-300">
                                    <tr>
                                        <th className="px-2 py-1 font-medium">Name</th>
                                        <th className="px-2 py-1 font-medium text-right">Left size</th>
                                        <th className="px-2 py-1 font-medium text-right">Right size</th>
                                        <th className="px-2 py-1 font-medium">Left mtime</th>
                                        <th className="px-2 py-1 font-medium">Right mtime</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {visible.map((entry) => (
                                        <tr key={entry.relativePath ?? entry.name} className="border-t border-gray-100 dark:border-gray-700/60">
                                            <td className="px-2 py-1 font-mono text-gray-800 dark:text-gray-200">
                                                {entry.relativePath ?? entry.name}
                                            </td>
                                            <td className="px-2 py-1 text-right text-gray-600 dark:text-gray-300">
                                                {entry.leftIsDir ? 'dir' : formatSize(entry.leftSize)}
                                            </td>
                                            <td className="px-2 py-1 text-right text-gray-600 dark:text-gray-300">
                                                {entry.rightIsDir ? 'dir' : formatSize(entry.rightSize)}
                                            </td>
                                            <td className="px-2 py-1 text-gray-500 dark:text-gray-400">
                                                {formatTimestamp(entry.leftMtimeMs)}
                                            </td>
                                            <td className="px-2 py-1 text-gray-500 dark:text-gray-400">
                                                {formatTimestamp(entry.rightMtimeMs)}
                                            </td>
                                        </tr>
                                    ))}
                                </tbody>
                            </table>
                            {truncated && (
                                <p className="border-t border-gray-200 px-3 py-1 text-[11px] text-gray-500 dark:border-gray-700 dark:text-gray-400">
                                    Showing first {MAX_PREVIEW_ROWS} of {entries.length} entries.
                                </p>
                            )}
                        </div>
                    )}
                </div>
            )}
        </div>
    );
};

export const CompareTabContent: React.FC<CompareTabContentProps> = ({
    result,
    loading,
    leftLabel,
    rightLabel,
    pairKind,
    canMirrorLeftToRight,
    canMirrorRightToLeft,
    onApplyMirrorLeftToRight,
    onApplyMirrorRightToLeft,
}) => {
    const t = useTranslation();
    // Live counters for the scan the spinner below used to hide entirely.
    const { totals: scanTotals, elapsedMs: scanElapsedMs } = useScanProgress(!!loading && !result);

    // Hooks must run unconditionally: the recursive connected-remote scan
    // (GAP-5) flips `result` from null to a value while this component stays
    // mounted, so an early return above a hook would change the hook count
    // between renders. Each memo guards the null case instead.
    const policyLabel = React.useMemo(() => {
        switch (result?.appliedOptions.policy) {
            case 'size-only':
                return 'Size only';
            case 'mtime-only':
                return 'Timestamp only';
            default:
                return 'Size + timestamp';
        }
    }, [result?.appliedOptions.policy]);

    const leftToRightEntries = React.useMemo(
        () => (result
            ? [...result.buckets['only-left'], ...result.buckets['newer-left']]
            : []),
        [result],
    );
    const rightToLeftEntries = React.useMemo(
        () => (result
            ? [...result.buckets['only-right'], ...result.buckets['newer-right']]
            : []),
        [result],
    );

    // GAP-9c: dry-run export of the compare result (#149, migrated from the
    // legacy SyncPanel). The unified compare carries a recursive
    // `relativePath`, so the export covers nested entries the legacy flat
    // export missed.
    const handleExportJson = async (): Promise<void> => {
        if (!result || result.entries.length === 0) return;
        try {
            const filePath = await pickSave({
                defaultPath: compareExportFilename('json'),
                filters: [{ name: 'JSON', extensions: ['json'] }],
            });
            if (filePath) {
                await writeTextFile(
                    filePath,
                    compareRowsToJson(buildCompareExportRows(result)),
                );
            }
        } catch {
            /* dialog cancelled or write error */
        }
    };

    const handleExportCsv = async (): Promise<void> => {
        if (!result || result.entries.length === 0) return;
        try {
            const filePath = await pickSave({
                defaultPath: compareExportFilename('csv'),
                filters: [{ name: 'CSV', extensions: ['csv'] }],
            });
            if (filePath) {
                await writeTextFile(
                    filePath,
                    compareRowsToCsv(buildCompareExportRows(result)),
                );
            }
        } catch {
            /* dialog cancelled or write error */
        }
    };

    if (!result) {
        if (loading) {
            return (
                <div className="flex flex-col items-center gap-2 p-8 text-center text-sm text-gray-500 dark:text-gray-400">
                    <Loader2 size={24} className="animate-spin text-blue-500" />
                    {t('syncPanel.scanning') || 'Scanning directories'}
                    {/* The same numbers the summary will show when the scan
                        finishes, shown while they are still the only thing to
                        go on: a scan that is working and a scan that is stuck
                        used to look identical. */}
                    <div className="flex flex-wrap items-center justify-center gap-x-4 gap-y-1 text-xs tabular-nums text-gray-400 dark:text-gray-500">
                        <span>⏱ {formatElapsed(scanElapsedMs)}</span>
                        <span>📄 {scanTotals.files.toLocaleString()}</span>
                        <span>📁 {scanTotals.dirs.toLocaleString()}</span>
                        <span>{formatBytes(scanTotals.bytes)}</span>
                    </div>
                </div>
            );
        }
        return (
            <div className="p-6 text-center text-sm text-gray-500 dark:text-gray-400">
                {t('aerosync.compareUnavailable') || 'Open a second panel or a remote connection to compare.'}
            </div>
        );
    }

    return (
        <div className="flex flex-col">
            <div className="grid gap-3 px-4 py-3 sm:grid-cols-3">
                <div className="rounded-md bg-gray-50 p-3 dark:bg-gray-900/40">
                    <div className="text-[11px] uppercase tracking-wide text-gray-500 dark:text-gray-400">
                        {t('aerosync.totalEntries') || 'Total entries'}
                    </div>
                    <div className="font-semibold text-gray-900 dark:text-white">{result.totals.count}</div>
                </div>
                <div className="rounded-md bg-gray-50 p-3 dark:bg-gray-900/40">
                    <div className="text-[11px] uppercase tracking-wide text-gray-500 dark:text-gray-400">
                        {t('aerosync.differences') || 'Differences'}
                    </div>
                    <div className="font-semibold text-gray-900 dark:text-white">
                        {result.totals.count - result.stats.same.count}
                    </div>
                </div>
                <div className="rounded-md bg-gray-50 p-3 dark:bg-gray-900/40">
                    <div className="text-[11px] uppercase tracking-wide text-gray-500 dark:text-gray-400">
                        {t('aerosync.totalBytes') || 'Total bytes'}
                    </div>
                    <div className="font-semibold text-gray-900 dark:text-white">{formatBytes(result.totals.bytes)}</div>
                </div>
                <div className="rounded-md bg-gray-50 p-3 dark:bg-gray-900/40">
                    <div className="text-[11px] uppercase tracking-wide text-gray-500 dark:text-gray-400">
                        {t('aerosync.outOfSyncItemsPct') || 'Items out of sync'}
                    </div>
                    <div className="font-semibold text-gray-900 dark:text-white">
                        {formatOutOfSyncPct(result.totals.count - result.stats.same.count, result.totals.count)}
                    </div>
                </div>
                <div className="rounded-md bg-gray-50 p-3 dark:bg-gray-900/40">
                    <div className="text-[11px] uppercase tracking-wide text-gray-500 dark:text-gray-400">
                        {t('aerosync.outOfSyncBytesPct') || 'Bytes out of sync'}
                    </div>
                    <div className="font-semibold text-gray-900 dark:text-white">
                        {formatOutOfSyncPct(result.totals.bytes - result.stats.same.bytes, result.totals.bytes)}
                    </div>
                </div>
            </div>

            <div className="px-4 pb-2 text-[11px] text-gray-500 dark:text-gray-400">
                {leftLabel} <ArrowLeftRight size={11} className="inline align-middle" /> {rightLabel}
                <span className="mx-1">&middot;</span>
                {policyLabel}
                {pairKind ? <span className="ml-1">&middot; {pairKind}</span> : null}
            </div>

            <div className="max-h-[50vh] space-y-2 overflow-y-auto px-4 pb-3">
                {BUCKET_ORDER.map((bucket) => (
                    <BucketSection
                        key={bucket}
                        bucket={bucket}
                        entries={result.buckets[bucket]}
                        bytes={result.stats[bucket].bytes}
                        initiallyOpen={bucket !== 'same' && result.buckets[bucket].length > 0}
                    />
                ))}
            </div>

            <div className="flex flex-col gap-2 border-t border-gray-200 px-4 py-3 dark:border-gray-700 sm:flex-row sm:items-center sm:justify-between">
                <p className="text-[11px] text-gray-500 dark:text-gray-400">
                    {t('aerosync.mirrorHint') || 'Mirror buttons stage the matching entries in the unified transfer planner for review.'}
                </p>
                <div className="flex flex-wrap justify-end gap-2">
                    <button
                        type="button"
                        onClick={handleExportJson}
                        disabled={result.entries.length === 0}
                        className="inline-flex items-center gap-1.5 rounded-lg border border-gray-300 px-2.5 py-2 text-xs font-medium text-gray-600 transition-colors hover:bg-gray-50 disabled:opacity-40 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-700"
                    >
                        <FileJson size={13} />
                        {t('syncPanel.exportJSON') || 'Export JSON'}
                    </button>
                    <button
                        type="button"
                        onClick={handleExportCsv}
                        disabled={result.entries.length === 0}
                        className="inline-flex items-center gap-1.5 rounded-lg border border-gray-300 px-2.5 py-2 text-xs font-medium text-gray-600 transition-colors hover:bg-gray-50 disabled:opacity-40 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-700"
                    >
                        <FileSpreadsheet size={13} />
                        {t('syncPanel.exportCSV') || 'Export CSV'}
                    </button>
                    <button
                        type="button"
                        onClick={() => onApplyMirrorRightToLeft(rightToLeftEntries)}
                        disabled={!canMirrorRightToLeft || rightToLeftEntries.length === 0}
                        className="inline-flex items-center gap-2 rounded-lg bg-gray-700 px-3 py-2 text-sm text-white transition-colors hover:bg-gray-800 disabled:opacity-40"
                    >
                        &larr; {t('aerosync.mirrorRightToLeft') || 'Mirror right to left'} ({rightToLeftEntries.length})
                    </button>
                    <button
                        type="button"
                        onClick={() => onApplyMirrorLeftToRight(leftToRightEntries)}
                        disabled={!canMirrorLeftToRight || leftToRightEntries.length === 0}
                        className="inline-flex items-center gap-2 rounded-lg bg-blue-600 px-3 py-2 text-sm text-white transition-colors hover:bg-blue-700 disabled:opacity-40"
                    >
                        {t('aerosync.mirrorLeftToRight') || 'Mirror left to right'} ({leftToRightEntries.length}) &rarr;
                    </button>
                </div>
            </div>
        </div>
    );
};

export default CompareTabContent;
