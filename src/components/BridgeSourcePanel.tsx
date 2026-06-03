// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import {
    Upload, Download, AlertCircle, CheckCircle2, Lock, RefreshCw,
    FolderInput, Shield, Info,
} from 'lucide-react';
import { ServerProfile } from '../types';
import { useTranslation } from '../i18n';
import { Checkbox } from './ui/Checkbox';
import { BridgeSourceDescriptor, BridgeSourceMeta } from './bridge/bridgeSources';
import { commitImportedServers } from './bridge/bridgeImportCommit';

interface ImportedServer {
    id: string;
    name: string;
    host: string;
    port: number;
    username: string;
    protocol?: string;
    initialPath?: string;
    options?: Record<string, unknown>;
    providerId?: string;
    hasStoredCredential?: boolean;
}

interface BridgeImportResult {
    servers: ImportedServer[];
    skipped: Array<{ name: string; reason: string }>;
    sourcePath: string | null;
    totalRemotes: number;
}

interface BridgeExportResult {
    exported: number;
    total: number;
    skipped: Array<{ name: string; reason: string }>;
}

interface Props {
    source: BridgeSourceDescriptor;
    direction: 'import' | 'export';
    servers: ServerProfile[];
    existingServerKeys: Set<string>;
    onImport: (servers: ServerProfile[]) => void;
    onClose: () => void;
    onBack: () => void;
    /** A profile file dragged onto the import dialog and identified as
     *  this source. When set, skip auto-detect/browse and scan it
     *  directly so drag-and-drop lands straight on the preview. */
    presetFilePath?: string;
}

const DEFAULT_EXPORT_NAME: Record<string, string> = {
    aws: 'credentials', ssh: 'config', mc: 'config', s3cmd: 's3cmd',
    cyberduck: 'bookmark', lftp: 'lftp', putty: 'putty-sessions',
    mobaxterm: 'MobaXterm', dreamweaver: 'site', kopia: 'repository',
    duplicacy: 'preferences', restic: 'restic-env',
};

export const BridgeSourcePanel: React.FC<Props> = ({
    source, direction, servers, existingServerKeys, onImport, onClose, onBack,
    presetFilePath,
}) => {
    const t = useTranslation();
    const app = source.label;

    const [meta, setMeta] = useState<BridgeSourceMeta | null>(null);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [success, setSuccess] = useState<string | null>(null);

    // import state
    const [detectedPath, setDetectedPath] = useState<string | null>(null);
    const [result, setResult] = useState<BridgeImportResult | null>(null);
    const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());

    // export state
    const [includeCredentials, setIncludeCredentials] = useState(true);
    const [exportSelectedIds, setExportSelectedIds] = useState<Set<string>>(new Set());
    const [exportResult, setExportResult] = useState<BridgeExportResult | null>(null);

    useEffect(() => {
        invoke<BridgeSourceMeta>('bridge_source_meta', { source: source.id })
            .then(setMeta)
            .catch(e => setError(String(e)));
    }, [source.id]);

    // Auto-detect the config file for imports. Skipped when a dropped
    // file was already identified for this source (presetFilePath wins).
    useEffect(() => {
        if (direction === 'import' && detectedPath === null && !presetFilePath) {
            invoke<string | null>('detect_bridge_config', { source: source.id })
                .then(p => setDetectedPath(p || ''))
                .catch(() => setDetectedPath(''));
        }
    }, [direction, source.id, detectedPath, presetFilePath]);

    const presetHandledRef = useRef<string | null>(null);

    // Pre-select exportable profiles (protocol supported by the target).
    const supported = meta?.supportedProtocols ?? [];
    const exportable = useMemo(
        () => servers.filter(s => supported.includes(s.protocol || 'ftp')),
        [servers, supported],
    );
    useEffect(() => {
        if (direction === 'export' && meta) {
            setExportSelectedIds(new Set(exportable.map(s => s.id)));
        }
    }, [direction, meta, exportable]);

    const secretNote = () => {
        if (!meta) return null;
        const k = meta.secretPolicy === 'full' ? 'settings.bridgeSecretFull'
            : meta.secretPolicy === 'limited' ? 'settings.bridgeSecretLimited'
            : 'settings.bridgeSecretMetadata';
        const tone = meta.secretPolicy === 'full'
            ? 'bg-green-50 dark:bg-green-900/20 border-green-200 dark:border-green-800 text-green-700 dark:text-green-300'
            : meta.secretPolicy === 'limited'
            ? 'bg-amber-50 dark:bg-amber-900/20 border-amber-200 dark:border-amber-800 text-amber-700 dark:text-amber-300'
            : 'bg-blue-50 dark:bg-blue-900/20 border-blue-200 dark:border-blue-800 text-blue-700 dark:text-blue-300';
        return (
            <div className={`flex items-start gap-2 p-3 border rounded-lg ${tone}`}>
                {meta.secretPolicy === 'full'
                    ? <Shield size={16} className="mt-0.5 flex-shrink-0" />
                    : <Info size={16} className="mt-0.5 flex-shrink-0" />}
                <div className="text-xs">{t(k).replace('{app}', app)}</div>
            </div>
        );
    };

    // ---- Import ----

    const runScan = async (filePath: string) => {
        if (!filePath) return;
        setLoading(true);
        setError(null);
        setResult(null);
        try {
            const r = await invoke<BridgeImportResult>('import_bridge_config', {
                source: source.id, filePath,
            });
            setResult(r);
            setSelectedIds(new Set(r.servers.map(s => s.id)));
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    // Drag-and-drop entry point: a file identified as this source is
    // scanned directly, skipping detect/browse. Guarded so it runs once
    // per dropped path.
    useEffect(() => {
        if (
            direction === 'import' &&
            presetFilePath &&
            presetHandledRef.current !== presetFilePath
        ) {
            presetHandledRef.current = presetFilePath;
            setDetectedPath(presetFilePath);
            void runScan(presetFilePath);
        }
        // runScan is a stable closure over source.id for this panel.
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [direction, presetFilePath]);

    const browse = async () => {
        const exts = meta?.importExtensions?.length ? meta.importExtensions : ['*'];
        const filePath = await open({
            title: t('settings.bridgeSelectConfig').replace('{app}', app),
            filters: [
                { name: meta?.importFilterName || app, extensions: exts },
                { name: 'All Files', extensions: ['*'] },
            ],
            multiple: false,
        });
        if (!filePath || typeof filePath !== 'string') return;
        setDetectedPath(filePath);
        await runScan(filePath);
    };

    const confirmImport = async () => {
        if (!result) return;
        const selected: ServerProfile[] = result.servers
            .filter(s => selectedIds.has(s.id))
            .map(s => ({
                id: s.id, name: s.name, host: s.host, port: s.port,
                username: s.username,
                protocol: s.protocol as ServerProfile['protocol'],
                initialPath: s.initialPath,
                options: s.options as ServerProfile['options'],
                providerId: s.providerId,
                hasStoredCredential: s.hasStoredCredential || false,
            }));
        if (selected.length === 0) return;
        const outcome = await commitImportedServers(selected, existingServerKeys, onImport);
        if (outcome.error) { setError(outcome.error); return; }
        const parts: string[] = [];
        if (outcome.added > 0) parts.push(t('settings.importSuccess').replace('{count}', String(outcome.added)));
        if (outcome.updated > 0) parts.push(t('settings.serversUpdated').replace('{count}', String(outcome.updated)));
        setSuccess(parts.join(', '));
        setTimeout(() => onClose(), 2500);
    };

    const toggle = (id: string, set: React.Dispatch<React.SetStateAction<Set<string>>>) =>
        set(prev => {
            const next = new Set(prev);
            if (next.has(id)) next.delete(id); else next.add(id);
            return next;
        });

    // ---- Export ----

    const runExport = async () => {
        const chosen = exportable.filter(s => exportSelectedIds.has(s.id));
        if (chosen.length === 0) return;
        const ext = meta?.exportExt || 'txt';
        const filePath = await save({
            title: t('settings.bridgeExportTitle').replace('{app}', app),
            filters: [{ name: meta?.exportLabel || app, extensions: [ext] }],
            defaultPath: `${DEFAULT_EXPORT_NAME[source.id] || source.id}.${ext}`,
        });
        if (!filePath) return;
        setLoading(true);
        setError(null);
        try {
            const r = await invoke<BridgeExportResult>('export_bridge_config', {
                source: source.id,
                serversJson: JSON.stringify(chosen),
                includeCredentials,
                filePath,
            });
            setExportResult(r);
            setSuccess(t('settings.bridgeExportSuccess').replace('{count}', String(r.exported)));
            setTimeout(() => onClose(), 2500);
        } catch (e) {
            setError(String(e));
        } finally {
            setLoading(false);
        }
    };

    const backBtn = (
        <button onClick={onBack} className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700">
            {t('common.back')}
        </button>
    );
    const alerts = (
        <>
            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}
        </>
    );

    if (direction === 'import') {
        return (
            <div className="space-y-4">
                {secretNote()}
                {!result ? (
                    detectedPath === null ? (
                        <div className="flex items-center justify-center py-6">
                            <RefreshCw size={20} className="animate-spin text-gray-400" />
                            <span className="ml-2 text-sm text-gray-500">{t('settings.bridgeDetecting')}</span>
                        </div>
                    ) : (
                        <div className="space-y-3">
                            {detectedPath ? (
                                <div className="p-3 bg-gray-50 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 rounded-lg">
                                    <div className="text-xs text-gray-500 dark:text-gray-400 mb-1">{t('settings.bridgeConfigFound')}</div>
                                    <div className="text-sm font-mono truncate" title={detectedPath}>{detectedPath}</div>
                                </div>
                            ) : (
                                <div className="p-3 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 rounded-lg">
                                    <div className="text-sm text-blue-700 dark:text-blue-300 flex items-center gap-2">
                                        <AlertCircle size={14} />{t('settings.bridgeNotFound')}
                                    </div>
                                    <div className="text-xs text-blue-600 dark:text-blue-400 mt-1">{t('settings.bridgeNotFoundHint')}</div>
                                </div>
                            )}
                            <div className="flex gap-2">
                                {detectedPath && (
                                    <button
                                        onClick={() => runScan(detectedPath)}
                                        disabled={loading}
                                        className="flex-1 px-4 py-2 text-sm bg-cyan-600 text-white rounded-lg hover:bg-cyan-700 disabled:opacity-50 flex items-center justify-center gap-2"
                                    >
                                        {loading ? <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" /> : <FolderInput size={16} />}
                                        {loading ? t('settings.bridgeScanning') : t('settings.bridgeScanConfig')}
                                    </button>
                                )}
                                <button
                                    onClick={browse}
                                    disabled={loading}
                                    className={`px-4 py-2 text-sm rounded-lg disabled:opacity-50 ${detectedPath ? 'border border-gray-300 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700' : 'flex-1 bg-cyan-600 text-white hover:bg-cyan-700 flex items-center justify-center gap-2'}`}
                                >
                                    {!detectedPath && <FolderInput size={16} />}
                                    {t('settings.bridgeBrowse')}
                                </button>
                            </div>
                        </div>
                    )
                ) : (
                    <>
                        <div className="text-sm text-gray-600 dark:text-gray-300">
                            {t('settings.bridgeFound')
                                .replace('{total}', String(result.totalRemotes))
                                .replace('{supported}', String(result.servers.length))}
                        </div>
                        {result.servers.length > 0 && (
                            <div>
                                <div className="flex items-center justify-between mb-2">
                                    <span className="text-sm font-medium text-gray-700 dark:text-gray-300">{t('settings.bridgeSelectProfiles')}</span>
                                    <button
                                        onClick={() => setSelectedIds(selectedIds.size === result.servers.length ? new Set() : new Set(result.servers.map(s => s.id)))}
                                        className="text-xs text-blue-500 hover:text-blue-600 font-medium"
                                    >
                                        {selectedIds.size === result.servers.length ? t('settings.deselectAll') : t('settings.selectAll')}
                                    </button>
                                </div>
                                <div className="border border-gray-200 dark:border-gray-600 rounded-lg max-h-[200px] overflow-y-auto">
                                    {result.servers.map(s => {
                                        const dup = existingServerKeys.has(`${s.host}:${s.port}:${s.username}`);
                                        return (
                                            <div key={s.id}
                                                className={`flex items-center gap-3 px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer border-b border-gray-100 dark:border-gray-700 last:border-b-0 ${dup ? 'opacity-50' : ''}`}
                                                onClick={() => toggle(s.id, setSelectedIds)}>
                                                <Checkbox checked={selectedIds.has(s.id)} onChange={() => toggle(s.id, setSelectedIds)} />
                                                <div className="min-w-0 flex-1">
                                                    <div className="text-sm font-medium truncate flex items-center gap-1.5">
                                                        {s.name}
                                                        {dup && <span className="text-[9px] px-1.5 py-0.5 rounded bg-amber-100 dark:bg-amber-900/30 text-amber-700 dark:text-amber-400 font-medium whitespace-nowrap">{t('settings.alreadyExists')}</span>}
                                                    </div>
                                                    <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                                        {s.host}{s.port && s.port !== 443 ? `:${s.port}` : ''}{s.username ? ` - ${s.username}` : ''}
                                                    </div>
                                                </div>
                                                <div className="flex items-center gap-1.5 flex-shrink-0">
                                                    {s.hasStoredCredential && <Lock size={12} className="text-green-500" />}
                                                    <span className="text-[10px] text-gray-400 uppercase">{(s.protocol || 'ftp').toUpperCase()}</span>
                                                </div>
                                            </div>
                                        );
                                    })}
                                </div>
                                <div className="flex items-center justify-between gap-2 mt-1">
                                    <span className="text-xs text-gray-500 dark:text-gray-400 flex-shrink-0">
                                        {selectedIds.size} / {result.servers.length} {t('settings.selected')}
                                    </span>
                                    {(result.sourcePath || detectedPath) && (
                                        <span
                                            className="text-[11px] font-mono text-gray-400 dark:text-gray-500 truncate flex items-center gap-1 min-w-0"
                                            title={result.sourcePath || detectedPath || ''}
                                        >
                                            <FolderInput size={11} className="flex-shrink-0" />
                                            {/* dir=rtl left-truncates to keep the filename visible;
                                                the leading U+200E (LRM) stops bidi from relocating
                                                the path's leading slash to the trailing edge. */}
                                            <span className="truncate" dir="rtl">{'\u200e'}{result.sourcePath || detectedPath}</span>
                                        </span>
                                    )}
                                </div>
                            </div>
                        )}
                        {result.skipped.length > 0 && (
                            <div>
                                <div className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">
                                    {t('settings.bridgeSkipped')} ({result.skipped.length})
                                </div>
                                <div className="text-xs text-gray-400 dark:text-gray-500 space-y-0.5 max-h-[110px] overflow-y-auto">
                                    {result.skipped.map((s, i) => (
                                        <div key={i} className="truncate">
                                            <span className="font-medium">{s.name}</span><span className="mx-1">-</span><span>{s.reason}</span>
                                        </div>
                                    ))}
                                </div>
                            </div>
                        )}
                    </>
                )}
                {alerts}
                <div className="flex gap-2">
                    {backBtn}
                    {result && result.servers.length > 0 && (
                        <button
                            onClick={confirmImport}
                            disabled={selectedIds.size === 0}
                            className="flex-1 px-4 py-2 text-sm bg-cyan-600 text-white rounded-lg hover:bg-cyan-700 disabled:opacity-50 flex items-center justify-center gap-2"
                        >
                            <Upload size={16} />
                            {t('settings.bridgeImportSelected').replace('{count}', String(selectedIds.size))}
                        </button>
                    )}
                </div>
            </div>
        );
    }

    // ---- Export view ----
    const unsupportedCount = servers.length - exportable.length;
    return (
        <div className="space-y-4">
            {secretNote()}
            <div className="text-sm text-gray-600 dark:text-gray-300">
                {t('settings.bridgeExportNotice').replace('{app}', app)}
            </div>
            <div>
                <div className="flex items-center justify-between mb-2">
                    <span className="text-sm font-medium text-gray-700 dark:text-gray-300">{t('settings.selectServersToExport')}</span>
                    {exportable.length > 0 && (
                        <button
                            onClick={() => setExportSelectedIds(exportSelectedIds.size === exportable.length ? new Set() : new Set(exportable.map(s => s.id)))}
                            className="text-xs text-blue-500 hover:text-blue-600 font-medium"
                        >
                            {exportSelectedIds.size === exportable.length ? t('settings.deselectAll') : t('settings.selectAll')}
                        </button>
                    )}
                </div>
                <div className="border border-gray-200 dark:border-gray-600 rounded-lg max-h-[220px] overflow-y-auto">
                    {servers.map(s => {
                        const ok = supported.includes(s.protocol || 'ftp');
                        return (
                            <div key={s.id}
                                className={`flex items-center gap-3 px-3 py-2 border-b border-gray-100 dark:border-gray-700 last:border-b-0 ${ok ? 'hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer' : 'opacity-50'}`}
                                onClick={() => ok && toggle(s.id, setExportSelectedIds)}>
                                <Checkbox checked={ok && exportSelectedIds.has(s.id)} disabled={!ok} onChange={() => ok && toggle(s.id, setExportSelectedIds)} />
                                <div className="w-2 h-2 rounded-full flex-shrink-0" style={{ backgroundColor: s.color || '#6B7280' }} />
                                <div className="min-w-0 flex-1">
                                    <div className="text-sm font-medium truncate">{s.name}</div>
                                    <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                        {ok ? `${s.host}:${s.port} - ${s.username}` : t('settings.bridgeUnsupportedProto').replace('{protocol}', (s.protocol || 'ftp').toUpperCase()).replace('{app}', app)}
                                    </div>
                                </div>
                                <span className="text-[10px] text-gray-400 uppercase flex-shrink-0">{s.protocol || 'ftp'}</span>
                            </div>
                        );
                    })}
                </div>
                <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                    {exportSelectedIds.size} / {exportable.length} {t('settings.selected')}
                    {unsupportedCount > 0 && ` (${unsupportedCount} ${t('settings.bridgeSkipped').toLowerCase()})`}
                </div>
            </div>
            {meta?.secretPolicy !== 'metadata' && (
                <div className="flex items-center gap-3 p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 rounded-lg">
                    <Checkbox
                        checked={includeCredentials}
                        onChange={setIncludeCredentials}
                        label={
                            <div>
                                <div className="text-sm font-medium flex items-center gap-1"><Lock size={14} />{t('settings.includeCredentials')}</div>
                                <div className="text-xs text-gray-500 dark:text-gray-400">{t('settings.includeCredentialsHint')}</div>
                            </div>
                        }
                    />
                </div>
            )}
            {exportResult && exportResult.skipped.length > 0 && (
                <div className="text-xs text-gray-400 dark:text-gray-500">
                    {t('settings.bridgeSkipped')}: {exportResult.skipped.map(s => s.name).join(', ')}
                </div>
            )}
            {exportable.length === 0 && (
                <div className="text-sm text-amber-600 dark:text-amber-400 flex items-center gap-2">
                    <AlertCircle size={14} />{t('settings.bridgeNoExportable').replace('{app}', app)}
                </div>
            )}
            {alerts}
            <div className="flex gap-2">
                {backBtn}
                <button
                    onClick={runExport}
                    disabled={loading || exportSelectedIds.size === 0}
                    className="flex-1 px-4 py-2 text-sm bg-cyan-600 text-white rounded-lg hover:bg-cyan-700 disabled:opacity-50 flex items-center justify-center gap-2"
                >
                    {loading ? <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" /> : <Download size={16} />}
                    {t('settings.bridgeExportButton').replace('{count}', String(exportSelectedIds.size))}
                </button>
            </div>
        </div>
    );
};
