// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// GAP-2 of the AeroSync connected-remote gap-closure filone.
//
// `remoteSyncRunner` is the connected-remote sync engine extracted from the
// legacy `SyncPanel.tsx::handleSync`. SLICE 6 of the unification deleted the
// monolith before the unified modal could execute deletes / renames / verify
// / retry / journal against a remote; this module is the framework-light,
// unit-testable core that GAP-3 wires into the unified Plan tab.
//
// It is deliberately free of React and DOM dependencies: every side effect
// (Tauri `invoke`, the clock, the sleep, id generation) is injected so the
// loop can be exercised under Vitest's `node` environment.

import type {
    RetryPolicy,
    VerifyPolicy,
    SyncDirection,
    SyncJournal,
    SyncJournalEntry,
    SyncErrorInfo,
    SyncErrorKind,
    VerifyResult,
    DeltaTransferStats,
    DeltaSavingsSummary,
} from '../types';

/** Per-file lifecycle status emitted to the caller for live UI updates. */
export type SyncRunFileStatus =
    | 'pending'
    | 'syncing'
    | 'retrying'
    | 'verifying'
    | 'success'
    | 'error'
    | 'skipped'
    | 'verify_failed';

/** The transfer / delete action the runner performs for one entry. */
export type SyncRunAction = 'upload' | 'download' | 'delete-remote' | 'delete-local';

/**
 * One file the runner must act on. `relativePath` is resolved against both
 * `localRoot` and `remoteRoot`; it may be a bare name (flat sync) or contain
 * `/` separators (recursive sync) — the runner pre-creates parent dirs either
 * way.
 */
export interface SyncRunFile {
    relativePath: string;
    action: SyncRunAction;
    /** Source-side size in bytes (best-effort; 0 when unknown). */
    size: number;
    /** Source-side mtime ISO string — used for download verify + journal. */
    mtime: string | null;
    /**
     * True iff the action overwrites or removes an existing local file, so
     * the runner archives it first when a versioning strategy is active.
     * Ignored for `upload` / `delete-remote`; `delete-local` always archives.
     */
    overwritesExisting?: boolean;
}

/** Standalone directories to create (counted in `dirsCreated`). */
export interface SyncRunDirs {
    /** Relative paths to create on the remote side. */
    remote: string[];
    /** Relative paths to create on the local side. */
    local: string[];
}

export interface RemoteSyncConfig {
    localRoot: string;
    remoteRoot: string;
    /** true → `provider_*` commands; false → ftp/sftp `upload_file`/`download_file`. */
    isProvider: boolean;
    /** true → emit `ftp_noop` keep-alives + inter-transfer delay. */
    isFtp: boolean;
    retryPolicy: RetryPolicy;
    verifyPolicy: VerifyPolicy;
    deltaSyncEnabled: boolean;
    /**
     * Versioning strategy for archive-before-delete / archive-before-overwrite.
     * `undefined`, `null` or `"disabled"` turns archiving off.
     */
    versioningStrategy?: string | null;
    /** Stop transferring once this many bytes have moved. 0 = unlimited. */
    transferBudget: number;
    /** Stored on the journal for resume bookkeeping. */
    direction: SyncDirection;
    /** Optional bandwidth caps (KB/s, 0/undefined = unlimited) applied at start. */
    uploadLimitKbps?: number;
    downloadLimitKbps?: number;
}

/** The rich connected-remote report — superset of the local-local report. */
export interface SyncRunReport {
    uploaded: number;
    downloaded: number;
    deleted: number;
    skipped: number;
    dirsCreated: number;
    errors: SyncErrorInfo[];
    verifyFailed: number;
    retried: number;
    totalBytes: number;
    durationMs: number;
    /** true iff the run stopped early on cancel or transfer budget. */
    cancelled: boolean;
    /** Absent when no file travelled the rsync delta path this run. */
    delta_savings?: DeltaSavingsSummary;
    delta_bytes_on_wire?: number;
    delta_batch_files?: number;
}

export interface RemoteSyncCallbacks {
    /** Fired on every per-file status transition for live UI. */
    onFileStatus?: (relativePath: string, status: SyncRunFileStatus) => void;
    /** Fired after every completed entry: `(done, total)`. */
    onProgress?: (current: number, total: number) => void;
    /** Polled before every entry; return true to stop gracefully. */
    isCancelled?: () => boolean;
}

export interface RemoteSyncDeps {
    /** Tauri command bridge. Injected so the loop is testable. */
    invoke: <T = unknown>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
    /** Sleep primitive; defaults to a `setTimeout` promise. */
    delay?: (ms: number) => Promise<void>;
    /** Clock; defaults to `Date.now`. */
    now?: () => number;
    /** Journal id generator; defaults to `crypto.randomUUID`. */
    makeId?: () => string;
    /**
     * Live map of per-file delta stats keyed by filename, maintained by the
     * caller from transfer-complete events. The runner re-keys entries onto
     * the full relative path and aggregates `delta_savings` at the end.
     * Omit to skip delta accounting entirely.
     */
    deltaStats?: Map<string, DeltaTransferStats>;
    /**
     * Optional journal to resume: entries already `completed` / `skipped` are
     * not re-transferred and the same journal object continues to be
     * checkpointed.
     */
    resumeJournal?: SyncJournal;
}

const FTP_TRANSFER_DELAY_MS = 60;

const defaultDelay = (ms: number): Promise<void> =>
    new Promise((resolve) => setTimeout(resolve, ms));

const defaultMakeId = (): string => {
    if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
        return crypto.randomUUID();
    }
    return `${Date.now()}-${Math.random().toString(36).slice(2)}`;
};

const stripTrailingSlash = (p: string): string => p.replace(/\/+$/, '');

const basename = (p: string): string => p.split('/').pop() || p;

/** Group a flat error list by `SyncErrorKind` for the report breakdown. */
export const groupErrorsByKind = (
    errors: SyncErrorInfo[],
): Map<SyncErrorKind, SyncErrorInfo[]> => {
    const grouped = new Map<SyncErrorKind, SyncErrorInfo[]>();
    for (const err of errors) {
        const list = grouped.get(err.kind) || [];
        list.push(err);
        grouped.set(err.kind, list);
    }
    return grouped;
};

/**
 * Reconstruct a minimal `SyncRunFile[]` from a journal so a caller can resume
 * an interrupted sync without re-running a fresh compare. `delete` entries are
 * dropped (the journal does not record which side they targeted).
 */
export const filesFromJournal = (journal: SyncJournal): SyncRunFile[] =>
    journal.entries
        .filter((e) => e.action === 'upload' || e.action === 'download')
        .map((e) => ({
            relativePath: e.relative_path,
            action: (e.action === 'upload' ? 'upload' : 'download') as SyncRunAction,
            size: e.bytes_transferred || 0,
            mtime: null,
            overwritesExisting: e.action === 'download',
        }));

/** Map a runner action to the coarse journal action string. */
const journalAction = (action: SyncRunAction): string => {
    if (action === 'upload') return 'upload';
    if (action === 'download') return 'download';
    return 'delete';
};

/** Exponential backoff delay for retry attempt N (1-based). */
const getRetryDelay = (attempt: number, policy: RetryPolicy): number => {
    const d = policy.base_delay_ms * Math.pow(policy.backoff_multiplier, attempt - 1);
    return Math.min(d, policy.max_delay_ms);
};

interface TransferOutcome {
    success: boolean;
    attempts: number;
    error?: SyncErrorInfo;
}

/**
 * The connected-remote sync engine. Executes every entry in `files` with
 * configurable retry, post-transfer verification, archive-before-mutation,
 * orphan deletes and a resumable checkpoint journal, then returns the rich
 * `SyncRunReport`.
 */
export const runRemoteSync = async (
    files: SyncRunFile[],
    dirs: SyncRunDirs,
    config: RemoteSyncConfig,
    callbacks: RemoteSyncCallbacks,
    deps: RemoteSyncDeps,
): Promise<SyncRunReport> => {
    const { invoke } = deps;
    const delay = deps.delay ?? defaultDelay;
    const now = deps.now ?? Date.now;
    const makeId = deps.makeId ?? defaultMakeId;

    const localBase = stripTrailingSlash(config.localRoot);
    const remoteBase = stripTrailingSlash(config.remoteRoot);
    const archivingActive =
        !!config.versioningStrategy && config.versioningStrategy !== 'disabled';

    const setStatus = (path: string, status: SyncRunFileStatus): void => {
        callbacks.onFileStatus?.(path, status);
    };
    const isCancelled = (): boolean => callbacks.isCancelled?.() === true;

    // ── Retry wrapper with error classification ────────────────────────────
    const executeTransferWithRetry = async (
        cmd: string,
        args: Record<string, unknown>,
        filePath: string,
    ): Promise<TransferOutcome> => {
        const maxAttempts = Math.max(1, config.retryPolicy.max_retries);
        for (let attempt = 1; attempt <= maxAttempts; attempt++) {
            try {
                if (config.retryPolicy.timeout_ms > 0) {
                    let timerId: ReturnType<typeof setTimeout>;
                    const timeoutPromise = new Promise<never>((_, reject) => {
                        timerId = setTimeout(
                            () => reject(new Error('Operation timed out')),
                            config.retryPolicy.timeout_ms,
                        );
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
            } catch (err) {
                const rawMsg = (err as { toString?: () => string })?.toString?.()
                    || 'Unknown error';
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
                    setStatus(filePath, 'retrying');
                    if (config.isFtp) {
                        await invoke('ftp_noop').catch(() => undefined);
                    }
                    await delay(getRetryDelay(attempt, config.retryPolicy));
                    continue;
                }
                return { success: false, attempts: attempt, error: errorInfo };
            }
        }
        return { success: false, attempts: maxAttempts };
    };

    // ── Post-transfer verification for downloads ───────────────────────────
    const verifyDownload = async (
        localFilePath: string,
        expectedSize: number,
        expectedMtime: string | null,
    ): Promise<VerifyResult | null> => {
        if (config.verifyPolicy === 'none') return null;
        return invoke<VerifyResult>('verify_local_transfer', {
            localPath: localFilePath,
            expectedSize,
            expectedMtime,
            policy: config.verifyPolicy,
        }).catch(() => null);
    };

    // ── Archive a local file before it is overwritten or deleted ───────────
    const archiveLocalBeforeMutation = async (
        localFilePath: string,
        relativePath: string,
    ): Promise<void> => {
        if (!archivingActive) return;
        try {
            await invoke('archive_before_sync_delete', {
                syncRoot: localBase,
                filePath: localFilePath,
                versioningStrategy: config.versioningStrategy,
            });
        } catch (e) {
            throw new Error(`Archive failed for ${relativePath}: ${String(e)}`);
        }
    };

    // ── Re-key delta stats captured under a basename onto the full path ────
    const captureDeltaForPath = (relativePath: string): void => {
        if (!deps.deltaStats) return;
        const base = basename(relativePath);
        if (base === relativePath) return;
        const stats = deps.deltaStats.get(base);
        if (stats) {
            deps.deltaStats.delete(base);
            deps.deltaStats.set(relativePath, stats);
        }
    };

    const startTime = now();
    let uploaded = 0;
    let downloaded = 0;
    let deleted = 0;
    let skipped = 0;
    let dirsCreated = 0;
    let retried = 0;
    let verifyFailed = 0;
    let totalBytes = 0;
    let runCancelled = false;
    const errors: SyncErrorInfo[] = [];

    // Reset the backend cancel flag: a stale cancel from a prior run would
    // otherwise fail every upload/download immediately.
    await invoke('reset_cancel_flag').catch(() => undefined);

    // Apply bandwidth caps for the duration of the run.
    if ((config.uploadLimitKbps ?? 0) > 0 || (config.downloadLimitKbps ?? 0) > 0) {
        const cmd = config.isFtp ? 'set_speed_limit' : 'provider_set_speed_limit';
        await invoke(cmd, {
            downloadKb: config.downloadLimitKbps ?? 0,
            uploadKb: config.uploadLimitKbps ?? 0,
        }).catch(() => undefined);
    }

    // ── Build (or resume) the checkpoint journal ───────────────────────────
    const journal: SyncJournal = deps.resumeJournal ?? {
        id: makeId(),
        created_at: new Date().toISOString(),
        updated_at: new Date().toISOString(),
        local_path: localBase,
        remote_path: remoteBase,
        direction: config.direction,
        retry_policy: config.retryPolicy,
        verify_policy: config.verifyPolicy,
        entries: files.map<SyncJournalEntry>((f) => ({
            relative_path: f.relativePath,
            action: journalAction(f.action),
            status: 'pending',
            attempts: 0,
            last_error: null,
            verified: null,
            bytes_transferred: 0,
        })),
        completed: false,
    };
    const journalEntryMap = new Map<string, number>();
    journal.entries.forEach((entry, idx) => journalEntryMap.set(entry.relative_path, idx));

    const saveJournal = async (): Promise<void> => {
        await invoke('save_sync_journal_cmd', { journal }).catch(() => undefined);
    };
    await saveJournal();

    callbacks.onProgress?.(0, files.length);

    // ── Pre-create parent directories for file transfers ───────────────────
    const remoteParentDirs = new Set<string>();
    const localParentDirs = new Set<string>();
    for (const f of files) {
        if (!/\//.test(f.relativePath)) continue;
        const parts = f.relativePath.split('/');
        for (let i = 1; i < parts.length; i++) {
            const dir = parts.slice(0, i).join('/');
            if (f.action === 'upload') remoteParentDirs.add(dir);
            if (f.action === 'download') localParentDirs.add(dir);
        }
    }
    const byDepth = (a: string, b: string): number =>
        a.split('/').length - b.split('/').length;
    for (const dir of [...remoteParentDirs].sort(byDepth)) {
        const cmd = config.isProvider ? 'provider_mkdir' : 'create_remote_folder';
        await invoke(cmd, { path: `${remoteBase}/${dir}` }).catch(() => undefined);
    }
    for (const dir of [...localParentDirs].sort(byDepth)) {
        await invoke('create_local_folder', { path: `${localBase}/${dir}` }).catch(
            () => undefined,
        );
    }

    // ── Create standalone empty directories (counted in dirsCreated) ───────
    for (const dir of [...dirs.remote].sort(byDepth)) {
        const cmd = config.isProvider ? 'provider_mkdir' : 'create_remote_folder';
        try {
            await invoke(cmd, { path: `${remoteBase}/${dir}` });
            dirsCreated++;
            setStatus(dir, 'success');
        } catch {
            setStatus(dir, 'skipped');
        }
    }
    for (const dir of [...dirs.local].sort(byDepth)) {
        try {
            await invoke('create_local_folder', { path: `${localBase}/${dir}` });
            dirsCreated++;
            setStatus(dir, 'success');
        } catch {
            setStatus(dir, 'skipped');
        }
    }

    // ── Main per-file loop ─────────────────────────────────────────────────
    let completed = 0;
    for (let i = 0; i < files.length; i++) {
        const item = files[i];
        const entryIdx = journalEntryMap.get(item.relativePath);
        const journalEntry = entryIdx !== undefined ? journal.entries[entryIdx] : undefined;

        // Resume: skip entries the prior run already finished.
        if (
            deps.resumeJournal &&
            (journalEntry?.status === 'completed' || journalEntry?.status === 'skipped')
        ) {
            if (journalEntry.status === 'completed') {
                if (journalEntry.action === 'upload') uploaded++;
                else if (journalEntry.action === 'download') downloaded++;
                else deleted++;
                totalBytes += journalEntry.bytes_transferred;
            } else {
                skipped++;
            }
            completed++;
            callbacks.onProgress?.(completed, files.length);
            continue;
        }

        // Transfer budget: stop once the cap is reached.
        const budgetReached =
            config.transferBudget > 0 && totalBytes >= config.transferBudget;

        if (budgetReached || isCancelled()) {
            runCancelled = true;
            for (let j = i; j < files.length; j++) {
                const jIdx = journalEntryMap.get(files[j].relativePath);
                if (jIdx !== undefined && journal.entries[jIdx]?.status === 'pending') {
                    journal.entries[jIdx].status = 'skipped';
                }
                setStatus(files[j].relativePath, 'skipped');
            }
            skipped += files.length - i;
            await saveJournal();
            break;
        }

        setStatus(item.relativePath, 'syncing');
        if (journalEntry) journalEntry.status = 'in_progress';

        const localFilePath = `${localBase}/${item.relativePath}`;
        const remoteFilePath = `${remoteBase}/${item.relativePath}`;
        let didTransfer = false;

        if (item.action === 'upload') {
            const cmd = config.isProvider ? 'provider_upload_file' : 'upload_file';
            const args = config.isProvider
                ? { localPath: localFilePath, remotePath: remoteFilePath, useDelta: config.deltaSyncEnabled }
                : { params: { local_path: localFilePath, remote_path: remoteFilePath, use_delta: config.deltaSyncEnabled } };
            const result = await executeTransferWithRetry(cmd, args, item.relativePath);
            if (journalEntry) {
                journalEntry.attempts = result.attempts;
                if (result.attempts > 1) retried++;
            }
            if (result.success) {
                didTransfer = true;
                uploaded++;
                totalBytes += item.size;
                if (journalEntry) {
                    journalEntry.status = 'completed';
                    journalEntry.bytes_transferred = item.size;
                    journalEntry.verified = true;
                }
                captureDeltaForPath(item.relativePath);
                setStatus(item.relativePath, 'success');
            } else {
                const errInfo = result.error || {
                    kind: 'unknown' as SyncErrorKind,
                    message: `Upload failed: ${item.relativePath}`,
                    retryable: false,
                    file_path: item.relativePath,
                };
                if (journalEntry) {
                    journalEntry.status = 'failed';
                    journalEntry.last_error = errInfo;
                }
                errors.push(errInfo);
                setStatus(item.relativePath, 'error');
            }
        } else if (item.action === 'download') {
            if (archivingActive && item.overwritesExisting) {
                try {
                    await archiveLocalBeforeMutation(localFilePath, item.relativePath);
                } catch (e) {
                    const archiveMessage = (e as Error)?.message || String(e);
                    const errInfo: SyncErrorInfo = {
                        kind: 'disk_error',
                        message: archiveMessage,
                        retryable: false,
                        file_path: item.relativePath,
                    };
                    if (journalEntry) {
                        journalEntry.status = 'failed';
                        journalEntry.last_error = errInfo;
                    }
                    errors.push(errInfo);
                    setStatus(item.relativePath, 'error');
                    completed++;
                    callbacks.onProgress?.(completed, files.length);
                    continue;
                }
            }
            const cmd = config.isProvider ? 'provider_download_file' : 'download_file';
            const args = config.isProvider
                ? { remotePath: remoteFilePath, localPath: localFilePath, modified: item.mtime || undefined, useDelta: config.deltaSyncEnabled }
                : { params: { remote_path: remoteFilePath, local_path: localFilePath, modified: item.mtime || undefined, use_delta: config.deltaSyncEnabled } };
            const result = await executeTransferWithRetry(cmd, args, item.relativePath);
            if (journalEntry) {
                journalEntry.attempts = result.attempts;
                if (result.attempts > 1) retried++;
            }
            if (result.success) {
                didTransfer = true;
                setStatus(item.relativePath, 'verifying');
                const vResult = await verifyDownload(localFilePath, item.size, item.mtime);
                if (vResult && !vResult.passed) {
                    verifyFailed++;
                    const errInfo: SyncErrorInfo = {
                        kind: 'unknown',
                        message: `${item.relativePath}: ${vResult.message || 'Verification failed'}`,
                        retryable: true,
                        file_path: item.relativePath,
                    };
                    if (journalEntry) {
                        journalEntry.status = 'verify_failed';
                        journalEntry.verified = false;
                        journalEntry.last_error = errInfo;
                    }
                    errors.push(errInfo);
                    setStatus(item.relativePath, 'verify_failed');
                } else {
                    downloaded++;
                    totalBytes += item.size;
                    if (journalEntry) {
                        journalEntry.status = 'completed';
                        journalEntry.bytes_transferred = item.size;
                        journalEntry.verified = true;
                    }
                    captureDeltaForPath(item.relativePath);
                    setStatus(item.relativePath, 'success');
                }
            } else {
                const errInfo = result.error || {
                    kind: 'unknown' as SyncErrorKind,
                    message: `Download failed: ${item.relativePath}`,
                    retryable: false,
                    file_path: item.relativePath,
                };
                if (journalEntry) {
                    journalEntry.status = 'failed';
                    journalEntry.last_error = errInfo;
                }
                errors.push(errInfo);
                setStatus(item.relativePath, 'error');
            }
        } else if (item.action === 'delete-remote' || item.action === 'delete-local') {
            try {
                if (item.action === 'delete-remote') {
                    const cmd = config.isProvider ? 'provider_delete_file' : 'delete_remote_file';
                    const args = config.isProvider
                        ? { path: remoteFilePath }
                        : { path: remoteFilePath, isDir: false };
                    await invoke(cmd, args);
                } else {
                    await archiveLocalBeforeMutation(localFilePath, item.relativePath);
                    await invoke('delete_local_file', { path: localFilePath });
                }
                deleted++;
                didTransfer = true;
                if (journalEntry) journalEntry.status = 'completed';
                setStatus(item.relativePath, 'success');
            } catch (e) {
                const errInfo: SyncErrorInfo = {
                    kind: 'permission_denied',
                    message: `Delete failed: ${item.relativePath} - ${(e as { toString?: () => string })?.toString?.() || 'unknown'}`,
                    retryable: false,
                    file_path: item.relativePath,
                };
                if (journalEntry) {
                    journalEntry.status = 'failed';
                    journalEntry.last_error = errInfo;
                }
                errors.push(errInfo);
                setStatus(item.relativePath, 'error');
            }
        }

        // FTP keep-alive + inter-transfer delay (only after a real transfer).
        if (config.isFtp && didTransfer && i < files.length - 1) {
            if (files.length > 100) {
                if ((i + 1) % 10 === 0) {
                    await invoke('ftp_noop').catch(() => undefined);
                }
                await delay(15);
            } else {
                await invoke('ftp_noop').catch(() => undefined);
                await delay(FTP_TRANSFER_DELAY_MS);
            }
        }

        completed++;
        callbacks.onProgress?.(completed, files.length);

        // Adaptive checkpoint interval to bound journal write I/O.
        const checkpointInterval =
            files.length > 2000 ? 200 : files.length > 500 ? 100 : 10;
        if (completed % checkpointInterval === 0) {
            await saveJournal();
        }
    }

    // ── Finalize the journal ───────────────────────────────────────────────
    journal.completed = !runCancelled;
    await saveJournal();
    if (journal.completed) {
        await invoke('delete_sync_journal_cmd', {
            localPath: localBase,
            remotePath: remoteBase,
        }).catch(() => undefined);
    }

    // ── Aggregate delta savings from the per-file map ──────────────────────
    let delta_savings: DeltaSavingsSummary | undefined;
    if (deps.deltaStats && deps.deltaStats.size > 0) {
        let files_using_delta = 0;
        let total_bytes_sent = 0;
        let total_size = 0;
        for (const s of deps.deltaStats.values()) {
            files_using_delta += 1;
            total_bytes_sent += s.bytes_sent;
            total_size += s.total_size;
        }
        const average_speedup =
            total_bytes_sent > 0 ? total_size / total_bytes_sent : null;
        delta_savings = {
            files_using_delta,
            total_bytes_sent,
            total_size,
            bytes_saved: total_size - total_bytes_sent,
            average_speedup,
        };
    }

    return {
        uploaded,
        downloaded,
        deleted,
        skipped,
        dirsCreated,
        errors,
        verifyFailed,
        retried,
        totalBytes,
        durationMs: now() - startTime,
        cancelled: runCancelled,
        delta_savings,
        delta_bytes_on_wire: delta_savings?.total_bytes_sent,
        delta_batch_files: delta_savings?.files_using_delta,
    };
};
