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
import { open } from '@tauri-apps/plugin-dialog';
import { FolderOpen, Play, AlertTriangle, CheckCircle2 } from 'lucide-react';
import { useTranslation } from '../../i18n';

interface SyncTabContentProps {
    initialSource?: string;
    initialDestination?: string;
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
}) => {
    const t = useTranslation();
    const [source, setSource] = useState(initialSource);
    const [destination, setDestination] = useState(initialDestination);
    const [exclude, setExclude] = useState('');
    const [useDelta, setUseDelta] = useState(true);
    const [dryRun, setDryRun] = useState(false);
    const [running, setRunning] = useState(false);
    const [progress, setProgress] = useState<LocalSyncProgress | null>(null);
    const [report, setReport] = useState<LocalSyncReport | null>(null);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        if (initialSource) setSource(initialSource);
        if (initialDestination) setDestination(initialDestination);
    }, [initialSource, initialDestination]);

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
            const sel = await open({ directory: true, multiple: false });
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
            setError(t('localSync.errorPathsRequired') || 'Source and destination are required');
            return;
        }
        setRunning(true);
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
                },
            });
            setReport(result);
        } catch (e) {
            setError(String(e));
        } finally {
            setRunning(false);
        }
    };

    const pct = progress && progress.total > 0 ? (progress.processed / progress.total) * 100 : 0;

    return (
        <div className="p-4 space-y-4">
            <p className="text-sm text-gray-600 dark:text-gray-400">
                {t('localSync.description') ||
                    'Mirror a local source directory to a local destination. Files >= 1 MiB route through the AeroRsync delta engine; smaller files use plain copy.'}
            </p>

            <div>
                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1.5">
                    {t('localSync.source') || 'Source'}
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
                        {t('localSync.browse') || 'Browse...'}
                    </button>
                </div>
            </div>

            <div>
                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1.5">
                    {t('localSync.destination') || 'Destination'}
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
                        {t('localSync.browse') || 'Browse...'}
                    </button>
                </div>
            </div>

            <div>
                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1.5">
                    {t('localSync.excludeLabel') || 'Exclude patterns (comma or newline separated)'}
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

            <div className="flex items-center gap-6 pt-1">
                <label className="flex items-center gap-2 text-sm text-gray-700 dark:text-gray-300 cursor-pointer">
                    <input
                        type="checkbox"
                        checked={useDelta}
                        onChange={(e) => setUseDelta(e.target.checked)}
                        disabled={running}
                        className="rounded border-gray-300 dark:border-gray-600"
                    />
                    {t('localSync.useDelta') || 'Use AeroRsync delta transport (>= 1 MiB files)'}
                </label>
                <label className="flex items-center gap-2 text-sm text-gray-700 dark:text-gray-300 cursor-pointer">
                    <input
                        type="checkbox"
                        checked={dryRun}
                        onChange={(e) => setDryRun(e.target.checked)}
                        disabled={running}
                        className="rounded border-gray-300 dark:border-gray-600"
                    />
                    {t('localSync.dryRun') || 'Dry run (preview only)'}
                </label>
            </div>

            {running && progress && (
                <div className="p-3 rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-700/50">
                    <div className="flex items-center justify-between text-xs text-gray-600 dark:text-gray-400 mb-1.5">
                        <span>
                            {progress.processed} / {progress.total}
                        </span>
                        <span className="font-mono">{pct.toFixed(0)}%</span>
                    </div>
                    <div className="h-2 bg-gray-200 dark:bg-gray-600 rounded-lg overflow-hidden">
                        <div
                            className="h-full bg-blue-500 transition-all"
                            style={{ width: `${pct}%` }}
                        />
                    </div>
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
                        ) : (
                            <AlertTriangle size={16} className="text-amber-500" />
                        )}
                        <span className="font-semibold text-sm">
                            {report.status === 'ok'
                                ? t('localSync.resultOk') || 'Sync completed'
                                : t('localSync.resultPartial') || 'Sync completed with errors'}
                        </span>
                    </div>
                    <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs text-gray-700 dark:text-gray-300">
                        <dt>{t('localSync.uploaded') || 'Uploaded'}</dt>
                        <dd className="font-mono">{report.uploaded}</dd>
                        <dt>{t('localSync.skipped') || 'Skipped'}</dt>
                        <dd className="font-mono">{report.skipped}</dd>
                        <dt>{t('localSync.errors') || 'Errors'}</dt>
                        <dd className="font-mono">{report.errors}</dd>
                        <dt>{t('localSync.elapsed') || 'Elapsed'}</dt>
                        <dd className="font-mono">{report.elapsed_ms} ms</dd>
                        <dt>{t('localSync.payload') || 'Payload'}</dt>
                        <dd className="font-mono">{formatBytes(report.total_payload_bytes)}</dd>
                        <dt>{t('localSync.bytesOnWire') || 'Bytes on wire'}</dt>
                        <dd className="font-mono">{formatBytes(report.bytes_on_wire)}</dd>
                        <dt>{t('localSync.savings') || 'Savings'}</dt>
                        <dd className="font-mono">
                            {((1 - report.savings_ratio) * 100).toFixed(2)}%
                        </dd>
                    </dl>
                    {report.error_messages.length > 0 && (
                        <details className="mt-2">
                            <summary className="text-xs cursor-pointer text-red-600 dark:text-red-400">
                                {report.error_messages.length}{' '}
                                {t('localSync.errorDetails') || 'error(s), details'}
                            </summary>
                            <ul className="mt-1 text-xs text-red-700 dark:text-red-300 max-h-32 overflow-y-auto list-disc pl-5">
                                {report.error_messages.map((m, i) => (
                                    <li key={i}>{m}</li>
                                ))}
                            </ul>
                        </details>
                    )}
                </div>
            )}

            <div className="flex justify-end pt-2">
                <button
                    type="button"
                    onClick={start}
                    disabled={running || !source || !destination}
                    className="px-4 py-2 bg-blue-500 hover:bg-blue-600 text-white rounded-lg text-sm font-medium flex items-center gap-1.5 transition-colors disabled:opacity-50"
                >
                    <Play size={16} />
                    {running
                        ? t('localSync.running') || 'Running...'
                        : t('localSync.start') || 'Start sync'}
                </button>
            </div>
        </div>
    );
};

export default SyncTabContent;
