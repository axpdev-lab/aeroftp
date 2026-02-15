import React, { useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import {
    FileComparison, CompareOptions, SyncStatus, SyncDirection, ProviderType,
    isFtpProtocol, TransferProgress, TransferEvent, SyncIndex,
    RetryPolicy, VerifyPolicy, SyncJournal, SyncJournalEntry,
    SyncErrorInfo, SyncErrorKind, VerifyResult, JournalEntryStatus,
    SyncProfile
} from '../types';
import { useTranslation } from '../i18n';
import { TransferProgressBar } from './TransferProgressBar';
import {
    Loader2, Search, RefreshCw, Zap, X, FolderSync,
    Folder, Globe, File, AlertTriangle, Check,
    ArrowUp, ArrowDown, Plus, Minus, ArrowLeftRight,
    ArrowDownToLine, ArrowUpFromLine, CheckCircle2, XCircle,
    Clock, SkipForward, StopCircle, RotateCcw, ShieldCheck,
    Wifi, WifiOff, KeyRound, HardDrive, Timer, Ban,
    Trash2, Gauge
} from 'lucide-react';
import './SyncPanel.css';
import { formatSize } from '../utils/formatters';

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
    errors: SyncErrorInfo[];
    verifyFailed: number;
    retried: number;
    totalBytes: number;
    durationMs: number;
}

// Per-file sync result tracking
type FileSyncResult = 'pending' | 'syncing' | 'success' | 'error' | 'skipped' | 'retrying' | 'verifying' | 'verify_failed';

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

// Error kind icon mapping
const ERROR_KIND_ICONS: Record<SyncErrorKind, typeof WifiOff> = {
    network: WifiOff,
    auth: KeyRound,
    path_not_found: Search,
    permission_denied: Ban,
    quota_exceeded: HardDrive,
    rate_limit: Timer,
    timeout: Clock,
    file_locked: Ban,
    disk_error: HardDrive,
    unknown: AlertTriangle,
};

// FTP transfer settings to avoid "Data connection already open"
const FTP_TRANSFER_DELAY_MS = 350;

// Virtual scrolling constants — only render visible rows in comparison list
const VIRTUAL_ROW_HEIGHT = 45; // px — matches sync-row padding (12+12) + content (~20) + border (1)
const VIRTUAL_OVERSCAN = 10; // extra rows above/below viewport
const VIRTUAL_VIEWPORT = 350; // px — matches .sync-table-body max-height

// Default retry/verify policies
const DEFAULT_RETRY_POLICY: RetryPolicy = {
    max_retries: 3,
    base_delay_ms: 500,
    max_delay_ms: 10_000,
    timeout_ms: 120_000,
    backoff_multiplier: 2.0,
};

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
    const [hasJournal, setHasJournal] = useState(false);
    const [pendingJournal, setPendingJournal] = useState<SyncJournal | null>(null);
    const isProvider = !!protocol && !isFtpProtocol(protocol);
    const isFtp = !isProvider;

    // Phase 2: Retry and Verify policies
    const [retryPolicy, setRetryPolicy] = useState<RetryPolicy>(DEFAULT_RETRY_POLICY);
    const [verifyPolicy, setVerifyPolicy] = useState<VerifyPolicy>('size_only');

    // Bandwidth control (KB/s, 0 = unlimited)
    const [downloadLimit, setDownloadLimit] = useState(0);
    const [uploadLimit, setUploadLimit] = useState(0);

    // Sync Profiles
    const [profiles, setProfiles] = useState<SyncProfile[]>([]);
    const [activeProfileId, setActiveProfileId] = useState<string>('custom');

    // Conflict resolution: maps relative_path → resolution action
    type ConflictResolution = 'upload' | 'download' | 'skip';
    const [conflictResolutions, setConflictResolutions] = useState<Map<string, ConflictResolution>>(new Map());
    const [showConflictPanel, setShowConflictPanel] = useState(false);

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
    const compareAbortedRef = useRef(false);
    const speedHistoryRef = useRef<number[]>([]);
    const scrollContainerRef = useRef<HTMLDivElement | null>(null);
    const [scrollTop, setScrollTop] = useState(0);

    // Batched state update refs — prevent O(n²) Map copies with large file counts
    const pendingFileResultsRef = useRef<Map<string, FileSyncResult>>(new Map());
    const flushTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
    const pendingSyncProgressRef = useRef<{ current: number; total: number } | null>(null);
    const progressFlushTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

    const flushFileResults = useCallback(() => {
        const pending = pendingFileResultsRef.current;
        if (pending.size > 0) {
            const batch = new Map(pending);
            pending.clear();
            setFileResults(prev => {
                const next = new Map(prev);
                for (const [k, v] of batch) {
                    next.set(k, v);
                }
                return next;
            });
        }
        flushTimerRef.current = null;
    }, []);

    const updateFileResult = useCallback((path: string, result: FileSyncResult) => {
        pendingFileResultsRef.current.set(path, result);
        if (!flushTimerRef.current) {
            flushTimerRef.current = setTimeout(flushFileResults, 200);
        }
    }, [flushFileResults]);

    // Virtual scroll handler — update scroll position for visible row calculation
    const handleVirtualScroll = useCallback((e: React.UIEvent<HTMLDivElement>) => {
        setScrollTop(e.currentTarget.scrollTop);
    }, []);

    const updateSyncProgress = useCallback((current: number, total: number) => {
        pendingSyncProgressRef.current = { current, total };
        if (!progressFlushTimerRef.current) {
            progressFlushTimerRef.current = setTimeout(() => {
                if (pendingSyncProgressRef.current) {
                    setSyncProgress(pendingSyncProgressRef.current);
                }
                progressFlushTimerRef.current = null;
            }, 150);
        }
    }, []);

    // Sync paths from props when panel opens
    useEffect(() => {
        if (isOpen) {
            setEditLocalPath(localPath);
            setEditRemotePath(remotePath);
            // Check if a sync index exists for this path pair
            invoke<SyncIndex | null>('load_sync_index_cmd', { localPath, remotePath })
                .then(idx => setHasIndex(!!idx))
                .catch(() => setHasIndex(false));
            // Check for interrupted journal
            invoke<SyncJournal | null>('load_sync_journal_cmd', { localPath, remotePath })
                .then(journal => {
                    if (journal && !journal.completed) {
                        setHasJournal(true);
                        setPendingJournal(journal);
                    } else {
                        setHasJournal(false);
                        setPendingJournal(null);
                    }
                })
                .catch(() => {
                    setHasJournal(false);
                    setPendingJournal(null);
                });
            // Auto-cleanup completed journals older than 30 days
            invoke<number>('cleanup_old_journals_cmd', { maxAgeDays: 30 }).catch(() => {});
            // Load current speed limits
            const loadCmd = isFtp ? 'get_speed_limit' : 'provider_get_speed_limit';
            invoke<[number, number]>(loadCmd)
                .then(([dl, ul]) => { setDownloadLimit(dl); setUploadLimit(ul); })
                .catch(() => {});
            // Load sync profiles
            invoke<SyncProfile[]>('load_sync_profiles_cmd')
                .then(p => setProfiles(p))
                .catch(() => {});
        }
    }, [isOpen, localPath, remotePath, isFtp]);

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
        setScrollTop(0);
        compareAbortedRef.current = false;

        let unlistenScan: UnlistenFn | null = null;
        try {
            unlistenScan = await listen<{ phase: string; files_found: number }>('sync_scan_progress', (event) => {
                if (!compareAbortedRef.current) {
                    setScanProgress(event.payload);
                }
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

            // Discard results if user cancelled the scan
            if (compareAbortedRef.current) return;

            // Filter to only show differences (not identical)
            let differences = results.filter(r => r.status !== 'identical');

            // Remove directory size/mtime mismatches — directory "size" is filesystem block
            // metadata (always 4.0 KB on ext4), not actual content size. Comparing is meaningless.
            // Only keep directories that are truly new (remote_only/local_only).
            differences = differences.filter(r =>
                !r.is_dir || r.status === 'remote_only' || r.status === 'local_only'
            );

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
            if (!compareAbortedRef.current) {
                console.error('[SyncPanel] Compare error:', e);
                setError(e.toString());
            }
        } finally {
            if (unlistenScan) unlistenScan();
            setScanProgress(null);
            setIsComparing(false);
        }
    };

    // Cancel an ongoing compare/scan — signals the Rust backend to abort and release the FTP lock
    const handleCancelCompare = async () => {
        compareAbortedRef.current = true;
        setIsComparing(false);
        setScanProgress(null);
        // Signal backend to stop scanning — releases FTP mutex so other operations can proceed
        try { await invoke('cancel_transfer'); } catch { /* ignore */ }
        // Reset the flag so subsequent transfers aren't blocked
        try { await invoke('reset_cancel_flag'); } catch { /* ignore */ }
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

    // formatSpeed is provided by the TransferProgressBar component

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
        setScrollTop(0);
        setConflictResolutions(new Map());
        setShowConflictPanel(false);
        cancelledRef.current = false;
    };

    const handleClose = async () => {
        cancelledRef.current = true;
        if (isSyncing) {
            try {
                await invoke('cancel_transfer');
            } catch (e) {
                console.error('[SyncPanel] Cancel on close error:', e);
            }
        }
        handleReset();
        onClose();
    };

    const handleCancel = async () => {
        cancelledRef.current = true;
        // Invoke backend cancel_transfer command to hard-stop the transfer engine
        try {
            await invoke('cancel_transfer');
        } catch (e) {
            console.error('[SyncPanel] Cancel transfer error:', e);
        }
    };

    // Dismiss a pending journal (discard resume)
    const handleDismissJournal = async () => {
        try {
            await invoke('delete_sync_journal_cmd', {
                localPath: editLocalPath,
                remotePath: editRemotePath,
            });
        } catch { /* ignore */ }
        setHasJournal(false);
        setPendingJournal(null);
    };

    const handleClearHistory = async () => {
        if (!confirm(t('syncPanel.clearHistoryConfirm'))) return;
        try {
            const count = await invoke<number>('clear_all_journals_cmd');
            setHasJournal(false);
            setPendingJournal(null);
            if (count > 0) {
                setError(t('syncPanel.journalsCleared', { count: String(count) }));
                setTimeout(() => setError(null), 3000);
            }
        } catch { /* ignore */ }
    };

    const handleSpeedLimitChange = async (dl: number, ul: number) => {
        setDownloadLimit(dl);
        setUploadLimit(ul);
        const cmd = isFtp ? 'set_speed_limit' : 'provider_set_speed_limit';
        try {
            await invoke(cmd, { downloadKb: dl, uploadKb: ul });
        } catch { /* ignore */ }
    };

    const applyProfile = (profileId: string) => {
        setActiveProfileId(profileId);
        if (profileId === 'custom') return;
        const profile = profiles.find(p => p.id === profileId);
        if (!profile) return;
        setOptions({
            compare_timestamp: profile.compare_timestamp,
            compare_size: profile.compare_size,
            compare_checksum: profile.compare_checksum,
            exclude_patterns: [...profile.exclude_patterns],
            direction: profile.direction,
        });
        setRetryPolicy({ ...profile.retry_policy });
        setVerifyPolicy(profile.verify_policy);
    };

    // Conflict resolution helpers
    const conflicts = comparisons.filter(c => c.status === 'conflict');
    const unresolvedConflicts = conflicts.filter(c => !conflictResolutions.has(c.relative_path));

    const resolveConflict = (path: string, resolution: ConflictResolution) => {
        setConflictResolutions(prev => {
            const next = new Map(prev);
            next.set(path, resolution);
            return next;
        });
        // Auto-select resolved conflicts for sync
        if (resolution !== 'skip') {
            setSelectedPaths(prev => {
                const next = new Set(prev);
                next.add(path);
                return next;
            });
        } else {
            setSelectedPaths(prev => {
                const next = new Set(prev);
                next.delete(path);
                return next;
            });
        }
    };

    const resolveAllConflicts = (resolution: ConflictResolution) => {
        const updates = new Map(conflictResolutions);
        const sel = new Set(selectedPaths);
        for (const c of conflicts) {
            updates.set(c.relative_path, resolution);
            if (resolution !== 'skip') {
                sel.add(c.relative_path);
            } else {
                sel.delete(c.relative_path);
            }
        }
        setConflictResolutions(updates);
        setSelectedPaths(sel);
        setShowConflictPanel(false);
    };

    const resolveAllKeepNewer = () => {
        const updates = new Map(conflictResolutions);
        const sel = new Set(selectedPaths);
        for (const c of conflicts) {
            const localTime = c.local_info?.modified ? new Date(c.local_info.modified).getTime() : 0;
            const remoteTime = c.remote_info?.modified ? new Date(c.remote_info.modified).getTime() : 0;
            const resolution: ConflictResolution = localTime >= remoteTime ? 'upload' : 'download';
            updates.set(c.relative_path, resolution);
            sel.add(c.relative_path);
        }
        setConflictResolutions(updates);
        setSelectedPaths(sel);
        setShowConflictPanel(false);
    };

    const delay = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

    // Calculate retry delay with exponential backoff
    const getRetryDelay = (attempt: number): number => {
        const d = retryPolicy.base_delay_ms * Math.pow(retryPolicy.backoff_multiplier, attempt - 1);
        return Math.min(d, retryPolicy.max_delay_ms);
    };

    // Execute a single transfer with retry logic and error classification
    const executeTransferWithRetry = async (
        cmd: string,
        args: Record<string, any>,
        filePath: string,
    ): Promise<{ success: boolean; attempts: number; error?: SyncErrorInfo }> => {
        const maxAttempts = retryPolicy.max_retries;

        for (let attempt = 1; attempt <= maxAttempts; attempt++) {
            try {
                // Wrap with cancellable timeout (F5: prevents timer leak in large batches)
                if (retryPolicy.timeout_ms > 0) {
                    let timerId: ReturnType<typeof setTimeout>;
                    const timeoutPromise = new Promise<never>((_, reject) => {
                        timerId = setTimeout(() => reject(new Error('Operation timed out')), retryPolicy.timeout_ms);
                    });
                    try {
                        await Promise.race([invoke(cmd, args), timeoutPromise]);
                    } finally {
                        clearTimeout(timerId!);
                    }
                } else {
                    await invoke(cmd, args);
                }
                return { success: true, attempts: attempt };
            } catch (err: any) {
                const rawMsg = err?.toString() || 'Unknown error';
                const errorInfo = await invoke<SyncErrorInfo>('classify_transfer_error', {
                    rawError: rawMsg,
                    filePath,
                }).catch(() => ({
                    kind: 'unknown' as SyncErrorKind,
                    message: rawMsg,
                    retryable: true,
                    file_path: filePath,
                }));

                if (errorInfo.retryable && attempt < maxAttempts) {
                    // Update UI to show retrying
                    updateFileResult(filePath, 'retrying');
                    // FTP: NOOP to reset server state
                    if (isFtp) {
                        try { await invoke('ftp_noop'); } catch { /* ignore */ }
                    }
                    await delay(getRetryDelay(attempt));
                    continue;
                }

                return { success: false, attempts: attempt, error: errorInfo };
            }
        }
        return { success: false, attempts: maxAttempts };
    };

    // Post-transfer verification for downloads
    const verifyDownload = async (
        localFilePath: string,
        expectedSize: number,
        expectedMtime: string | null,
    ): Promise<VerifyResult | null> => {
        if (verifyPolicy === 'none') return null;
        try {
            return await invoke<VerifyResult>('verify_local_transfer', {
                localPath: localFilePath,
                expectedSize,
                expectedMtime: expectedMtime,
                policy: verifyPolicy,
            });
        } catch {
            return null;
        }
    };

    // Execute sync (new or resume)
    const handleSync = async (resumeJournal?: SyncJournal) => {
        if (!hasSyncableItems && !resumeJournal) return;
        if (!isConnected) return;

        setIsSyncing(true);
        setError(null);
        setSyncReport(null);
        cancelledRef.current = false;

        // CRITICAL: Reset backend cancel flag before starting sync
        // Without this, any previous cancel (from panels or sync) leaves the flag stuck on true
        // and ALL subsequent download_file/upload_file calls fail immediately
        try { await invoke('reset_cancel_flag'); } catch { /* ignore */ }

        // Build transfer list — from comparisons or from journal entries (resume without compare)
        let selectedComparisons: FileComparison[];
        if (resumeJournal && comparisons.length === 0) {
            // Resume mode: reconstruct minimal FileComparison objects from journal entries
            selectedComparisons = resumeJournal.entries.map(e => ({
                relative_path: e.relative_path,
                status: (e.action === 'upload' ? 'local_newer' : 'remote_newer') as SyncStatus,
                is_dir: false,
                local_info: e.action === 'upload'
                    ? { name: e.relative_path.split('/').pop() || '', path: e.relative_path, size: e.bytes_transferred || 0, modified: null, is_dir: false, checksum: null }
                    : null,
                remote_info: e.action === 'download'
                    ? { name: e.relative_path.split('/').pop() || '', path: e.relative_path, size: e.bytes_transferred || 0, modified: null, is_dir: false, checksum: null }
                    : null,
            }));
        } else {
            selectedComparisons = comparisons.filter(c => selectedPaths.has(c.relative_path) && !c.is_dir);
        }

        setSyncProgress({ current: 0, total: selectedComparisons.length });

        // Initialize file results
        const initialResults = new Map<string, FileSyncResult>();
        selectedComparisons.forEach(c => initialResults.set(c.relative_path, 'pending'));
        if (resumeJournal) {
            // Mark already-completed/skipped entries from journal
            for (const e of resumeJournal.entries) {
                if (e.status === 'completed') initialResults.set(e.relative_path, 'success');
                else if (e.status === 'skipped') initialResults.set(e.relative_path, 'skipped');
            }
        }
        setFileResults(initialResults);
        speedHistoryRef.current = [];

        // Listen for transfer progress events (throttled to prevent UI flood)
        let lastProgressUpdate = 0;
        const unlisten = await listen<TransferEvent>('transfer_event', (event) => {
            if (event.payload?.progress && event.payload.progress.percentage !== undefined) {
                // Accumulate speed samples regardless of throttle
                if (event.payload.progress.speed_bps > 0) {
                    const history = speedHistoryRef.current;
                    history.push(event.payload.progress.speed_bps);
                    if (history.length > 120) history.shift();
                }
                // Throttle React state updates to max ~5/sec (200ms)
                const now = Date.now();
                if (now - lastProgressUpdate > 200 || event.payload.progress.percentage >= 100) {
                    lastProgressUpdate = now;
                    setCurrentFileProgress(event.payload.progress);
                }
            }
        });
        unlistenRef.current = unlisten;
        let completed = 0;
        let uploaded = 0;
        let downloaded = 0;
        let skipped = 0;
        let totalBytes = 0;
        let retried = 0;
        let verifyFailed = 0;
        const errors: SyncErrorInfo[] = [];
        const startTime = Date.now();

        // Create journal for checkpoint
        const journal: SyncJournal = resumeJournal || {
            id: crypto.randomUUID?.() || `${Date.now()}`,
            created_at: new Date().toISOString(),
            updated_at: new Date().toISOString(),
            local_path: editLocalPath,
            remote_path: editRemotePath,
            direction: options.direction,
            retry_policy: retryPolicy,
            verify_policy: verifyPolicy,
            entries: selectedComparisons.map(c => {
                let action: string;
                if (c.status === 'conflict') {
                    const resolution = conflictResolutions.get(c.relative_path);
                    action = resolution === 'upload' ? 'upload' : 'download';
                } else {
                    action = (c.status === 'local_newer' || c.status === 'local_only') ? 'upload' : 'download';
                }
                return {
                    relative_path: c.relative_path,
                    action,
                    status: 'pending' as JournalEntryStatus,
                    attempts: 0,
                    last_error: null,
                    verified: null,
                    bytes_transferred: 0,
                };
            }),
            completed: false,
        };

        // Save initial journal
        try {
            await invoke('save_sync_journal_cmd', { journal });
        } catch (e) {
            console.error('[SyncPanel] Failed to save journal:', e);
        }

        // Pre-create remote directories needed for uploads
        if (selectedComparisons.some(c => c.status === 'local_newer' || c.status === 'local_only')) {
            const dirsToCreate = new Set<string>();
            for (const item of selectedComparisons) {
                const shouldUpload = (item.status === 'local_newer' || item.status === 'local_only') &&
                    (options.direction === 'local_to_remote' || options.direction === 'bidirectional');
                if (shouldUpload && /[\\/]/.test(item.relative_path)) {
                    const parts = item.relative_path.split(/[\\/]/);
                    for (let i = 1; i < parts.length; i++) {
                        dirsToCreate.add(parts.slice(0, i).join('/'));
                    }
                }
            }
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
                updateFileResult(dir.relative_path, 'success');
            } catch {
                // Directory may already exist
                updateFileResult(dir.relative_path, 'skipped');
            }
        }

        for (let i = 0; i < selectedComparisons.length; i++) {
            const item = selectedComparisons[i];
            const journalEntry = journal.entries[i];

            // Skip already completed entries (for resume)
            if (resumeJournal && (journalEntry?.status === 'completed' || journalEntry?.status === 'skipped')) {
                if (journalEntry.status === 'completed') {
                    uploaded += journalEntry.action === 'upload' ? 1 : 0;
                    downloaded += journalEntry.action === 'download' ? 1 : 0;
                    totalBytes += journalEntry.bytes_transferred;
                } else {
                    skipped++;
                }
                completed++;
                updateSyncProgress(completed, selectedComparisons.length);
                continue;
            }

            // Check for cancellation
            if (cancelledRef.current) {
                // Mark remaining as skipped in journal
                for (let j = i; j < selectedComparisons.length; j++) {
                    if (journal.entries[j] && journal.entries[j].status === 'pending') {
                        journal.entries[j].status = 'skipped';
                    }
                }
                // Flush pending results then mark remaining as skipped
                if (flushTimerRef.current) {
                    clearTimeout(flushTimerRef.current);
                    flushTimerRef.current = null;
                }
                flushFileResults();
                setFileResults(prev => {
                    const next = new Map(prev);
                    for (const c of selectedComparisons.slice(i)) {
                        if (next.get(c.relative_path) === 'pending') {
                            next.set(c.relative_path, 'skipped');
                        }
                    }
                    return next;
                });
                skipped += selectedComparisons.length - i;
                // Save journal checkpoint on cancel
                try {
                    await invoke('save_sync_journal_cmd', { journal });
                } catch { /* ignore */ }
                break;
            }

            // Mark current file as syncing
            updateFileResult(item.relative_path, 'syncing');
            if (journalEntry) journalEntry.status = 'in_progress';

            const localFilePath = `${editLocalPath.replace(/\/+$/, '')}/${item.relative_path}`;
            const remoteFilePath = `${editRemotePath.replace(/\/+$/, '')}/${item.relative_path}`;

            // Determine transfer direction — use conflict resolution if available
            const conflictRes = item.status === 'conflict' ? conflictResolutions.get(item.relative_path) : undefined;
            const shouldUpload = conflictRes === 'upload' ||
                (!conflictRes && (item.status === 'local_newer' || item.status === 'local_only') &&
                (options.direction === 'local_to_remote' || options.direction === 'bidirectional'));

            const shouldDownload = conflictRes === 'download' ||
                (!conflictRes && (item.status === 'remote_newer' || item.status === 'remote_only') &&
                (options.direction === 'remote_to_local' || options.direction === 'bidirectional'));

            // Track whether a data connection was actually opened (for FTP delay logic)
            let didTransfer = false;

            if (shouldUpload) {
                const cmd = isProvider ? 'provider_upload_file' : 'upload_file';
                const args = isProvider
                    ? { localPath: localFilePath, remotePath: remoteFilePath }
                    : { params: { local_path: localFilePath, remote_path: remoteFilePath } };

                const result = await executeTransferWithRetry(cmd, args, item.relative_path);
                if (journalEntry) {
                    journalEntry.attempts = result.attempts;
                    if (result.attempts > 1) retried++;
                }

                if (result.success) {
                    didTransfer = true;
                    uploaded++;
                    const bytes = item.local_info?.size || 0;
                    totalBytes += bytes;
                    if (journalEntry) {
                        journalEntry.status = 'completed';
                        journalEntry.bytes_transferred = bytes;
                        journalEntry.verified = true; // Uploads verified by server acceptance
                    }
                    updateFileResult(item.relative_path, 'success');
                } else {
                    if (journalEntry) {
                        journalEntry.status = 'failed';
                        journalEntry.last_error = result.error || null;
                    }
                    errors.push(result.error || {
                        kind: 'unknown',
                        message: `Upload failed: ${item.relative_path}`,
                        retryable: false,
                        file_path: item.relative_path,
                    });
                    updateFileResult(item.relative_path, 'error');
                }
            } else if (shouldDownload) {
                const cmd = isProvider ? 'provider_download_file' : 'download_file';
                const args = isProvider
                    ? { remotePath: remoteFilePath, localPath: localFilePath }
                    : { params: { remote_path: remoteFilePath, local_path: localFilePath } };

                const result = await executeTransferWithRetry(cmd, args, item.relative_path);
                if (journalEntry) {
                    journalEntry.attempts = result.attempts;
                    if (result.attempts > 1) retried++;
                }

                if (result.success) {
                    didTransfer = true;
                    // Post-transfer verification for downloads
                    const expectedSize = item.remote_info?.size || 0;
                    const expectedMtime = item.remote_info?.modified || null;
                    updateFileResult(item.relative_path, 'verifying');

                    const vResult = await verifyDownload(localFilePath, expectedSize, expectedMtime);

                    if (vResult && !vResult.passed) {
                        verifyFailed++;
                        if (journalEntry) {
                            journalEntry.status = 'verify_failed';
                            journalEntry.verified = false;
                            journalEntry.last_error = {
                                kind: 'unknown',
                                message: vResult.message || 'Verification failed',
                                retryable: true,
                                file_path: item.relative_path,
                            };
                        }
                        errors.push({
                            kind: 'unknown',
                            message: `${item.relative_path}: ${vResult.message || 'Verification failed'}`,
                            retryable: true,
                            file_path: item.relative_path,
                        });
                        updateFileResult(item.relative_path, 'verify_failed');
                    } else {
                        downloaded++;
                        const bytes = expectedSize;
                        totalBytes += bytes;
                        if (journalEntry) {
                            journalEntry.status = 'completed';
                            journalEntry.bytes_transferred = bytes;
                            journalEntry.verified = true;
                        }
                        updateFileResult(item.relative_path, 'success');
                    }
                } else {
                    if (journalEntry) {
                        journalEntry.status = 'failed';
                        journalEntry.last_error = result.error || null;
                    }
                    errors.push(result.error || {
                        kind: 'unknown',
                        message: `Download failed: ${item.relative_path}`,
                        retryable: false,
                        file_path: item.relative_path,
                    });
                    updateFileResult(item.relative_path, 'error');
                }
            } else {
                skipped++;
                if (journalEntry) journalEntry.status = 'skipped';
                updateFileResult(item.relative_path, 'skipped');
            }

            // FTP delay ONLY after successful transfers that opened a data connection
            // Skipped/failed files never opened a data connection — no delay needed
            if (isFtp && didTransfer && i < selectedComparisons.length - 1) {
                if (selectedComparisons.length > 100) {
                    // Large batch: minimal delay, NOOP only every 10 files for keep-alive
                    if ((i + 1) % 10 === 0) {
                        try { await invoke('ftp_noop'); } catch { /* ignore */ }
                    }
                    await delay(15);
                } else {
                    // Small batch: full safety delay
                    try { await invoke('ftp_noop'); } catch { /* ignore */ }
                    await delay(FTP_TRANSFER_DELAY_MS);
                }
            }

            completed++;
            updateSyncProgress(completed, selectedComparisons.length);

            // Save journal checkpoint — adaptive interval to reduce I/O (F7)
            const checkpointInterval = selectedComparisons.length > 2000 ? 200
                : selectedComparisons.length > 500 ? 100 : 10;
            if (completed % checkpointInterval === 0) {
                try {
                    await invoke('save_sync_journal_cmd', { journal });
                } catch { /* ignore */ }
            }
        }

        // Mark journal as complete and save final state
        journal.completed = !cancelledRef.current;
        try {
            await invoke('save_sync_journal_cmd', { journal });
            // If fully completed, clean up journal
            if (journal.completed) {
                await invoke('delete_sync_journal_cmd', {
                    localPath: editLocalPath,
                    remotePath: editRemotePath,
                });
            }
        } catch { /* ignore */ }

        // Flush any pending batched state updates before showing report
        if (flushTimerRef.current) {
            clearTimeout(flushTimerRef.current);
            flushTimerRef.current = null;
        }
        flushFileResults();
        if (progressFlushTimerRef.current) {
            clearTimeout(progressFlushTimerRef.current);
            progressFlushTimerRef.current = null;
        }

        // Cleanup listener
        unlisten();
        unlistenRef.current = null;

        setIsSyncing(false);
        setSyncProgress(null);
        setCurrentFileProgress(null);
        setHasJournal(!journal.completed);
        setPendingJournal(!journal.completed ? journal : null);

        // Show completion report
        setSyncReport({
            uploaded,
            downloaded,
            skipped,
            dirsCreated,
            errors,
            verifyFailed,
            retried,
            totalBytes,
            durationMs: Date.now() - startTime,
        });

        // Save sync index for faster future comparisons
        try {
            const indexFiles: Record<string, { size: number; modified: string | null; is_dir: boolean }> = {};
            const successPaths = new Set<string>();
            for (const entry of journal.entries) {
                if (entry.status === 'completed') {
                    successPaths.add(entry.relative_path);
                }
            }
            for (const dir of dirComparisons) {
                successPaths.add(dir.relative_path);
            }
            for (const item of [...selectedComparisons, ...dirComparisons]) {
                if (!successPaths.has(item.relative_path)) continue;
                const info = item.local_info || item.remote_info;
                if (info) {
                    indexFiles[item.relative_path] = {
                        size: info.size,
                        modified: info.modified,
                        is_dir: info.is_dir,
                    };
                }
            }
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

        // Refresh file listings if at least one operation completed
        if (onSyncComplete && (uploaded > 0 || downloaded > 0)) {
            await onSyncComplete();
        }
    };

    if (!isOpen) return null;

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

    const getErrorKindLabel = (kind: SyncErrorKind): string => {
        const key = `syncPanel.errorKind.${kind}`;
        const val = t(key);
        return val !== key ? val : kind.replace(/_/g, ' ');
    };

    // Get the sync result indicator for a file row
    const getFileResultIcon = (path: string): React.ReactNode => {
        const result = fileResults.get(path);
        if (!result || result === 'pending') return null;
        if (result === 'syncing') return <Loader2 size={14} className="animate-spin text-blue-400" />;
        if (result === 'retrying') return <RotateCcw size={14} className="animate-spin text-amber-400" />;
        if (result === 'verifying') return <ShieldCheck size={14} className="animate-pulse text-cyan-400" />;
        if (result === 'success') return <CheckCircle2 size={14} className="text-green-500" />;
        if (result === 'error') return <XCircle size={14} className="text-red-500" />;
        if (result === 'verify_failed') return <ShieldCheck size={14} className="text-red-500" />;
        if (result === 'skipped') return <SkipForward size={14} className="text-gray-400" />;
        return null;
    };

    // Group errors by kind for the report
    const groupErrorsByKind = (errs: SyncErrorInfo[]): Map<SyncErrorKind, SyncErrorInfo[]> => {
        const grouped = new Map<SyncErrorKind, SyncErrorInfo[]>();
        for (const err of errs) {
            const list = grouped.get(err.kind) || [];
            list.push(err);
            grouped.set(err.kind, list);
        }
        return grouped;
    };

    const dirDesc = getDirectionDescription(options.direction);

    // Virtual scroll: compute which rows are visible in the viewport
    const visibleRowCount = Math.ceil(VIRTUAL_VIEWPORT / VIRTUAL_ROW_HEIGHT) + VIRTUAL_OVERSCAN * 2;
    const virtualStart = Math.max(0, Math.floor(scrollTop / VIRTUAL_ROW_HEIGHT) - VIRTUAL_OVERSCAN);
    const virtualEnd = Math.min(comparisons.length, virtualStart + visibleRowCount);
    const virtualTopPad = virtualStart * VIRTUAL_ROW_HEIGHT;
    const virtualTotalHeight = comparisons.length * VIRTUAL_ROW_HEIGHT;

    return (
        <div className="sync-panel-overlay">
            <div className="sync-panel">
                <div className="sync-panel-header">
                    <div className="flex items-center gap-3">
                        <h2><FolderSync size={20} className="inline mr-2" /> {t('syncPanel.title')}</h2>
                        {profiles.length > 0 && (
                            <select
                                className="text-xs bg-gray-800 dark:bg-gray-800 text-gray-300 border border-gray-600 rounded px-2 py-1"
                                value={activeProfileId}
                                onChange={e => applyProfile(e.target.value)}
                                disabled={isSyncing}
                            >
                                <option value="custom">{t('syncPanel.profileCustom')}</option>
                                {profiles.map(p => (
                                    <option key={p.id} value={p.id}>
                                        {p.id === 'mirror' ? t('syncPanel.profileMirror')
                                            : p.id === 'two_way' ? t('syncPanel.profileTwoWay')
                                            : p.id === 'backup' ? t('syncPanel.profileBackup')
                                            : p.name}
                                    </option>
                                ))}
                            </select>
                        )}
                    </div>
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

                {/* Resume Journal Banner */}
                {hasJournal && pendingJournal && !isSyncing && !syncReport && (
                    <div className="mx-4 my-2 p-3 rounded-lg border bg-amber-50 dark:bg-amber-900/20 border-amber-300 dark:border-amber-700">
                        <div className="flex items-center gap-2 mb-1">
                            <RotateCcw size={16} className="text-amber-500" />
                            <span className="font-semibold text-sm">{t('syncPanel.journalFound')}</span>
                        </div>
                        <p className="text-xs text-gray-600 dark:text-gray-400 mb-2">
                            {t('syncPanel.journalDescription', {
                                completed: String(pendingJournal.entries.filter(e => e.status === 'completed').length),
                                total: String(pendingJournal.entries.length),
                                date: new Date(pendingJournal.updated_at).toLocaleString(),
                            })}
                        </p>
                        <div className="flex gap-2">
                            <button
                                className="text-xs px-3 py-1 rounded bg-amber-500 text-white hover:bg-amber-600"
                                onClick={() => handleSync(pendingJournal)}
                                disabled={!isConnected}
                            >
                                <RotateCcw size={12} className="inline mr-1" />
                                {t('syncPanel.resumeSync')}
                            </button>
                            <button
                                className="text-xs px-3 py-1 rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600"
                                onClick={handleDismissJournal}
                            >
                                {t('syncPanel.dismissJournal')}
                            </button>
                        </div>
                    </div>
                )}

                {/* Conflict Resolution Center */}
                {conflicts.length > 0 && !isSyncing && !syncReport && (
                    <div className="mx-4 my-2 p-3 rounded-lg border bg-red-50 dark:bg-red-900/15 border-red-300 dark:border-red-700">
                        <div className="flex items-center justify-between mb-2">
                            <div className="flex items-center gap-2">
                                <AlertTriangle size={16} className="text-red-500" />
                                <span className="font-semibold text-sm">
                                    {t('syncPanel.conflictsFound', { count: String(conflicts.length) })}
                                </span>
                                {unresolvedConflicts.length === 0 && (
                                    <span className="text-xs text-green-500 flex items-center gap-1">
                                        <CheckCircle2 size={12} /> {t('syncPanel.conflictResolved')}
                                    </span>
                                )}
                            </div>
                            <button
                                className="text-xs px-2 py-1 rounded bg-red-500/20 text-red-400 border border-red-500/30 hover:bg-red-500/30"
                                onClick={() => setShowConflictPanel(!showConflictPanel)}
                            >
                                {showConflictPanel ? t('syncPanel.close') : t('syncPanel.resolveConflicts')}
                            </button>
                        </div>
                        {!showConflictPanel && (
                            <div className="flex gap-2 flex-wrap">
                                <button
                                    className="text-xs px-2 py-1 rounded bg-blue-500/20 text-blue-400 border border-blue-500/30 hover:bg-blue-500/30"
                                    onClick={resolveAllKeepNewer}
                                >
                                    {t('syncPanel.keepNewerAll')}
                                </button>
                                <button
                                    className="text-xs px-2 py-1 rounded bg-green-500/20 text-green-400 border border-green-500/30 hover:bg-green-500/30"
                                    onClick={() => resolveAllConflicts('upload')}
                                >
                                    {t('syncPanel.keepLocalAll')}
                                </button>
                                <button
                                    className="text-xs px-2 py-1 rounded bg-amber-500/20 text-amber-400 border border-amber-500/30 hover:bg-amber-500/30"
                                    onClick={() => resolveAllConflicts('download')}
                                >
                                    {t('syncPanel.keepRemoteAll')}
                                </button>
                                <button
                                    className="text-xs px-2 py-1 rounded bg-gray-500/20 text-gray-400 border border-gray-500/30 hover:bg-gray-500/30"
                                    onClick={() => resolveAllConflicts('skip')}
                                >
                                    {t('syncPanel.skipAll')}
                                </button>
                            </div>
                        )}
                        {showConflictPanel && (
                            <div className="mt-2 max-h-48 overflow-y-auto space-y-1">
                                {conflicts.map(c => {
                                    const res = conflictResolutions.get(c.relative_path);
                                    return (
                                        <div key={c.relative_path} className="flex items-center gap-2 text-xs py-1 border-b border-gray-200/10 last:border-0">
                                            <File size={12} className="text-gray-400 flex-shrink-0" />
                                            <span className="flex-1 truncate text-gray-300" title={c.relative_path}>
                                                {c.relative_path}
                                            </span>
                                            <span className="text-gray-500 flex-shrink-0">
                                                {c.local_info ? formatSize(c.local_info.size) : '—'} / {c.remote_info ? formatSize(c.remote_info.size) : '—'}
                                            </span>
                                            <div className="flex gap-1 flex-shrink-0">
                                                <button
                                                    className={`px-1.5 py-0.5 rounded text-[10px] ${res === 'upload' ? 'bg-blue-500 text-white' : 'bg-gray-700 text-gray-400 hover:bg-gray-600'}`}
                                                    onClick={() => resolveConflict(c.relative_path, 'upload')}
                                                    title={t('syncPanel.keepLocal')}
                                                >
                                                    <ArrowUp size={10} className="inline" />
                                                </button>
                                                <button
                                                    className={`px-1.5 py-0.5 rounded text-[10px] ${res === 'download' ? 'bg-amber-500 text-white' : 'bg-gray-700 text-gray-400 hover:bg-gray-600'}`}
                                                    onClick={() => resolveConflict(c.relative_path, 'download')}
                                                    title={t('syncPanel.keepRemote')}
                                                >
                                                    <ArrowDown size={10} className="inline" />
                                                </button>
                                                <button
                                                    className={`px-1.5 py-0.5 rounded text-[10px] ${res === 'skip' ? 'bg-gray-500 text-white' : 'bg-gray-700 text-gray-400 hover:bg-gray-600'}`}
                                                    onClick={() => resolveConflict(c.relative_path, 'skip')}
                                                    title={t('syncPanel.skipAll')}
                                                >
                                                    <SkipForward size={10} className="inline" />
                                                </button>
                                            </div>
                                        </div>
                                    );
                                })}
                            </div>
                        )}
                    </div>
                )}

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
                            {syncReport.retried > 0 && (
                                <div className="flex items-center gap-1.5">
                                    <RotateCcw size={14} className="text-amber-400" />
                                    <span>{t('syncPanel.reportRetried')}: <strong>{syncReport.retried}</strong></span>
                                </div>
                            )}
                            {syncReport.verifyFailed > 0 && (
                                <div className="flex items-center gap-1.5">
                                    <ShieldCheck size={14} className="text-red-500" />
                                    <span>{t('syncPanel.reportVerifyFailed')}: <strong>{syncReport.verifyFailed}</strong></span>
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
                        {/* Classified error breakdown */}
                        {syncReport.errors.length > 0 && (
                            <div className="mt-3 border-t border-gray-200 dark:border-gray-600 pt-2">
                                <div className="text-xs font-semibold mb-1">{t('syncPanel.errorBreakdown')}</div>
                                {[...groupErrorsByKind(syncReport.errors)].map(([kind, errs]) => {
                                    const ErrIcon = ERROR_KIND_ICONS[kind] || AlertTriangle;
                                    return (
                                        <div key={kind} className="mb-1">
                                            <div className="flex items-center gap-1 text-xs font-medium text-gray-700 dark:text-gray-300">
                                                <ErrIcon size={12} />
                                                <span>{getErrorKindLabel(kind)} ({errs.length})</span>
                                                {errs[0]?.retryable && (
                                                    <span className="text-[10px] text-amber-500 ml-1">{t('syncPanel.retryable')}</span>
                                                )}
                                            </div>
                                            <div className="ml-4 text-[11px] text-red-600 dark:text-red-400 max-h-20 overflow-y-auto">
                                                {errs.map((err, i) => (
                                                    <div key={i} className="truncate">{err.file_path || err.message}</div>
                                                ))}
                                            </div>
                                        </div>
                                    );
                                })}
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

                        {/* Phase 2: Verify & Retry Options */}
                        <div className="sync-compare-options">
                            <label className="flex items-center gap-1">
                                <ShieldCheck size={12} className="text-cyan-500" />
                                <select
                                    className="text-xs bg-transparent border border-gray-300 dark:border-gray-600 rounded px-1 py-0.5"
                                    value={verifyPolicy}
                                    onChange={e => setVerifyPolicy(e.target.value as VerifyPolicy)}
                                    disabled={isSyncing}
                                >
                                    <option value="none">{t('syncPanel.verifyNone')}</option>
                                    <option value="size_only">{t('syncPanel.verifySize')}</option>
                                    <option value="size_and_mtime">{t('syncPanel.verifySizeMtime')}</option>
                                    <option value="full">{t('syncPanel.verifyFull')}</option>
                                </select>
                            </label>
                            <label className="flex items-center gap-1">
                                <RotateCcw size={12} className="text-amber-500" />
                                <select
                                    className="text-xs bg-transparent border border-gray-300 dark:border-gray-600 rounded px-1 py-0.5"
                                    value={retryPolicy.max_retries}
                                    onChange={e => setRetryPolicy(prev => ({ ...prev, max_retries: Number(e.target.value) }))}
                                    disabled={isSyncing}
                                >
                                    <option value="1">{t('syncPanel.retries', { count: '1' })}</option>
                                    <option value="3">{t('syncPanel.retries', { count: '3' })}</option>
                                    <option value="5">{t('syncPanel.retries', { count: '5' })}</option>
                                    <option value="10">{t('syncPanel.retries', { count: '10' })}</option>
                                </select>
                            </label>
                        </div>

                        {/* Bandwidth control + Clear History */}
                        <div className="sync-compare-options">
                            <label className="flex items-center gap-1">
                                <Gauge size={12} className="text-purple-400" />
                                <span className="text-xs text-gray-400">{t('syncPanel.bandwidthDownload')}:</span>
                                <select
                                    className="text-xs bg-transparent border border-gray-300 dark:border-gray-600 rounded px-1 py-0.5"
                                    value={downloadLimit}
                                    onChange={e => handleSpeedLimitChange(Number(e.target.value), uploadLimit)}
                                    disabled={isSyncing}
                                >
                                    <option value="0">{t('syncPanel.bandwidthUnlimited')}</option>
                                    <option value="128">128 KB/s</option>
                                    <option value="256">256 KB/s</option>
                                    <option value="512">512 KB/s</option>
                                    <option value="1024">1 MB/s</option>
                                    <option value="2048">2 MB/s</option>
                                    <option value="5120">5 MB/s</option>
                                    <option value="10240">10 MB/s</option>
                                </select>
                            </label>
                            <label className="flex items-center gap-1">
                                <Gauge size={12} className="text-purple-400" />
                                <span className="text-xs text-gray-400">{t('syncPanel.bandwidthUpload')}:</span>
                                <select
                                    className="text-xs bg-transparent border border-gray-300 dark:border-gray-600 rounded px-1 py-0.5"
                                    value={uploadLimit}
                                    onChange={e => handleSpeedLimitChange(downloadLimit, Number(e.target.value))}
                                    disabled={isSyncing}
                                >
                                    <option value="0">{t('syncPanel.bandwidthUnlimited')}</option>
                                    <option value="128">128 KB/s</option>
                                    <option value="256">256 KB/s</option>
                                    <option value="512">512 KB/s</option>
                                    <option value="1024">1 MB/s</option>
                                    <option value="2048">2 MB/s</option>
                                    <option value="5120">5 MB/s</option>
                                    <option value="10240">10 MB/s</option>
                                </select>
                            </label>
                            <button
                                className="flex items-center gap-1 text-xs text-red-400 hover:text-red-300 transition-colors ml-auto"
                                onClick={handleClearHistory}
                                disabled={isSyncing}
                                title={t('syncPanel.clearHistory')}
                            >
                                <Trash2 size={12} /> {t('syncPanel.clearHistory')}
                            </button>
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

                <div className="sync-results">
                    {isComparing && (
                        <div className="sync-loading">
                            <Loader2 size={32} className="animate-spin" />
                            <span>
                                {scanProgress
                                    ? `${t(`syncPanel.scanPhase.${scanProgress.phase}`)} (${scanProgress.files_found} ${t('syncPanel.filesFound')})`
                                    : `${t('syncPanel.scanning')}...`}
                            </span>
                            <button
                                className="mt-2 px-4 py-1.5 text-xs rounded bg-red-500/20 text-red-400 border border-red-500/40 hover:bg-red-500/30 transition-colors flex items-center gap-1.5"
                                onClick={handleCancelCompare}
                            >
                                <StopCircle size={14} /> {t('syncPanel.cancel')}
                            </button>
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

                            <div
                                className="sync-table-body"
                                ref={scrollContainerRef}
                                onScroll={handleVirtualScroll}
                            >
                                {/* Virtual scroll container — total height for scrollbar, only visible rows rendered */}
                                <div style={{ height: virtualTotalHeight, position: 'relative' }}>
                                    <div style={{ position: 'absolute', top: virtualTopPad, left: 0, right: 0 }}>
                                        {comparisons.slice(virtualStart, virtualEnd).map((comparison) => {
                                            const statusCfg = STATUS_ICONS[comparison.status];
                                            const StatusIcon = statusCfg.Icon;
                                            const resultIcon = getFileResultIcon(comparison.relative_path);
                                            const result = fileResults.get(comparison.relative_path);
                                            return (
                                                <div
                                                    key={comparison.relative_path}
                                                    className={`sync-row ${selectedPaths.has(comparison.relative_path) ? 'selected' : ''} ${result === 'success' ? 'sync-row-success' : result === 'error' || result === 'verify_failed' ? 'sync-row-error' : ''} ${comparison.is_dir ? 'sync-row-dir' : ''}`}
                                                    onClick={() => !isSyncing && !comparison.is_dir && toggleSelection(comparison.relative_path)}
                                                    style={{ height: VIRTUAL_ROW_HEIGHT, boxSizing: 'border-box' }}
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
                                                        {comparison.relative_path}
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
                                </div>
                            </div>
                        </>
                    )}
                </div>

                {/* Transfer progress — positioned at bottom to avoid layout shift */}
                {isSyncing && syncProgress && (
                    <div className="sync-progress-wrapper">
                        {/* Batch progress bar — always visible even when individual files fail */}
                        <div className="mb-2">
                            <div className="flex justify-between text-xs text-gray-400 mb-1">
                                <span>{syncProgress.current}/{syncProgress.total}</span>
                                <span>{syncProgress.total > 0 ? Math.round((syncProgress.current / syncProgress.total) * 100) : 0}%</span>
                            </div>
                            <div className="w-full h-1.5 bg-gray-700 rounded-full overflow-hidden">
                                <div
                                    className="h-full bg-blue-500 rounded-full transition-all duration-300"
                                    style={{ width: `${syncProgress.total > 0 ? (syncProgress.current / syncProgress.total) * 100 : 0}%` }}
                                />
                            </div>
                        </div>
                        {/* Per-file transfer progress — always visible during sync to prevent flicker */}
                        <TransferProgressBar
                            percentage={currentFileProgress?.percentage ?? 0}
                            filename={currentFileProgress?.filename}
                            speedBps={currentFileProgress?.speed_bps}
                            currentFile={syncProgress.current}
                            totalFiles={syncProgress.total}
                            size="md"
                            variant="gradient"
                            slideAnimation
                            showGraph
                            speedHistory={[...speedHistoryRef.current]}
                        />
                    </div>
                )}

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
                                onClick={() => handleSync()}
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
