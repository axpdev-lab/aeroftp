// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Local-to-local sync tab content: walks a local source directory and
// mirrors it to a local destination. Files >= 1 MiB route through the
// AeroRsync in-process delta engine; smaller files use plain copy.
// Backend command `local_sync_run`, progress event `local-sync-progress`.

import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { pickFile } from '../../utils/pickPath';
import { ChevronDown, ChevronRight, CircleStop, FolderOpen, Info, Play, AlertTriangle, Ban, CheckCircle2, Zap } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { TransferProgressBar } from '../TransferProgressBar';
import type { AeroSyncPairKind } from './types';

interface SyncTabContentProps {
    initialSource?: string;
    initialDestination?: string;
    pairKind?: AeroSyncPairKind | null;
    activeProfileId?: string;
    /** Lifts the in-progress state up so the host dialog can guard its close
     *  while a sync is running (issue #332). */
    onRunningChange?: (running: boolean) => void;
}

// SLICE 4 + CO-4: advanced override for the auto-detected delta gate.
// - `DELTA_OVERRIDE_KEY` is the legacy global slot. CO-4 keeps it alive
//   as the fallback whenever no profile id is in scope (AeroFile / dual-
//   local sessions).
// - `DELTA_OVERRIDES_MAP_KEY` is the per-profile map introduced by CO-4
//   so that connected-remote sessions can remember a different choice
//   per saved server, mirroring what SyncPanel did via
//   `skipDeltaEligibilityPrompt`.
const DELTA_OVERRIDE_KEY = 'aerosync.delta.override';
const DELTA_OVERRIDES_MAP_KEY = 'aerosync.delta.overrides';
const DELTA_OVERRIDE_VALUES = ['auto', 'force-on', 'force-off'] as const;
type DeltaOverride = typeof DELTA_OVERRIDE_VALUES[number];
function isDeltaOverride(value: unknown): value is DeltaOverride {
    return typeof value === 'string'
        && (DELTA_OVERRIDE_VALUES as readonly string[]).includes(value);
}
function readOverrideMap(): Record<string, DeltaOverride> {
    try {
        const raw = localStorage.getItem(DELTA_OVERRIDES_MAP_KEY);
        if (!raw) return {};
        const parsed = JSON.parse(raw);
        if (!parsed || typeof parsed !== 'object') return {};
        const cleaned: Record<string, DeltaOverride> = {};
        for (const [k, v] of Object.entries(parsed)) {
            if (isDeltaOverride(v)) cleaned[k] = v;
        }
        return cleaned;
    } catch { return {}; }
}
function writeOverrideMap(map: Record<string, DeltaOverride>) {
    try {
        const hasEntries = Object.keys(map).length > 0;
        if (!hasEntries) localStorage.removeItem(DELTA_OVERRIDES_MAP_KEY);
        else localStorage.setItem(DELTA_OVERRIDES_MAP_KEY, JSON.stringify(map));
    } catch { /* localStorage unavailable */ }
}
function readDeltaOverride(profileId?: string): DeltaOverride {
    try {
        if (profileId) {
            return readOverrideMap()[profileId] || 'auto';
        }
        const raw = localStorage.getItem(DELTA_OVERRIDE_KEY);
        if (isDeltaOverride(raw)) return raw;
    } catch { /* localStorage unavailable */ }
    return 'auto';
}
function writeDeltaOverride(value: DeltaOverride, profileId?: string) {
    try {
        if (profileId) {
            const map = readOverrideMap();
            if (value === 'auto') delete map[profileId];
            else map[profileId] = value;
            writeOverrideMap(map);
            return;
        }
        if (value === 'auto') localStorage.removeItem(DELTA_OVERRIDE_KEY);
        else localStorage.setItem(DELTA_OVERRIDE_KEY, value);
    } catch { /* localStorage unavailable */ }
}

// CO-3: bandwidth caps in KB/s. 0 = unlimited. Persisted under
// dedicated localStorage keys so the user's preference survives
// across runs (same pattern as the delta override).
const UPLOAD_LIMIT_KEY = 'aerosync.bandwidth.upload';
const DOWNLOAD_LIMIT_KEY = 'aerosync.bandwidth.download';
function readBandwidthLimit(key: string): number {
    try {
        const raw = localStorage.getItem(key);
        if (!raw) return 0;
        const parsed = Number.parseInt(raw, 10);
        return Number.isFinite(parsed) && parsed >= 0 ? parsed : 0;
    } catch { return 0; }
}
function writeBandwidthLimit(key: string, value: number) {
    try {
        if (!value || value <= 0) localStorage.removeItem(key);
        else localStorage.setItem(key, String(Math.floor(value)));
    } catch { /* localStorage unavailable */ }
}

interface LocalSyncEntry {
    name: string;
    action: string; // 'delta' | 'copy' | 'skip' | 'dry-run' | 'error'
    bytes: number;
    status: string; // 'ok' | 'error' | 'skipped' | 'dry-run'
}

interface LocalSyncReport {
    status: string;
    uploaded: number;
    skipped: number;
    errors: number;
    elapsed_ms: number;
    total_payload_bytes: number;
    bytes_on_wire: number;
    savings_ratio: number;
    error_messages: string[];
    entries?: LocalSyncEntry[];
    entries_truncated?: boolean;
}

interface LocalSyncProgress {
    processed: number;
    total: number;
    current_path: string;
    bytes_on_wire: number;
    used_delta: boolean;
}

function formatBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
    if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MiB`;
    return `${(n / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}

export const SyncTabContent: React.FC<SyncTabContentProps> = ({
    initialSource = '',
    initialDestination = '',
    pairKind = null,
    activeProfileId,
    onRunningChange,
}) => {
    const t = useTranslation();
    const [source, setSource] = useState(initialSource);
    const [destination, setDestination] = useState(initialDestination);
    const [exclude, setExclude] = useState('');
    // CO-4: profile-keyed override; falls back to the global slot when
    // no profile is in scope (AeroFile / dual-local sessions). The
    // initial read uses the active profile id so dialog re-opens for a
    // different profile render the right pill out of the box.
    const [deltaOverride, setDeltaOverride] = useState<DeltaOverride>(() => readDeltaOverride(activeProfileId));
    const [uploadLimit, setUploadLimit] = useState<number>(() => readBandwidthLimit(UPLOAD_LIMIT_KEY));
    const [downloadLimit, setDownloadLimit] = useState<number>(() => readBandwidthLimit(DOWNLOAD_LIMIT_KEY));
    const [dryRun, setDryRun] = useState(false);
    const [running, setRunning] = useState(false);
    const [cancelling, setCancelling] = useState(false);
    const [progress, setProgress] = useState<LocalSyncProgress | null>(null);
    const [report, setReport] = useState<LocalSyncReport | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [showAdvanced, setShowAdvanced] = useState(false);

    // SLICE 4 delta-sync gate consolidation:
    //  - pairKind=local-local      -> always eligible (AeroRsync local engine)
    //  - pairKind=local-remote /
    //    pairKind=remote-local     -> Sync tab still runs local_sync_run, so
    //                                  delta only applies when source AND
    //                                  destination are both local paths.
    //                                  SLICE 6 will route this tab through
    //                                  the right backend per pairKind.
    //  - pairKind=null             -> no panel context, fall back to auto.
    const deltaEligible = pairKind === 'local-local' || pairKind === null;
    const useDelta = deltaOverride === 'force-off'
        ? false
        : deltaOverride === 'force-on'
            ? true
            : deltaEligible;

    useEffect(() => {
        if (initialSource) setSource(initialSource);
        if (initialDestination) setDestination(initialDestination);
    }, [initialSource, initialDestination]);

    // CO-4: re-read the override when the active profile changes (the
    // dialog can swap between profiles without unmounting). Without
    // this the pill would stay frozen on the first-mount profile.
    useEffect(() => {
        setDeltaOverride(readDeltaOverride(activeProfileId));
    }, [activeProfileId]);

    // Lift the running state so the host dialog (AeroSyncDialog) can guard
    // its close while a local sync is in flight (issue #332). On unmount we
    // report "not running" so the parent guard can never get stuck.
    useEffect(() => {
        onRunningChange?.(running);
    }, [running, onRunningChange]);
    useEffect(() => () => onRunningChange?.(false), [onRunningChange]);

    useEffect(() => {
        let unlisten: UnlistenFn | null = null;
        listen<LocalSyncProgress>('local-sync-progress', (e) => {
            setProgress(e.payload);
        }).then((fn) => {
            unlisten = fn;
        });
        return () => {
            if (unlisten) unlisten();
        };
    }, []);

    const pickFolder = async (kind: 'source' | 'destination') => {
        try {
            const sel = await pickFile({ directory: true, multiple: false });
            if (typeof sel === 'string') {
                if (kind === 'source') setSource(sel);
                else setDestination(sel);
            }
        } catch (e) {
            console.error(e);
        }
    };

    const start = async () => {
        setError(null);
        setReport(null);
        setProgress(null);
        if (!source || !destination) {
            setError(t('aerosync.sync.errorPathsRequired') || 'Source and destination are required');
            return;
        }
        setRunning(true);
        setCancelling(false);
        try {
            const excludeList = exclude
                .split(/[\n,]/)
                .map((s) => s.trim())
                .filter(Boolean);
            const result = await invoke<LocalSyncReport>('local_sync_run', {
                request: {
                    source,
                    destination,
                    exclude: excludeList,
                    no_delta: !useDelta,
                    dry_run: dryRun,
                    // CO-3: forward bandwidth caps (KB/s, 0 = unlimited).
                    upload_limit_kbps: uploadLimit > 0 ? uploadLimit : null,
                    download_limit_kbps: downloadLimit > 0 ? downloadLimit : null,
                },
            });
            setReport(result);
        } catch (e) {
            setError(String(e));
        } finally {
            setRunning(false);
            setCancelling(false);
        }
    };

    // Ask the backend to stop the in-progress run at the next file boundary.
    // The run resolves on its own with a `cancelled` report; we only flip the
    // button into a "stopping" state here.
    const cancelRun = async () => {
        setCancelling(true);
        try {
            await invoke('local_sync_cancel');
        } catch (e) {
            console.error(e);
        }
    };

    const pct = progress && progress.total > 0 ? (progress.processed / progress.total) * 100 : 0;

    return (
        <div className="p-4 space-y-4">
            <p className="text-sm text-gray-600 dark:text-gray-400">
                {t('aerosync.sync.description') ||
                    'Mirror a local source directory to a local destination. Files >= 1 MiB route through the AeroRsync delta engine; smaller files use plain copy.'}
            </p>

            <div>
                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1.5">
                    {t('aerosync.sync.source') || 'Source'}
                </label>
                <div className="flex gap-2">
                    <input
                        type="text"
                        value={source}
                        onChange={(e) => setSource(e.target.value)}
                        disabled={running}
                        className="flex-1 px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-sm disabled:opacity-50"
                        placeholder="/path/to/source"
                    />
                    <button
                        type="button"
                        onClick={() => pickFolder('source')}
                        disabled={running}
                        className="px-3 py-2 bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors text-sm flex items-center gap-1.5 disabled:opacity-50"
                    >
                        <FolderOpen size={16} />
                        {t('aerosync.sync.browse') || 'Browse...'}
                    </button>
                </div>
            </div>

            <div>
                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1.5">
                    {t('aerosync.sync.destination') || 'Destination'}
                </label>
                <div className="flex gap-2">
                    <input
                        type="text"
                        value={destination}
                        onChange={(e) => setDestination(e.target.value)}
                        disabled={running}
                        className="flex-1 px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-sm disabled:opacity-50"
                        placeholder="/path/to/destination"
                    />
                    <button
                        type="button"
                        onClick={() => pickFolder('destination')}
                        disabled={running}
                        className="px-3 py-2 bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors text-sm flex items-center gap-1.5 disabled:opacity-50"
                    >
                        <FolderOpen size={16} />
                        {t('aerosync.sync.browse') || 'Browse...'}
                    </button>
                </div>
            </div>

            <div>
                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1.5">
                    {t('aerosync.sync.excludeLabel') || 'Exclude patterns (comma or newline separated)'}
                </label>
                <input
                    type="text"
                    value={exclude}
                    onChange={(e) => setExclude(e.target.value)}
                    disabled={running}
                    placeholder="*.tmp, .git, node_modules"
                    className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-sm disabled:opacity-50"
                />
            </div>

            {/* SLICE 4: delta-sync auto-detect indicator. The legacy
                per-run checkbox is now an advanced override hidden
                behind a disclosure. */}
            <div className="flex flex-col gap-2 pt-1">
                <div className="flex items-center justify-between gap-3 px-3 py-2 rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-900/40">
                    <div className="flex items-center gap-2 text-sm">
                        <Zap size={14} className={useDelta ? 'text-emerald-500' : 'text-gray-400'} />
                        <span className="font-medium text-gray-700 dark:text-gray-200">
                            {t('aerosync.sync.deltaStatus') || 'Delta sync'}
                        </span>
                        <span className={`text-[11px] rounded px-1.5 py-0.5 font-semibold ${
                            useDelta
                                ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300'
                                : 'bg-gray-200 text-gray-600 dark:bg-gray-700 dark:text-gray-300'
                        }`}>
                            {deltaOverride !== 'auto'
                                ? (t('aerosync.sync.deltaOverridden') || 'OVERRIDE')
                                : (useDelta ? (t('aerosync.sync.deltaAuto') || 'AUTO ON') : (t('aerosync.sync.deltaAutoOff') || 'AUTO OFF'))}
                        </span>
                    </div>
                    <button
                        type="button"
                        onClick={() => setShowAdvanced((v) => !v)}
                        className="flex items-center gap-1 text-xs text-blue-600 dark:text-blue-300 hover:underline"
                    >
                        {showAdvanced ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                        {t('aerosync.sync.advanced') || 'Advanced'}
                    </button>
                </div>
                {!deltaEligible && pairKind && (
                    <div className="flex items-start gap-2 px-3 py-2 rounded-lg border border-amber-200 bg-amber-50 dark:border-amber-800 dark:bg-amber-900/30 text-amber-800 dark:text-amber-200 text-xs">
                        <Info size={14} className="flex-shrink-0 mt-0.5" />
                        <span>
                            {t('aerosync.sync.localOnlyHint') ||
                                'This tab runs a local -> local mirror. For local <-> remote sync, use the Plan tab and execute a preset.'}
                        </span>
                    </div>
                )}
                {showAdvanced && (
                    <div className="px-3 py-2 rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800/50">
                        <label className="block text-xs font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400 mb-1.5">
                            {t('aerosync.sync.deltaOverrideLabel') || 'Delta-sync override'}
                        </label>
                        <select
                            value={deltaOverride}
                            onChange={(e) => {
                                const v = e.target.value as DeltaOverride;
                                setDeltaOverride(v);
                                writeDeltaOverride(v, activeProfileId);
                            }}
                            disabled={running}
                            className="w-full px-2 py-1.5 rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900/60 text-sm disabled:opacity-50"
                        >
                            <option value="auto">{t('aerosync.sync.deltaOverrideAuto') || 'Auto-detect (recommended)'}</option>
                            <option value="force-on">{t('aerosync.sync.deltaOverrideOn') || 'Force on (use delta when possible)'}</option>
                            <option value="force-off">{t('aerosync.sync.deltaOverrideOff') || 'Force off (plain copy)'}</option>
                        </select>
                        <p className="mt-1 text-[10px] leading-snug text-gray-500 dark:text-gray-400">
                            {t('aerosync.sync.deltaOverrideHint') || 'Auto-detect uses AeroRsync for files >= 1 MiB when both sides are local. Override is remembered between runs.'}
                        </p>
                        <label className="mt-3 flex items-center gap-2 text-sm text-gray-700 dark:text-gray-300 cursor-pointer">
                            <input
                                type="checkbox"
                                checked={dryRun}
                                onChange={(e) => setDryRun(e.target.checked)}
                                disabled={running}
                                className="rounded border-gray-300 dark:border-gray-600"
                            />
                            {t('aerosync.sync.dryRun') || 'Dry run (preview only)'}
                        </label>
                        {/* CO-3: bandwidth caps. 0 = unlimited; the
                            in-process planner divides the cap evenly
                            between the source-read / dest-write loop. */}
                        <div className="mt-3 grid grid-cols-2 gap-2">
                            <div>
                                <label className="block text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400 mb-1">
                                    {t('aerosync.sync.bandwidthUpload') || 'Upload KB/s'}
                                </label>
                                <input
                                    type="number"
                                    min={0}
                                    step={64}
                                    value={uploadLimit}
                                    onChange={(e) => {
                                        const v = Math.max(0, Number.parseInt(e.target.value, 10) || 0);
                                        setUploadLimit(v);
                                        writeBandwidthLimit(UPLOAD_LIMIT_KEY, v);
                                    }}
                                    disabled={running}
                                    placeholder="0"
                                    className="w-full rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900/60 px-2 py-1.5 text-sm disabled:opacity-50"
                                />
                            </div>
                            <div>
                                <label className="block text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400 mb-1">
                                    {t('aerosync.sync.bandwidthDownload') || 'Download KB/s'}
                                </label>
                                <input
                                    type="number"
                                    min={0}
                                    step={64}
                                    value={downloadLimit}
                                    onChange={(e) => {
                                        const v = Math.max(0, Number.parseInt(e.target.value, 10) || 0);
                                        setDownloadLimit(v);
                                        writeBandwidthLimit(DOWNLOAD_LIMIT_KEY, v);
                                    }}
                                    disabled={running}
                                    placeholder="0"
                                    className="w-full rounded-md border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900/60 px-2 py-1.5 text-sm disabled:opacity-50"
                                />
                            </div>
                        </div>
                        <p className="mt-1 text-[10px] leading-snug text-gray-500 dark:text-gray-400">
                            {t('aerosync.sync.bandwidthHint') || '0 = unlimited. The local mirror loop throttles using the higher of the two caps.'}
                        </p>
                    </div>
                )}
            </div>

            {running && progress && (
                <div className="p-3 rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-700/50">
                    <TransferProgressBar
                        percentage={Math.round(pct)}
                        filename={progress.current_path}
                        currentFile={progress.processed}
                        totalFiles={progress.total}
                        size="md"
                        variant={pct > 0 ? 'gradient' : 'indeterminate'}
                    />
                    <div className="mt-1.5 text-xs text-gray-500 dark:text-gray-400 truncate">
                        {progress.used_delta ? '[delta] ' : '[copy] '}
                        {progress.current_path}
                    </div>
                </div>
            )}

            {error && (
                <div className="p-3 rounded-lg border border-red-200 bg-red-50 dark:bg-red-900/30 dark:border-red-800 text-red-700 dark:text-red-300 text-sm flex items-start gap-2">
                    <AlertTriangle size={16} className="flex-shrink-0 mt-0.5" />
                    <span>{error}</span>
                </div>
            )}

            {report && (
                <div className="p-3 rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-700/50">
                    <div className="flex items-center gap-2 mb-2">
                        {report.status === 'ok' ? (
                            <CheckCircle2 size={16} className="text-green-500" />
                        ) : report.status === 'cancelled' ? (
                            <Ban size={16} className="text-amber-500" />
                        ) : (
                            <AlertTriangle size={16} className="text-amber-500" />
                        )}
                        <span className="font-semibold text-sm">
                            {report.status === 'ok'
                                ? t('aerosync.sync.resultOk') || 'Sync completed'
                                : report.status === 'cancelled'
                                    ? t('aerosync.sync.resultCancelled') || 'Sync cancelled'
                                    : t('aerosync.sync.resultPartial') || 'Sync completed with errors'}
                        </span>
                    </div>
                    <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs text-gray-700 dark:text-gray-300">
                        <dt>{t('aerosync.sync.uploaded') || 'Uploaded'}</dt>
                        <dd className="font-mono">{report.uploaded}</dd>
                        <dt>{t('aerosync.sync.skipped') || 'Skipped'}</dt>
                        <dd className="font-mono">{report.skipped}</dd>
                        <dt>{t('aerosync.sync.errors') || 'Errors'}</dt>
                        <dd className="font-mono">{report.errors}</dd>
                        <dt>{t('aerosync.sync.elapsed') || 'Elapsed'}</dt>
                        <dd className="font-mono">{report.elapsed_ms} ms</dd>
                        <dt>{t('aerosync.sync.payload') || 'Payload'}</dt>
                        <dd className="font-mono">{formatBytes(report.total_payload_bytes)}</dd>
                        <dt>{t('aerosync.sync.bytesOnWire') || 'Bytes on wire'}</dt>
                        <dd className="font-mono">{formatBytes(report.bytes_on_wire)}</dd>
                        <dt>{t('aerosync.sync.savings') || 'Savings'}</dt>
                        <dd className="font-mono">
                            {((1 - report.savings_ratio) * 100).toFixed(2)}%
                        </dd>
                    </dl>
                    {report.error_messages.length > 0 && (
                        <details className="mt-2">
                            <summary className="text-xs cursor-pointer text-red-600 dark:text-red-400">
                                {report.error_messages.length}{' '}
                                {t('aerosync.sync.errorDetails') || 'error(s), details'}
                            </summary>
                            <ul className="mt-1 text-xs text-red-700 dark:text-red-300 max-h-32 overflow-y-auto list-disc pl-5">
                                {report.error_messages.map((m, i) => (
                                    <li key={i}>{m}</li>
                                ))}
                            </ul>
                        </details>
                    )}
                    {/* CO-2: per-file results table. Source of truth is
                        the LocalSyncReport.entries array (capped at
                        5000 rows by the backend). On dry-run all rows
                        carry the `dry-run` action; the placeholder
                        below covers the empty case. */}
                    <details className="mt-2" open>
                        <summary className="text-xs cursor-pointer text-gray-700 dark:text-gray-300 font-semibold">
                            {t('aerosync.sync.resultsTable') || 'Per-file results'}
                            {report.entries_truncated && (
                                <span className="ml-1 rounded bg-amber-100 px-1 py-0.5 text-[10px] font-semibold text-amber-700 dark:bg-amber-900/40 dark:text-amber-300">
                                    {t('aerosync.sync.resultsTruncated') || 'TRUNCATED'}
                                </span>
                            )}
                        </summary>
                        {report.entries && report.entries.length > 0 ? (
                            <div className="mt-1 max-h-64 overflow-y-auto rounded border border-gray-200 dark:border-gray-700">
                                <table className="w-full text-[11px]">
                                    <thead className="sticky top-0 bg-gray-100 dark:bg-gray-800 text-left text-gray-600 dark:text-gray-300">
                                        <tr>
                                            <th className="px-2 py-1 font-medium">
                                                {t('aerosync.sync.resultName') || 'Name'}
                                            </th>
                                            <th className="px-2 py-1 font-medium">
                                                {t('aerosync.sync.resultAction') || 'Action'}
                                            </th>
                                            <th className="px-2 py-1 font-medium text-right">
                                                {t('aerosync.sync.resultBytes') || 'Bytes'}
                                            </th>
                                            <th className="px-2 py-1 font-medium">
                                                {t('aerosync.sync.resultStatus') || 'Status'}
                                            </th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {report.entries.map((e, i) => (
                                            <tr
                                                key={i}
                                                className={`border-t border-gray-100 dark:border-gray-700/60 ${
                                                    e.status === 'error'
                                                        ? 'bg-rose-50/60 dark:bg-rose-900/20'
                                                        : ''
                                                }`}
                                            >
                                                <td className="px-2 py-1 font-mono text-gray-800 dark:text-gray-200 break-all">
                                                    {e.name}
                                                </td>
                                                <td className="px-2 py-1 text-gray-700 dark:text-gray-300">
                                                    {e.action}
                                                </td>
                                                <td className="px-2 py-1 text-right font-mono text-gray-600 dark:text-gray-300">
                                                    {e.bytes > 0 ? formatBytes(e.bytes) : '-'}
                                                </td>
                                                <td className={`px-2 py-1 ${
                                                    e.status === 'error'
                                                        ? 'text-rose-600 dark:text-rose-300'
                                                        : e.status === 'skipped'
                                                            ? 'text-gray-500 dark:text-gray-400'
                                                            : 'text-emerald-600 dark:text-emerald-300'
                                                }`}>
                                                    {e.status}
                                                </td>
                                            </tr>
                                        ))}
                                    </tbody>
                                </table>
                            </div>
                        ) : (
                            <p className="mt-1 text-xs text-gray-500 dark:text-gray-400 italic">
                                {dryRun
                                    ? (t('aerosync.sync.resultsDryRun') || 'Dry run preview only.')
                                    : (t('aerosync.sync.resultsEmpty') || 'No files processed.')}
                            </p>
                        )}
                    </details>
                </div>
            )}

            <div className="flex justify-end gap-2 pt-2">
                {running && (
                    <button
                        type="button"
                        onClick={cancelRun}
                        disabled={cancelling}
                        className="px-4 py-2 bg-red-500 hover:bg-red-600 text-white rounded-lg text-sm font-medium flex items-center gap-1.5 transition-colors disabled:opacity-50"
                    >
                        <CircleStop size={16} />
                        {cancelling
                            ? t('aerosync.sync.stopping') || 'Stopping...'
                            : t('aerosync.sync.stop') || 'Stop'}
                    </button>
                )}
                <button
                    type="button"
                    onClick={start}
                    disabled={running || !source || !destination}
                    className="px-4 py-2 bg-blue-500 hover:bg-blue-600 text-white rounded-lg text-sm font-medium flex items-center gap-1.5 transition-colors disabled:opacity-50"
                >
                    <Play size={16} />
                    {running
                        ? t('aerosync.sync.running') || 'Running...'
                        : t('aerosync.sync.start') || 'Start sync'}
                </button>
            </div>
        </div>
    );
};

export default SyncTabContent;
