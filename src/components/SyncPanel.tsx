import React, { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { FileComparison, CompareOptions, SyncStatus, SyncDirection, ProviderType, isFtpProtocol, TransferProgress, SyncIndex } from '../types';
import { useTranslation } from '../i18n';
import {
    Loader2, Search, RefreshCw, Zap, X, FolderSync,
    Folder, Globe, File, AlertTriangle, Check,
    ArrowUp, ArrowDown, Plus, Minus, ArrowLeftRight,
    ArrowDownToLine, ArrowUpFromLine, CheckCircle2, XCircle,
    Clock, SkipForward, StopCircle
} from 'lucide-react';
import './SyncPanel.css';

interface SyncPanelProps {
    isOpen: boolean;
    onClose: () => void;
    localPath: string;
    remotePath: string;
    isConnected: boolean;
    protocol?: ProviderType;
    onSyncComplete?: () => void;
}

interface SyncReport {
    uploaded: number;
    downloaded: number;
    skipped: number;
    dirsCreated: number;
    errors: string[];
    totalBytes: number;
    durationMs: number;
}

// Per-file sync result tracking
type FileSyncResult = 'pending' | 'syncing' | 'success' | 'error' | 'skipped';

// Status display configuration with Lucide icon components
const STATUS_ICONS: Record<SyncStatus, { Icon: typeof Check; color: string }> = {
    identical: { Icon: Check, color: '#10b981' },
    local_newer: { Icon: ArrowUp, color: '#3b82f6' },
    remote_newer: { Icon: ArrowDown, color: '#f59e0b' },
    local_only: { Icon: Plus, color: '#10b981' },
    remote_only: { Icon: Minus, color: '#f59e0b' },
    conflict: { Icon: AlertTriangle, color: '#ef4444' },
    size_mismatch: { Icon: ArrowLeftRight, color: '#ef4444' },
};

// FTP transfer settings to avoid "Data connection already open"
const FTP_TRANSFER_DELAY_MS = 350;
const FTP_RETRY_MAX = 3;
const FTP_RETRY_BASE_DELAY_MS = 500;

export const SyncPanel: React.FC<SyncPanelProps> = ({
    isOpen,
    onClose,
    localPath,
    remotePath,
    isConnected,
    protocol,
    onSyncComplete,
}) => {
    const t = useTranslation();
    const [editLocalPath, setEditLocalPath] = useState(localPath);
    const [editRemotePath, setEditRemotePath] = useState(remotePath);
    const [comparisons, setComparisons] = useState<FileComparison[]>([]);
    const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());
    const [isComparing, setIsComparing] = useState(false);
    const [scanProgress, setScanProgress] = useState<{ phase: string; files_found: number } | null>(null);
    const [isSyncing, setIsSyncing] = useState(false);
    const [syncProgress, setSyncProgress] = useState<{ current: number; total: number } | null>(null);
    const [currentFileProgress, setCurrentFileProgress] = useState<TransferProgress | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [syncReport, setSyncReport] = useState<SyncReport | null>(null);
    const [fileResults, setFileResults] = useState<Map<string, FileSyncResult>>(new Map());
    const [hasIndex, setHasIndex] = useState(false);
    const isProvider = !!protocol && !isFtpProtocol(protocol);
    const isFtp = !isProvider;

    const [options, setOptions] = useState<CompareOptions>({
        // Cloud providers don't preserve timestamps on upload, so default to size-only
        compare_timestamp: !isProvider,
        compare_size: true,
        compare_checksum: false,
        exclude_patterns: ['node_modules', '.git', '.DS_Store', 'Thumbs.db', '__pycache__', 'target'],
        direction: 'bidirectional',
    });
    const unlistenRef = useRef<UnlistenFn | null>(null);
    const cancelledRef = useRef(false);

    // Sync paths from props when panel opens
    useEffect(() => {
        if (isOpen) {
            setEditLocalPath(localPath);
            setEditRemotePath(remotePath);
            // Check if a sync index exists for this path pair
            invoke<SyncIndex | null>('load_sync_index_cmd', { localPath, remotePath })
                .then(idx => setHasIndex(!!idx))
                .catch(() => setHasIndex(false));
        }
    }, [isOpen, localPath, remotePath]);

    // Load default options on mount
    useEffect(() => {
        const loadDefaults = async () => {
            try {
                const defaults = await invoke<CompareOptions>('get_compare_options_default');
                // Cloud providers don't preserve timestamps on upload, default to size-only
                if (isProvider) {
                    defaults.compare_timestamp = false;
                }
                setOptions(defaults);
            } catch (e) {
                console.error('Failed to load default options:', e);
            }
        };
        loadDefaults();
    }, [isProvider]);

    // Cleanup event listener on unmount
    useEffect(() => {
        return () => {
            if (unlistenRef.current) {
                unlistenRef.current();
            }
        };
    }, []);

    const handleCompare = async () => {
        if (!isConnected) {
            setError(t('syncPanel.notConnected'));
            return;
        }

        setIsComparing(true);
        setError(null);
        setComparisons([]);
        setSelectedPaths(new Set());
        setSyncReport(null);
        setFileResults(new Map());
        setScanProgress(null);

        let unlistenScan: UnlistenFn | null = null;
        try {
            unlistenScan = await listen<{ phase: string; files_found: number }>('sync_scan_progress', (event) => {
                setScanProgress(event.payload);
            });

            // Check if sync index exists for this path pair
            const existingIndex = await invoke<SyncIndex | null>('load_sync_index_cmd', {
                localPath: editLocalPath,
                remotePath: editRemotePath,
            }).catch(() => null);
            setHasIndex(!!existingIndex);

            const command = isProvider ? 'provider_compare_directories' : 'compare_directories';
            const results = await invoke<FileComparison[]>(command, {
                localPath: editLocalPath,
                remotePath: editRemotePath,
                options,
            });

            // Filter to only show differences (not identical)
            let differences = results.filter(r => r.status !== 'identical');

            // Apply direction filter
            if (options.direction === 'remote_to_local') {
                differences = differences.filter(r =>
                    r.status === 'remote_newer' || r.status === 'remote_only'
                );
            } else if (options.direction === 'local_to_remote') {
                differences = differences.filter(r =>
                    r.status === 'local_newer' || r.status === 'local_only'
                );
            }

            setComparisons(differences);

            // Auto-select all non-conflict, non-directory items
            const autoSelect = new Set<string>();
            differences.forEach(c => {
                if (c.status !== 'conflict' && c.status !== 'size_mismatch' && !c.is_dir) {
                    autoSelect.add(c.relative_path);
                }
            });
            setSelectedPaths(autoSelect);

        } catch (e: any) {
            console.error('[SyncPanel] Compare error:', e);
            setError(e.toString());
        } finally {
            if (unlistenScan) unlistenScan();
            setScanProgress(null);
            setIsComparing(false);
        }
    };

    const handleSelectAll = () => {
        const all = new Set<string>();
        comparisons.forEach(c => {
            if (!c.is_dir) all.add(c.relative_path);
        });
        setSelectedPaths(all);
    };

    const handleDeselectAll = () => {
        setSelectedPaths(new Set());
    };

    const toggleSelection = (path: string) => {
        const newSelection = new Set(selectedPaths);
        if (newSelection.has(path)) {
            newSelection.delete(path);
        } else {
            newSelection.add(path);
        }
        setSelectedPaths(newSelection);
    };

    const formatSize = (bytes: number): string => {
        if (bytes === 0) return '\u2014';
        if (bytes < 1024) return `${bytes} B`;
        if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
        if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
        return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
    };

    const formatSpeed = (bps: number): string => {
        if (bps < 1024) return `${bps} B/s`;
        if (bps < 1024 * 1024) return `${(bps / 1024).toFixed(1)} KB/s`;
        return `${(bps / (1024 * 1024)).toFixed(1)} MB/s`;
    };

    const formatDuration = (ms: number): string => {
        const secs = Math.floor(ms / 1000);
        if (secs < 60) return `${secs}s`;
        const mins = Math.floor(secs / 60);
        const remSecs = secs % 60;
        return `${mins}m ${remSecs}s`;
    };

    const handleDirectionChange = (direction: SyncDirection) => {
        setOptions(prev => ({ ...prev, direction }));
    };

    // Count directories that would be synced (empty dirs not in selectedPaths)
    const syncableDirs = comparisons.filter(c => c.is_dir &&
        ((c.status === 'remote_only' && (options.direction === 'remote_to_local' || options.direction === 'bidirectional')) ||
         (c.status === 'local_only' && (options.direction === 'local_to_remote' || options.direction === 'bidirectional')))
    ).length;

    const hasSyncableItems = selectedPaths.size > 0 || syncableDirs > 0;

    const handleReset = () => {
        setComparisons([]);
        setSelectedPaths(new Set());
        setError(null);
        setSyncProgress(null);
        setCurrentFileProgress(null);
        setSyncReport(null);
        setFileResults(new Map());
        cancelledRef.current = false;
    };

    const handleClose = () => {
        cancelledRef.current = true;
        handleReset();
        onClose();
    };

    const handleCancel = () => {
        cancelledRef.current = true;
    };

    const delay = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

    // Execute sync
    const handleSync = async () => {
        if (!hasSyncableItems || !isConnected) return;

        setIsSyncing(true);
        setError(null);
        setSyncProgress({ current: 0, total: selectedPaths.size });
        setSyncReport(null);
        cancelledRef.current = false;

        // Initialize all file results as pending
        const initialResults = new Map<string, FileSyncResult>();
        comparisons.forEach(c => {
            if (selectedPaths.has(c.relative_path)) {
                initialResults.set(c.relative_path, 'pending');
            }
        });
        setFileResults(initialResults);

        // Listen for transfer progress events
        const unlisten = await listen<TransferProgress>('transfer_event', (event) => {
            if (event.payload && event.payload.percentage !== undefined) {
                setCurrentFileProgress(event.payload);
            }
        });
        unlistenRef.current = unlisten;

        const selectedComparisons = comparisons.filter(c => selectedPaths.has(c.relative_path) && !c.is_dir);
        let completed = 0;
        let uploaded = 0;
        let downloaded = 0;
        let skipped = 0;
        let totalBytes = 0;
        const errors: string[] = [];
        const startTime = Date.now();

        // Pre-create remote directories needed for uploads
        if (selectedComparisons.some(c => c.status === 'local_newer' || c.status === 'local_only')) {
            const dirsToCreate = new Set<string>();
            for (const item of selectedComparisons) {
                const shouldUpload = (item.status === 'local_newer' || item.status === 'local_only') &&
                    (options.direction === 'local_to_remote' || options.direction === 'bidirectional');
                if (shouldUpload && /[\\/]/.test(item.relative_path)) {
                    // Collect all parent directories
                    const parts = item.relative_path.split(/[\\/]/);
                    for (let i = 1; i < parts.length; i++) {
                        dirsToCreate.add(parts.slice(0, i).join('/'));
                    }
                }
            }
            // Sort by depth (shortest first) so parents are created before children
            const sortedDirs = [...dirsToCreate].sort((a, b) => a.split(/[\\/]/).length - b.split(/[\\/]/).length);
            const remoteBase = editRemotePath.replace(/\/+$/, '');
            for (const dir of sortedDirs) {
                const remoteDirPath = `${remoteBase}/${dir}`;
                try {
                    if (isProvider) {
                        await invoke('provider_mkdir', { path: remoteDirPath });
                    } else {
                        await invoke('create_remote_folder', { path: remoteDirPath });
                    }
                } catch {
                    // Directory may already exist - ignore errors
                }
            }
        }

        // Pre-create local directories needed for downloads
        if (selectedComparisons.some(c => c.status === 'remote_newer' || c.status === 'remote_only')) {
            const dirsToCreate = new Set<string>();
            for (const item of selectedComparisons) {
                const shouldDownload = (item.status === 'remote_newer' || item.status === 'remote_only') &&
                    (options.direction === 'remote_to_local' || options.direction === 'bidirectional');
                if (shouldDownload && /[\\/]/.test(item.relative_path)) {
                    const parts = item.relative_path.split(/[\\/]/);
                    for (let i = 1; i < parts.length; i++) {
                        dirsToCreate.add(parts.slice(0, i).join('/'));
                    }
                }
            }
            const localBase = editLocalPath.replace(/\/+$/, '');
            for (const dir of [...dirsToCreate].sort((a, b) => a.split(/[\\/]/).length - b.split(/[\\/]/).length)) {
                try {
                    await invoke('create_local_folder', { path: `${localBase}/${dir}` });
                } catch {
                    // Directory may already exist
                }
            }
        }

        // Create standalone empty directories (remote_only → local, local_only → remote)
        let dirsCreated = 0;
        const dirComparisons = comparisons.filter(c => c.is_dir &&
            ((c.status === 'remote_only' && (options.direction === 'remote_to_local' || options.direction === 'bidirectional')) ||
             (c.status === 'local_only' && (options.direction === 'local_to_remote' || options.direction === 'bidirectional')))
        );
        for (const dir of dirComparisons.sort((a, b) => a.relative_path.split('/').length - b.relative_path.split('/').length)) {
            try {
                if (dir.status === 'remote_only') {
                    const localDirPath = `${editLocalPath.replace(/\/+$/, '')}/${dir.relative_path}`;
                    await invoke('create_local_folder', { path: localDirPath });
                } else if (dir.status === 'local_only') {
                    const remoteDirPath = `${editRemotePath.replace(/\/+$/, '')}/${dir.relative_path}`;
                    if (isProvider) {
                        await invoke('provider_mkdir', { path: remoteDirPath });
                    } else {
                        await invoke('create_remote_folder', { path: remoteDirPath });
                    }
                }
                dirsCreated++;
                setFileResults(prev => new Map(prev).set(dir.relative_path, 'success'));
            } catch {
                // Directory may already exist
                setFileResults(prev => new Map(prev).set(dir.relative_path, 'skipped'));
            }
        }

        for (const item of selectedComparisons) {
            // Check for cancellation
            if (cancelledRef.current) {
                // Mark remaining as skipped
                setFileResults(prev => {
                    const next = new Map(prev);
                    for (const c of selectedComparisons.slice(completed)) {
                        if (next.get(c.relative_path) === 'pending') {
                            next.set(c.relative_path, 'skipped');
                        }
                    }
                    return next;
                });
                skipped += selectedComparisons.length - completed;
                break;
            }

            // Mark current file as syncing
            setFileResults(prev => new Map(prev).set(item.relative_path, 'syncing'));

            try {
                const localFilePath = `${editLocalPath.replace(/\/+$/, '')}/${item.relative_path}`;
                const remoteFilePath = `${editRemotePath.replace(/\/+$/, '')}/${item.relative_path}`;

                const shouldUpload = (item.status === 'local_newer' || item.status === 'local_only') &&
                    (options.direction === 'local_to_remote' || options.direction === 'bidirectional');

                const shouldDownload = (item.status === 'remote_newer' || item.status === 'remote_only') &&
                    (options.direction === 'remote_to_local' || options.direction === 'bidirectional');

                // Helper: execute a transfer invoke with FTP retry logic
                const executeTransfer = async (cmd: string, args: Record<string, any>) => {
                    if (!isFtp) {
                        await invoke(cmd, args);
                        return;
                    }
                    // FTP: retry on "Data connection" errors with exponential backoff
                    for (let attempt = 1; attempt <= FTP_RETRY_MAX; attempt++) {
                        try {
                            await invoke(cmd, args);
                            return;
                        } catch (err: any) {
                            const msg = err?.toString() || '';
                            const isRetryable = msg.includes('Data connection') || msg.includes('data connection');
                            if (isRetryable && attempt < FTP_RETRY_MAX) {
                                // NOOP to reset server state + exponential backoff
                                try { await invoke('ftp_noop'); } catch { /* ignore */ }
                                await delay(FTP_RETRY_BASE_DELAY_MS * attempt);
                                continue;
                            }
                            throw err;
                        }
                    }
                };

                if (shouldUpload) {
                    if (isProvider) {
                        await invoke('provider_upload_file', {
                            localPath: localFilePath,
                            remotePath: remoteFilePath,
                        });
                    } else {
                        await executeTransfer('upload_file', {
                            params: { local_path: localFilePath, remote_path: remoteFilePath }
                        });
                    }
                    uploaded++;
                    totalBytes += item.local_info?.size || 0;
                    setFileResults(prev => new Map(prev).set(item.relative_path, 'success'));
                } else if (shouldDownload) {
                    if (isProvider) {
                        await invoke('provider_download_file', {
                            remotePath: remoteFilePath,
                            localPath: localFilePath,
                        });
                    } else {
                        await executeTransfer('download_file', {
                            params: { remote_path: remoteFilePath, local_path: localFilePath }
                        });
                    }
                    downloaded++;
                    totalBytes += item.remote_info?.size || 0;
                    setFileResults(prev => new Map(prev).set(item.relative_path, 'success'));
                } else {
                    skipped++;
                    setFileResults(prev => new Map(prev).set(item.relative_path, 'skipped'));
                }

                // Between FTP transfers: NOOP + delay to flush server data connection state
                if (isFtp && completed < selectedComparisons.length - 1) {
                    try { await invoke('ftp_noop'); } catch { /* ignore */ }
                    await delay(FTP_TRANSFER_DELAY_MS);
                }
            } catch (e: any) {
                errors.push(`${item.relative_path}: ${e.toString()}`);
                setFileResults(prev => new Map(prev).set(item.relative_path, 'error'));
            }

            completed++;
            setSyncProgress({ current: completed, total: selectedComparisons.length });
            setCurrentFileProgress(null);
        }

        // Cleanup listener
        unlisten();
        unlistenRef.current = null;

        setIsSyncing(false);
        setSyncProgress(null);
        setCurrentFileProgress(null);

        // Show completion report
        setSyncReport({
            uploaded,
            downloaded,
            skipped,
            dirsCreated,
            errors,
            totalBytes,
            durationMs: Date.now() - startTime,
        });

        // Save sync index for faster future comparisons
        try {
            const indexFiles: Record<string, { size: number; modified: string | null; is_dir: boolean }> = {};
            // Record successfully synced files (uploaded + downloaded + dirs created)
            const successPaths = new Set<string>();
            for (const item of selectedComparisons) {
                // Items that were uploaded or downloaded successfully (not in errors)
                if (!errors.some(e => e.startsWith(item.relative_path + ':'))) {
                    successPaths.add(item.relative_path);
                }
            }
            for (const dir of dirComparisons) {
                successPaths.add(dir.relative_path);
            }
            for (const item of [...selectedComparisons, ...dirComparisons]) {
                if (!successPaths.has(item.relative_path)) continue;
                // Use the "winning" side's info as the synced state
                const info = item.local_info || item.remote_info;
                if (info) {
                    indexFiles[item.relative_path] = {
                        size: info.size,
                        modified: info.modified,
                        is_dir: info.is_dir,
                    };
                }
            }
            // Merge with existing index (keep entries we didn't touch)
            const existing = await invoke<SyncIndex | null>('load_sync_index_cmd', {
                localPath: editLocalPath,
                remotePath: editRemotePath,
            });
            const mergedFiles = { ...(existing?.files || {}), ...indexFiles };
            const index: SyncIndex = {
                version: 1,
                last_sync: new Date().toISOString(),
                local_path: editLocalPath,
                remote_path: editRemotePath,
                files: mergedFiles,
            };
            await invoke('save_sync_index_cmd', { index });
            setHasIndex(true);
        } catch (e) {
            console.error('[SyncPanel] Failed to save sync index:', e);
        }

        // Refresh file listings
        if (onSyncComplete && errors.length === 0) {
            await onSyncComplete();
        }
    };

    if (!isOpen) return null;

    const truncatePath = (path: string, maxLen: number = 50): string => {
        if (path.length <= maxLen) return path;
        return '...' + path.slice(-maxLen + 3);
    };

    const getStatusLabel = (status: SyncStatus): string => {
        switch (status) {
            case 'identical': return t('syncPanel.statusIdentical');
            case 'local_newer': return t('syncPanel.statusUpload');
            case 'remote_newer': return t('syncPanel.statusDownload');
            case 'local_only': return t('syncPanel.statusNewLocal');
            case 'remote_only': return t('syncPanel.statusNewRemote');
            case 'conflict': return t('syncPanel.statusConflict');
            case 'size_mismatch': return t('syncPanel.statusSizeDiffers');
        }
    };

    const getDirectionDescription = (direction: SyncDirection): { Icon: typeof ArrowDownToLine; text: string } => {
        switch (direction) {
            case 'remote_to_local':
                return { Icon: ArrowDownToLine, text: t('syncPanel.descRemoteToLocal') };
            case 'local_to_remote':
                return { Icon: ArrowUpFromLine, text: t('syncPanel.descLocalToRemote') };
            case 'bidirectional':
                return { Icon: ArrowLeftRight, text: t('syncPanel.descBidirectional') };
        }
    };

    // Get the sync result indicator for a file row
    const getFileResultIcon = (path: string): React.ReactNode => {
        const result = fileResults.get(path);
        if (!result || result === 'pending') return null;
        if (result === 'syncing') return <Loader2 size={14} className="animate-spin text-blue-400" />;
        if (result === 'success') return <CheckCircle2 size={14} className="text-green-500" />;
        if (result === 'error') return <XCircle size={14} className="text-red-500" />;
        if (result === 'skipped') return <SkipForward size={14} className="text-gray-400" />;
        return null;
    };

    const dirDesc = getDirectionDescription(options.direction);

    return (
        <div className="sync-panel-overlay">
            <div className="sync-panel">
                <div className="sync-panel-header">
                    <h2><FolderSync size={20} className="inline mr-2" /> {t('syncPanel.title')}</h2>
                    <button className="sync-close-btn" onClick={handleClose}><X size={18} /></button>
                </div>

                {/* Path Display */}
                <div className="sync-paths-display">
                    <div className="sync-path-row">
                        <span className="sync-path-label"><Folder size={14} className="inline mr-1" /> {t('syncPanel.local')}:</span>
                        <input
                            className="sync-path-input"
                            value={editLocalPath}
                            onChange={e => setEditLocalPath(e.target.value)}
                            disabled={isSyncing || isComparing}
                            spellCheck={false}
                        />
                    </div>
                    <div className="sync-path-row">
                        <span className="sync-path-label"><Globe size={14} className="inline mr-1" /> {t('syncPanel.remote')}:</span>
                        <input
                            className="sync-path-input"
                            value={editRemotePath}
                            onChange={e => setEditRemotePath(e.target.value)}
                            disabled={isSyncing || isComparing}
                            spellCheck={false}
                        />
                    </div>
                </div>

                {/* Sync Report (shown after sync completes) */}
                {syncReport && (
                    <div className={`mx-4 my-3 p-4 rounded-lg border ${syncReport.errors.length > 0 ? 'bg-yellow-50 dark:bg-yellow-900/20 border-yellow-300 dark:border-yellow-700' : 'bg-green-50 dark:bg-green-900/20 border-green-300 dark:border-green-700'}`}>
                        <div className="flex items-center gap-2 mb-3">
                            {syncReport.errors.length > 0 ? (
                                <AlertTriangle size={20} className="text-yellow-500" />
                            ) : (
                                <CheckCircle2 size={20} className="text-green-500" />
                            )}
                            <span className="font-semibold text-sm">
                                {syncReport.errors.length > 0 ? t('syncPanel.reportPartial') : t('syncPanel.reportSuccess')}
                            </span>
                        </div>
                        <div className="grid grid-cols-2 gap-2 text-sm">
                            {syncReport.uploaded > 0 && (
                                <div className="flex items-center gap-1.5">
                                    <ArrowUp size={14} className="text-blue-500" />
                                    <span>{t('syncPanel.reportUploaded')}: <strong>{syncReport.uploaded}</strong></span>
                                </div>
                            )}
                            {syncReport.downloaded > 0 && (
                                <div className="flex items-center gap-1.5">
                                    <ArrowDown size={14} className="text-amber-500" />
                                    <span>{t('syncPanel.reportDownloaded')}: <strong>{syncReport.downloaded}</strong></span>
                                </div>
                            )}
                            {syncReport.dirsCreated > 0 && (
                                <div className="flex items-center gap-1.5">
                                    <Folder size={14} className="text-purple-500" />
                                    <span>{t('syncPanel.reportDirsCreated')}: <strong>{syncReport.dirsCreated}</strong></span>
                                </div>
                            )}
                            {syncReport.skipped > 0 && (
                                <div className="flex items-center gap-1.5">
                                    <SkipForward size={14} className="text-gray-400" />
                                    <span>{t('syncPanel.reportSkipped')}: <strong>{syncReport.skipped}</strong></span>
                                </div>
                            )}
                            {syncReport.errors.length > 0 && (
                                <div className="flex items-center gap-1.5">
                                    <XCircle size={14} className="text-red-500" />
                                    <span>{t('syncPanel.reportErrors')}: <strong>{syncReport.errors.length}</strong></span>
                                </div>
                            )}
                            <div className="flex items-center gap-1.5">
                                <ArrowLeftRight size={14} className="text-gray-400" />
                                <span>{t('syncPanel.reportTransferred')}: <strong>{formatSize(syncReport.totalBytes)}</strong></span>
                            </div>
                            <div className="flex items-center gap-1.5">
                                <Clock size={14} className="text-gray-400" />
                                <span>{t('syncPanel.reportDuration')}: <strong>{formatDuration(syncReport.durationMs)}</strong></span>
                            </div>
                        </div>
                        {syncReport.errors.length > 0 && (
                            <div className="mt-2 text-xs text-red-600 dark:text-red-400 max-h-32 overflow-y-auto">
                                {syncReport.errors.map((err, i) => (
                                    <div key={i} className="truncate">{err}</div>
                                ))}
                            </div>
                        )}
                    </div>
                )}

                {/* Options (hidden during sync report) */}
                {!syncReport && (
                    <div className="sync-panel-options">
                        <div className="sync-direction-selector">
                            <label><strong>{t('syncPanel.direction')}:</strong></label>
                            <div className="direction-buttons">
                                <button
                                    className={options.direction === 'remote_to_local' ? 'active' : ''}
                                    onClick={() => handleDirectionChange('remote_to_local')}
                                    disabled={isSyncing}
                                >
                                    <ArrowDown size={14} className="inline mr-1" /> {t('syncPanel.dirRemoteToLocal')}
                                </button>
                                <button
                                    className={options.direction === 'local_to_remote' ? 'active' : ''}
                                    onClick={() => handleDirectionChange('local_to_remote')}
                                    disabled={isSyncing}
                                >
                                    <ArrowUp size={14} className="inline mr-1" /> {t('syncPanel.dirLocalToRemote')}
                                </button>
                                <button
                                    className={options.direction === 'bidirectional' ? 'active' : ''}
                                    onClick={() => handleDirectionChange('bidirectional')}
                                    disabled={isSyncing}
                                >
                                    <ArrowLeftRight size={14} className="inline mr-1" /> {t('syncPanel.dirBoth')}
                                </button>
                            </div>
                        </div>

                        {/* Action Description */}
                        <div className="sync-action-description">
                            <dirDesc.Icon size={16} className="inline mr-1.5 flex-shrink-0" />
                            {dirDesc.text}
                        </div>

                        <div className="sync-compare-options">
                            <label>
                                <input
                                    type="checkbox"
                                    checked={options.compare_timestamp}
                                    onChange={e => setOptions(prev => ({ ...prev, compare_timestamp: e.target.checked }))}
                                    disabled={isSyncing}
                                />
                                {t('syncPanel.timestamp')}
                            </label>
                            <label>
                                <input
                                    type="checkbox"
                                    checked={options.compare_size}
                                    onChange={e => setOptions(prev => ({ ...prev, compare_size: e.target.checked }))}
                                    disabled={isSyncing}
                                />
                                {t('syncPanel.size')}
                            </label>
                            <label>
                                <input
                                    type="checkbox"
                                    checked={options.compare_checksum}
                                    onChange={e => setOptions(prev => ({ ...prev, compare_checksum: e.target.checked }))}
                                    disabled={isSyncing}
                                />
                                {t('syncPanel.checksum')}
                            </label>
                        </div>

                        <div className="flex items-center gap-3">
                            <button
                                className="sync-compare-btn"
                                onClick={handleCompare}
                                disabled={isComparing || !isConnected || isSyncing}
                            >
                                {isComparing ? (
                                    <><Loader2 size={16} className="animate-spin" /> {t('syncPanel.comparing')}...</>
                                ) : (
                                    <><Search size={16} /> {t('syncPanel.compareNow')}</>
                                )}
                            </button>
                            {hasIndex && (
                                <span className="text-xs text-emerald-400 flex items-center gap-1" title={t('syncPanel.indexAvailableTooltip')}>
                                    <Zap size={12} /> {t('syncPanel.indexAvailable')}
                                </span>
                            )}
                        </div>
                    </div>
                )}

                {error && (
                    <div className="sync-error">
                        <AlertTriangle size={16} className="inline mr-1" /> {error}
                    </div>
                )}

                {/* Progress bar during sync */}
                {isSyncing && currentFileProgress && (
                    <div className="mx-4 mb-2">
                        <div className="flex items-center justify-between text-xs text-gray-500 dark:text-gray-400 mb-1">
                            <span className="truncate max-w-[60%]">{currentFileProgress.filename}</span>
                            <span>{currentFileProgress.percentage}% &middot; {formatSpeed(currentFileProgress.speed_bps)}</span>
                        </div>
                        <div className="w-full h-1.5 bg-gray-200 dark:bg-gray-600 rounded-full overflow-hidden">
                            <div
                                className="h-full bg-blue-500 rounded-full transition-all duration-300"
                                style={{ width: `${currentFileProgress.percentage}%` }}
                            />
                        </div>
                    </div>
                )}

                <div className="sync-results">
                    {isComparing && (
                        <div className="sync-loading">
                            <Loader2 size={32} className="animate-spin" />
                            <span>
                                {scanProgress
                                    ? `${t(`syncPanel.scanPhase.${scanProgress.phase}`)} (${scanProgress.files_found} ${t('syncPanel.filesFound')})`
                                    : `${t('syncPanel.scanning')}...`}
                            </span>
                        </div>
                    )}

                    {comparisons.length === 0 && !isComparing && !syncReport && (
                        <div className="sync-empty">
                            {isConnected
                                ? t('syncPanel.clickCompare')
                                : t('syncPanel.notConnected')}
                        </div>
                    )}

                    {comparisons.length > 0 && (
                        <>
                            <div className="sync-table-header">
                                <div className="sync-col-check">
                                    <input
                                        type="checkbox"
                                        checked={selectedPaths.size === comparisons.filter(c => !c.is_dir).length && comparisons.length > 0}
                                        onChange={() =>
                                            selectedPaths.size === comparisons.filter(c => !c.is_dir).length
                                                ? handleDeselectAll()
                                                : handleSelectAll()
                                        }
                                        disabled={isSyncing}
                                    />
                                </div>
                                <div className="sync-col-status">{t('syncPanel.colStatus')}</div>
                                <div className="sync-col-file">{t('syncPanel.colFile')}</div>
                                <div className="sync-col-result"></div>
                                <div className="sync-col-local">{t('syncPanel.colLocal')}</div>
                                <div className="sync-col-remote">{t('syncPanel.colRemote')}</div>
                            </div>

                            <div className="sync-table-body">
                                {comparisons.map((comparison) => {
                                    const statusCfg = STATUS_ICONS[comparison.status];
                                    const StatusIcon = statusCfg.Icon;
                                    const resultIcon = getFileResultIcon(comparison.relative_path);
                                    const result = fileResults.get(comparison.relative_path);
                                    return (
                                        <div
                                            key={comparison.relative_path}
                                            className={`sync-row ${selectedPaths.has(comparison.relative_path) ? 'selected' : ''} ${result === 'success' ? 'sync-row-success' : result === 'error' ? 'sync-row-error' : ''} ${comparison.is_dir ? 'sync-row-dir' : ''}`}
                                            onClick={() => !isSyncing && !comparison.is_dir && toggleSelection(comparison.relative_path)}
                                        >
                                            <div className="sync-col-check">
                                                {!comparison.is_dir ? (
                                                    <input
                                                        type="checkbox"
                                                        checked={selectedPaths.has(comparison.relative_path)}
                                                        onChange={() => toggleSelection(comparison.relative_path)}
                                                        onClick={e => e.stopPropagation()}
                                                        disabled={isSyncing}
                                                    />
                                                ) : (
                                                    <span className="text-gray-500 text-xs">&mdash;</span>
                                                )}
                                            </div>
                                            <div className="sync-col-status" style={{ color: statusCfg.color }}>
                                                <StatusIcon size={14} />
                                                <span className="status-label">{getStatusLabel(comparison.status)}</span>
                                            </div>
                                            <div className="sync-col-file">
                                                {comparison.is_dir ? <Folder size={14} className="inline mr-1" /> : <File size={14} className="inline mr-1" />}
                                                {comparison.relative_path}{comparison.is_dir ? ':' : ''}
                                            </div>
                                            <div className="sync-col-result">
                                                {resultIcon}
                                            </div>
                                            <div className="sync-col-local">
                                                {comparison.local_info
                                                    ? formatSize(comparison.local_info.size)
                                                    : '\u2014'}
                                            </div>
                                            <div className="sync-col-remote">
                                                {comparison.remote_info
                                                    ? formatSize(comparison.remote_info.size)
                                                    : '\u2014'}
                                            </div>
                                        </div>
                                    );
                                })}
                            </div>
                        </>
                    )}
                </div>

                <div className="sync-panel-footer">
                    <div className="sync-summary">
                        {syncProgress ? (
                            <span className="sync-progress-indicator">
                                <Loader2 size={14} className="animate-spin" />
                                {t('syncPanel.syncing')} {syncProgress.current}/{syncProgress.total}...
                            </span>
                        ) : syncReport ? (
                            <span className="text-sm">{t('syncPanel.syncComplete')}</span>
                        ) : comparisons.length > 0 ? (
                            <span>{selectedPaths.size} / {comparisons.filter(c => !c.is_dir).length} {t('syncPanel.filesSelected')}</span>
                        ) : null}
                    </div>
                    <div className="sync-actions">
                        {isSyncing ? (
                            <button className="sync-cancel-btn" onClick={handleCancel}>
                                <StopCircle size={16} /> {t('syncPanel.cancel')}
                            </button>
                        ) : (
                            <>
                                <button onClick={handleReset} disabled={comparisons.length === 0 && !syncReport}>
                                    <RefreshCw size={14} /> {t('syncPanel.reset')}
                                </button>
                                {!syncReport && (
                                    <>
                                        <button onClick={handleDeselectAll} disabled={selectedPaths.size === 0}>
                                            {t('syncPanel.deselect')}
                                        </button>
                                        <button onClick={handleSelectAll} disabled={comparisons.length === 0}>
                                            {t('syncPanel.selectAll')}
                                        </button>
                                    </>
                                )}
                            </>
                        )}
                        {syncReport ? (
                            <button className="sync-execute-btn" onClick={handleClose}>
                                <Check size={16} /> {t('syncPanel.close')}
                            </button>
                        ) : (
                            <button
                                className="sync-execute-btn"
                                onClick={handleSync}
                                disabled={!hasSyncableItems || isSyncing}
                            >
                                {isSyncing ? (
                                    <><Loader2 size={16} className="animate-spin" /> {t('syncPanel.syncing')} ({syncProgress?.current || 0}/{syncProgress?.total || 0})...</>
                                ) : (
                                    <><Zap size={16} /> {t('syncPanel.synchronize')} ({selectedPaths.size + syncableDirs})</>
                                )}
                            </button>
                        )}
                    </div>
                </div>
            </div>
        </div>
    );
};

export default SyncPanel;
