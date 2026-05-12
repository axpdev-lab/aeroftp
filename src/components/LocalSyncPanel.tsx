// Z.2.3: AeroSync UI for local-to-local sync.
//
// Minimal dedicated panel that walks a source directory and mirrors it to
// a destination directory in-process, routing files >= 1 MiB through
// `LocalDeltaTransport` (in-process delta engine, no SSH). Falls through
// to plain copy for smaller files and on transport errors.
//
// Backend command: `local_sync_run` (see src-tauri/src/local_sync.rs).
// Progress event: `local-sync-progress`.

import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { FolderOpen, Play, X, AlertTriangle, CheckCircle2, Folder } from 'lucide-react';
import { useTranslation } from '../i18n';

interface LocalSyncPanelProps {
    isOpen: boolean;
    onClose: () => void;
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

export const LocalSyncPanel: React.FC<LocalSyncPanelProps> = ({
    isOpen,
    onClose,
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
        if (isOpen) {
            listen<LocalSyncProgress>('local-sync-progress', (e) => {
                setProgress(e.payload);
            }).then((fn) => {
                unlisten = fn;
            });
        }
        return () => {
            if (unlisten) unlisten();
        };
    }, [isOpen]);

    if (!isOpen) return null;

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
        <div
            className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/50 animate-scale-in"
            onClick={onClose}
        >
            <div
                className="bg-white dark:bg-gray-900 rounded-lg shadow-xl max-w-2xl w-[92%] max-h-[90vh] overflow-hidden flex flex-col"
                onClick={(e) => e.stopPropagation()}
            >
                <div className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <Folder className="w-5 h-5 text-blue-500" />
                        <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">
                            {t('localSync.title') || 'AeroSync: Local to Local'}
                        </h2>
                    </div>
                    <button
                        type="button"
                        onClick={onClose}
                        disabled={running}
                        className="p-1 rounded hover:bg-gray-100 dark:hover:bg-gray-800 disabled:opacity-50"
                        aria-label="Close"
                    >
                        <X className="w-5 h-5" />
                    </button>
                </div>

                <div className="px-5 py-4 flex-1 overflow-y-auto">
                    <p className="text-sm text-gray-600 dark:text-gray-400 mb-4">
                        {t('localSync.description') ||
                            'Mirror a local source directory to a local destination. Files ≥ 1 MiB use the in-process delta engine; smaller files use plain copy.'}
                    </p>

                    <label className="block text-xs font-semibold uppercase text-gray-500 mb-1">
                        {t('localSync.source') || 'Source'}
                    </label>
                    <div className="flex gap-2 mb-3">
                        <input
                            type="text"
                            value={source}
                            onChange={(e) => setSource(e.target.value)}
                            disabled={running}
                            className="flex-1 px-3 py-2 border rounded text-sm bg-white dark:bg-gray-800 border-gray-300 dark:border-gray-600 text-gray-900 dark:text-gray-100 disabled:opacity-50"
                            placeholder="/path/to/source"
                        />
                        <button
                            type="button"
                            onClick={() => pickFolder('source')}
                            disabled={running}
                            className="px-3 py-2 rounded bg-blue-500 hover:bg-blue-600 text-white text-sm flex items-center gap-1 disabled:opacity-50"
                        >
                            <FolderOpen className="w-4 h-4" />
                            {t('localSync.browse') || 'Browse…'}
                        </button>
                    </div>

                    <label className="block text-xs font-semibold uppercase text-gray-500 mb-1">
                        {t('localSync.destination') || 'Destination'}
                    </label>
                    <div className="flex gap-2 mb-3">
                        <input
                            type="text"
                            value={destination}
                            onChange={(e) => setDestination(e.target.value)}
                            disabled={running}
                            className="flex-1 px-3 py-2 border rounded text-sm bg-white dark:bg-gray-800 border-gray-300 dark:border-gray-600 text-gray-900 dark:text-gray-100 disabled:opacity-50"
                            placeholder="/path/to/destination"
                        />
                        <button
                            type="button"
                            onClick={() => pickFolder('destination')}
                            disabled={running}
                            className="px-3 py-2 rounded bg-blue-500 hover:bg-blue-600 text-white text-sm flex items-center gap-1 disabled:opacity-50"
                        >
                            <FolderOpen className="w-4 h-4" />
                            {t('localSync.browse') || 'Browse…'}
                        </button>
                    </div>

                    <label className="block text-xs font-semibold uppercase text-gray-500 mb-1">
                        {t('localSync.excludeLabel') || 'Exclude patterns (comma or newline separated)'}
                    </label>
                    <input
                        type="text"
                        value={exclude}
                        onChange={(e) => setExclude(e.target.value)}
                        disabled={running}
                        placeholder="*.tmp, .git, node_modules"
                        className="w-full px-3 py-2 border rounded text-sm bg-white dark:bg-gray-800 border-gray-300 dark:border-gray-600 text-gray-900 dark:text-gray-100 disabled:opacity-50 mb-3"
                    />

                    <div className="flex items-center gap-4 mb-4">
                        <label className="flex items-center gap-2 text-sm text-gray-700 dark:text-gray-300">
                            <input
                                type="checkbox"
                                checked={useDelta}
                                onChange={(e) => setUseDelta(e.target.checked)}
                                disabled={running}
                            />
                            {t('localSync.useDelta') || 'Use delta transport (≥ 1 MiB files)'}
                        </label>
                        <label className="flex items-center gap-2 text-sm text-gray-700 dark:text-gray-300">
                            <input
                                type="checkbox"
                                checked={dryRun}
                                onChange={(e) => setDryRun(e.target.checked)}
                                disabled={running}
                            />
                            {t('localSync.dryRun') || 'Dry run (preview only)'}
                        </label>
                    </div>

                    {running && progress && (
                        <div className="mb-4">
                            <div className="flex items-center justify-between text-xs text-gray-600 dark:text-gray-400 mb-1">
                                <span>
                                    {progress.processed} / {progress.total}
                                </span>
                                <span>{pct.toFixed(0)}%</span>
                            </div>
                            <div className="h-2 bg-gray-200 dark:bg-gray-700 rounded overflow-hidden">
                                <div
                                    className="h-full bg-blue-500 transition-all"
                                    style={{ width: `${pct}%` }}
                                />
                            </div>
                            <div className="mt-1 text-xs text-gray-500 truncate">
                                {progress.used_delta ? '[delta] ' : '[copy] '}
                                {progress.current_path}
                            </div>
                        </div>
                    )}

                    {error && (
                        <div className="mb-3 p-3 rounded border border-red-200 bg-red-50 dark:bg-red-900/30 dark:border-red-800 text-red-700 dark:text-red-300 text-sm flex items-start gap-2">
                            <AlertTriangle className="w-4 h-4 flex-shrink-0 mt-0.5" />
                            <span>{error}</span>
                        </div>
                    )}

                    {report && (
                        <div className="mt-3 p-3 rounded border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800">
                            <div className="flex items-center gap-2 mb-2">
                                {report.status === 'ok' ? (
                                    <CheckCircle2 className="w-4 h-4 text-green-500" />
                                ) : (
                                    <AlertTriangle className="w-4 h-4 text-amber-500" />
                                )}
                                <span className="font-semibold text-sm text-gray-900 dark:text-gray-100">
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
                                <dd className="font-mono">
                                    {formatBytes(report.total_payload_bytes)}
                                </dd>
                                <dt>{t('localSync.bytesOnWire') || 'Bytes on wire'}</dt>
                                <dd className="font-mono">
                                    {formatBytes(report.bytes_on_wire)}
                                </dd>
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
                </div>

                <div className="px-5 py-3 border-t border-gray-200 dark:border-gray-700 flex justify-end gap-2">
                    <button
                        type="button"
                        onClick={onClose}
                        disabled={running}
                        className="px-4 py-2 rounded text-sm bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 disabled:opacity-50"
                    >
                        {t('common.close') || 'Close'}
                    </button>
                    <button
                        type="button"
                        onClick={start}
                        disabled={running || !source || !destination}
                        className="px-4 py-2 rounded text-sm bg-blue-500 hover:bg-blue-600 text-white flex items-center gap-1 disabled:opacity-50"
                    >
                        <Play className="w-4 h-4" />
                        {running
                            ? t('localSync.running') || 'Running…'
                            : t('localSync.start') || 'Start sync'}
                    </button>
                </div>
            </div>
        </div>
    );
};
