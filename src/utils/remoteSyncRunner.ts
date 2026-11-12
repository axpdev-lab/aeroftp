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

import { withTransferRun } from './transferRun';
import { QueuedTargetChangedError } from './queuedTarget';
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
    SyncIndex,
    SyncIndexEntry,
    SyncEcStatus,
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
 * Why a folder a Mirror would have removed stayed (round 2 of the 4.2.1
 * review): a folder read as "skipped" said nothing of what it still held.
 */
export type SyncKeptDirReason =
    /** A row under it did not complete (failed, kept, skipped): it holds files that were not synced. */
    | 'unfinished_rows'
    /** Its listing shows entries the plan did not have: excluded or unlisted files. */
    | 'entries'
    /** The server refused it as not empty while its listing shows nothing. */
    | 'server';

/** A folder kept by the run, with the reason. */
export interface SyncKeptDir {
    file_path: string;
    reason: SyncKeptDirReason;
}

/** What `sync_remove_empty_dir` answers, read into an outcome and a reason. */
export function readEmptyDirAnswer(answer: unknown): { outcome: 'removed' } | { outcome: 'kept'; reason: 'entries' | 'server' } {
    switch (answer) {
        case 'removed': return { outcome: 'removed' };
        case 'kept:entries': return { outcome: 'kept', reason: 'entries' };
        case 'kept:server': return { outcome: 'kept', reason: 'server' };
        default: throw new Error(`unexpected answer ${JSON.stringify(answer ?? null)}`);
    }
}

/**
 * One file the runner must act on. `relativePath` is resolved against both
 * `localRoot` and `remoteRoot`; it may be a bare name (flat sync) or contain
 * `/` separators (recursive sync) — the runner pre-creates parent dirs either
 * way.
 */
export interface SyncRunFile {
    /** Exact index entry this operation supersedes; retained across resume. */
    baselineKey?: string;
    relativePath: string;
    action: SyncRunAction;
    /** Source-side size in bytes (best-effort; 0 when unknown). */
    size: number;
    /** Source-side mtime ISO string — used for download verify + journal. */
    mtime: string | null;
    /**
     * True iff the action overwrites an existing local file, so the runner
     * archives it first when a versioning strategy is active. Ignored for
     * `upload` / `delete-remote`; a non-directory `delete-local` always
     * archives regardless of this flag.
     */
    overwritesExisting?: boolean;
    /**
     * True for directory entries. Only meaningful for `delete-remote` /
     * `delete-local`: a directory skips the archive step and is removed only
     * once it is empty (`sync_remove_empty_dir`), never recursively, and not
     * at all when a row under it did not complete this run.
     */
    isDir?: boolean;
    /**
     * Actual source spelling when it differs from the destination, including
     * canonical Unicode counterparts and keep-both renames. Persisted in the
     * journal so resumed transfers address the same source.
     */
    sourcePath?: string;
    /** Remote/source SHA-256 when available; needed for EC verify/repair. */
    expectedSha256?: string | null;
}

export interface ErrorCorrectionRuntime {
    enabled: boolean;
    pct: number;
    maxFileSize?: number;
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
    /**
     * The saved profile of the remote, recorded in the journal and the sync
     * index so that a history serves only its own profile and moves with it
     * when a Proton Drive profile changes client (Requested 79). Absent for a
     * connection with no saved profile and for a local-local run.
     */
    profileId?: string | null;
    /** true → `provider_*` commands; false → ftp/sftp `upload_file`/`download_file`. */
    isProvider: boolean;
    /** true → emit `ftp_noop` keep-alives + inter-transfer delay. */
    isFtp: boolean;
    retryPolicy: RetryPolicy;
    verifyPolicy: VerifyPolicy;
    deltaSyncEnabled: boolean;
    /**
     * Versioned backup: before a destination file is overwritten or deleted,
     * its copy is moved to `<destination root>/<dir>/<stamp>/<path>`
     * (`sync_backup_archive_*`, one stamp per run). A failed backup fails
     * that file and leaves the destination copy where it was. Unset or null
     * turns it off. `dir` must already be validated (`sync_backup_validate`).
     */
    versionedBackup?: { dir: string } | null;
    /** Stop transferring once this many bytes have moved. 0 = unlimited. */
    transferBudget: number;
    /** Stored on the journal for resume bookkeeping. */
    direction: SyncDirection;
    /** Optional bandwidth caps (KB/s, 0/undefined = unlimited) applied at start. */
    uploadLimitKbps?: number;
    downloadLimitKbps?: number;
    /**
     * GAP-9a — when `false`, the checkpoint journal is never persisted (the
     * legacy Maniac mode disables it for raw speed). The in-memory journal
     * object is still maintained so the sync-index merge stays correct.
     * Defaults to `true`.
     */
    journalEnabled?: boolean;
    /**
     * GAP-9a — when `true`, the runner sweeps every completed download with a
     * `size_and_mtime` check after the main loop, regardless of `verifyPolicy`.
     * Maniac runs with verify off during the transfer for speed, then this
     * mandatory pass catches any corruption.
     */
    postSyncVerification?: boolean;
    /**
     * GAP-10 — when `true`, both endpoints are local directories (the Plan
     * tab / Compare-tab mirror on a dual-local AeroFile pair). The runner
     * maps `upload` / `download` to `copy_local_file` between the two roots,
     * `delete-remote` to `delete_local_file`, and the remote-side mkdir
     * pre-pass to `create_local_folder`; it also skips the bandwidth-cap
     * commands, which are meaningless for a filesystem copy. `isProvider`
     * and `isFtp` must both be `false` when this is set.
     */
    isLocalLocal?: boolean;
    /** AeroSync recovery sidecars. P2 wiring only; profile/UI defaults land later. */
    errorCorrection?: ErrorCorrectionRuntime | null;
    /**
     * The remote provider authenticates downloaded content before returning
     * plaintext, so size/mtime verification can be skipped when provider-reported
     * sizes are not plaintext sizes.
     */
    authenticatedRemoteContent?: boolean;
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
    /** The folders a delete row left in place, each with why (counted in `skipped`). */
    keptDirs: SyncKeptDir[];
    /**
     * GAP-9a — present only when `postSyncVerification` ran (Maniac mode). The
     * counts feed the `syncPanel.maniacPostSyncReport` line in the result
     * dialog.
     */
    postSyncVerification?: { ok: number; mismatches: number; failed: number };
    /** Absent when no file travelled the rsync delta path this run. */
    delta_savings?: DeltaSavingsSummary;
    delta_bytes_on_wire?: number;
    delta_batch_files?: number;
    ec_generated: number;
    ec_verified: number;
    ec_repaired: number;
    ec_skipped_too_large: number;
    ec_generate_failed: number;
}

interface SyncEcCommandResult {
    status: SyncEcStatus;
    sidecar_path?: string | null;
    message?: string | null;
}

export interface RemoteSyncCallbacks {
    /**
     * Fired on every per-file status transition for live UI. `message` is the
     * recorded error for `error` / `verify_failed`, undefined otherwise.
     */
    onFileStatus?: (relativePath: string, status: SyncRunFileStatus, message?: string) => void;
    /** Fired after every completed entry: `(done, total)`. */
    onProgress?: (current: number, total: number) => void;
    /** Polled before every entry; return true to stop gracefully. */
    isCancelled?: () => boolean;
}

export interface RemoteSyncDeps {
    /** Tauri command bridge. Injected so the loop is testable. */
    invoke: <T = unknown>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
    /** Immutable connection token and live UI check captured before any prompt. */
    remoteTarget?: { epoch: number; assertCurrent: () => void; ownerBinding?: string };
    /** Optional journal telemetry; ownership is verified by the backend. */
    onJournalCreated?: (id: string) => void;
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
    /**
     * GAP-6: when true, the runner merges the successfully synced entries into
     * the persisted sync index (`save_sync_index_cmd`) at the end of the run,
     * so the next recursive compare is faster and bisync state stays correct.
     */
    writeIndex?: boolean;
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
// Pair identities must remain absolute roots; join prefixes below deliberately
// omit the final slash so that appending /relative never produces //relative.
const normalizedSyncRoot = (path: string): string => /^\/+$/.test(path) ? '/'
    : /^[A-Za-z]:[\\/]$/.test(path) ? path.replace(/\\/g, '/') : stripTrailingSlash(path);

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
            ...(e.source_path ? { sourcePath: e.source_path } : {}),
            ...(e.baseline_key ? { baselineKey: e.baseline_key } : {}),
            action: (e.action === 'upload' ? 'upload' : 'download') as SyncRunAction,
            size: e.action === 'upload' && (e.status === 'completed' || e.status === 'transferred')
                ? (e.local_size ?? e.source_size ?? e.bytes_transferred ?? 0)
                : (e.source_size ?? e.bytes_transferred ?? 0),
            mtime: e.action === 'upload' && (e.status === 'completed' || e.status === 'transferred')
                ? ((e.local_modified !== undefined ? e.local_modified : e.source_mtime) ?? null)
                : (e.source_mtime ?? null),
            expectedSha256: e.source_sha256 ?? null,
            overwritesExisting: e.overwrites_existing ?? (e.action === 'download'),
        }));

/**
 * GAP-8 — derive a `RetryPolicy` from the Plan tab's speed mode. Faster modes
 * push harder: more attempts with a shorter backoff so a flaky transfer is
 * retried quickly rather than stalling the run. `normal` / `fast` keep the
 * conservative 3-retry default.
 *
 * GAP-9a — `maniac` mirrors the legacy `MANIAC_OVERRIDES.retryPolicy`: only 2
 * attempts with a tight backoff and a long per-file timeout, trading recovery
 * depth for raw throughput. Kept inline so this module stays free of any
 * `components/` import.
 */
export const retryPolicyForSpeed = (speedMode: string): RetryPolicy => {
    switch (speedMode) {
        case 'maniac':
            return {
                max_retries: 2,
                base_delay_ms: 250,
                max_delay_ms: 2_000,
                timeout_ms: 300_000,
                backoff_multiplier: 1.5,
            };
        case 'extreme':
            return {
                max_retries: 5,
                base_delay_ms: 250,
                max_delay_ms: 8_000,
                timeout_ms: 120_000,
                backoff_multiplier: 2,
            };
        case 'turbo':
            return {
                max_retries: 4,
                base_delay_ms: 350,
                max_delay_ms: 10_000,
                timeout_ms: 120_000,
                backoff_multiplier: 2,
            };
        default:
            return {
                max_retries: 3,
                base_delay_ms: 500,
                max_delay_ms: 10_000,
                timeout_ms: 120_000,
                backoff_multiplier: 2,
            };
    }
};

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

/** What `provider_sync_transfer_bulk` answers for one file, in the order sent. */
type SyncBulkResult =
    | { status: 'completed' }
    | { status: 'target_changed' }
    /** Final for this run: the file may have reached the server. */
    | { status: 'failed'; message: string }
    /** Back to the file's single-file command. */
    | { status: 'not_attempted' };

/** A file of a bulk group, after its preparation and its transfer. */
type BulkPrepared = SyncBulkResult | { status: 'backup_failed'; error: unknown };

/** Files sent in one `provider_sync_transfer_bulk` call at most. */
const BULK_CHUNK_FILES = 256;

/**
 * The connected-remote sync engine. Executes every entry in `files` with
 * configurable retry, post-transfer verification, archive-before-mutation,
 * orphan deletes and a resumable checkpoint journal, then returns the rich
 * `SyncRunReport`.
 */
export const runRemoteSync = (
    files: SyncRunFile[],
    dirs: SyncRunDirs,
    config: RemoteSyncConfig,
    callbacks: RemoteSyncCallbacks,
    deps: RemoteSyncDeps,
): Promise<SyncRunReport> => withTransferRun(() => runSync(files, dirs, config, callbacks, deps));

const runSync = async (
    requestedFiles: SyncRunFile[],
    dirs: SyncRunDirs,
    config: RemoteSyncConfig,
    callbacks: RemoteSyncCallbacks,
    deps: RemoteSyncDeps,
): Promise<SyncRunReport> => {
    // Pending resumed uploads refresh their source metadata for this attempt;
    // retain the caller's original plan and the journal's prior source proof.
    const files = deps.resumeJournal ? requestedFiles.map(file => ({ ...file })) : requestedFiles;
    const localRoot = normalizedSyncRoot(config.localRoot);
    const remoteRoot = normalizedSyncRoot(config.remoteRoot);
    const resumeEntries = new Map(deps.resumeJournal?.entries.map(entry => [entry.relative_path, entry] as const));
    const intentKey = (entry: { baseline_key?: string | null; relative_path: string }) =>
        (entry.baseline_key ?? entry.relative_path).normalize('NFC');
    const observedIntents = await deps.invoke<Record<string,string> | undefined>('sync_intent_snapshot_cmd', {
        localPath: localRoot, remotePath: remoteRoot,
    }) ?? {};

    if (deps.resumeJournal && (deps.resumeJournal.resume_safe !== true
        || deps.resumeJournal.intent_protocol !== 1
        || (!config.isLocalLocal && !deps.resumeJournal.owner_binding)
        || (!config.isLocalLocal && ((deps.resumeJournal.profile_id ?? null) !== (config.profileId ?? null)))
        || (deps.resumeJournal.backup_dir ?? null) !== (config.versionedBackup?.dir ?? null)
        || (deps.resumeJournal.ec_enabled === true) !== (config.errorCorrection?.enabled === true)
        || (deps.resumeJournal.ec_enabled === true && (deps.resumeJournal.ec_pct !== config.errorCorrection?.pct
            || (deps.resumeJournal.ec_max_file_size ?? null) !== (config.errorCorrection?.maxFileSize ?? null)))
        || deps.resumeJournal.local_path !== localRoot
        || deps.resumeJournal.remote_path !== remoteRoot
        || JSON.stringify(deps.resumeJournal.remote_dirs ?? []) !== JSON.stringify(dirs.remote)
        || JSON.stringify(deps.resumeJournal.local_dirs ?? []) !== JSON.stringify(dirs.local)
        || deps.resumeJournal.entries.some(entry => (entry.archive_state != null && entry.archive_state !== 'started' && entry.archive_state !== 'archived')
            || entry.archive_state === 'started'
            // An in-flight command may have committed before its checkpoint
            // failed. EC tails also lack a frozen content proof: a new compare
            // must protect intervening source/destination edits before repair
            // or sidecar generation can write anything.
            || entry.status === 'in_progress'
            || (entry.status === 'transferred' && (deps.resumeJournal?.ec_enabled === true || entry.action === 'download'))
            || (entry.status !== 'completed' && (entry.intent_token ?? null) !== (observedIntents[intentKey(entry)] ?? null))
            || (entry.archive_state === 'archived' && (!entry.archive_receipt || !deps.resumeJournal?.backup_stamp))
            || (entry.status !== 'completed' && entry.action !== 'upload' && entry.action !== 'download')
            || (!!deps.resumeJournal?.backup_dir && entry.overwrites_existing == null))
        || (!!deps.resumeJournal.backup_dir && files.some(file => {
            const entry = resumeEntries.get(file.relativePath);
            return !entry || entry.action !== journalAction(file.action)
                || entry.overwrites_existing !== (file.overwritesExisting ?? false);
        })))) {
        throw new Error('SYNC_RESUME_NEEDS_COMPARE');
    }
    if (!config.isLocalLocal && (!deps.remoteTarget || !Number.isSafeInteger(deps.remoteTarget.epoch) || deps.remoteTarget.epoch < 0 || !deps.remoteTarget.ownerBinding)) {
        throw new Error('A remote sync requires a bound target');
    }
    let targetError: unknown = null;
    let checkpointError: Error | null = null;
    let transferTimedOut = false;
    const remoteCommands = new Set([
        'load_sync_index_cmd', 'save_sync_index_cmd', 'provider_gui_verify_resume_owner', 'reset_cancel_flag', 'set_speed_limit', 'provider_set_speed_limit', 'provider_bulk_support',
        'provider_sync_transfer_bulk', 'provider_mkdir', 'create_remote_folder',
        'provider_upload_file', 'provider_download_file', 'upload_file', 'download_file',
        'provider_delete_file', 'delete_remote_file', 'sync_backup_archive_remote',
        'sync_backup_verify_receipt',
        'sync_remove_empty_dir', 'sync_ec_generate', 'sync_ec_verify_repair', 'ftp_noop',
    ]);
    const localMutations = new Set(['create_local_folder', 'delete_local_file', 'sync_backup_archive_local']);
    const assertTarget = () => {
        if (targetError) throw targetError;
        try { deps.remoteTarget?.assertCurrent(); }
        catch { targetError = new QueuedTargetChangedError('CONNECTION_CHANGED: reconnect and compare again'); throw targetError; }
    };
    const invoke: RemoteSyncDeps['invoke'] = async (cmd, args) => {
        if (config.isLocalLocal || (!remoteCommands.has(cmd) && !localMutations.has(cmd))) return deps.invoke(cmd, args);
        assertTarget();
        const epoch = deps.remoteTarget!.epoch;
        const params = cmd === 'upload_file' || cmd === 'download_file';
        const guardedArgs = params
            ? { ...args, params: { ...(args?.params as Record<string, unknown>), expected_gui_target: epoch } }
            : { ...args, expectedGuiTarget: epoch, ...(localMutations.has(cmd) ? { isProvider: config.isProvider } : {}) };
        if (cmd === 'load_sync_index_cmd' || cmd === 'save_sync_index_cmd') {
            Object.assign(guardedArgs, { ownerBinding: deps.remoteTarget!.ownerBinding, isProvider: config.isProvider });
        }
        try {
            return await deps.invoke(cmd, guardedArgs);
        } catch (error) {
            let ownerChanged = error instanceof QueuedTargetChangedError;
            if (!ownerChanged && String(error).includes('CONNECTION_CHANGED')) {
                // A server may quote the marker in arbitrary error text.
                // Confirm it through the backend's local ownership endpoint;
                // this endpoint computes a sealed tag without remote I/O.
                try {
                    assertTarget();
                    const binding = await deps.invoke<string>('provider_gui_target_binding', {
                        expectedGuiTarget: epoch, isProvider: config.isProvider,
                    });
                    ownerChanged = binding !== deps.remoteTarget!.ownerBinding;
                } catch { ownerChanged = true; }
            }
            if (ownerChanged) {
                targetError = new QueuedTargetChangedError('CONNECTION_CHANGED: reconnect and compare again');
                throw targetError;
            }
            throw error;
        }
    };
    if (deps.resumeJournal && !config.isLocalLocal) {
        // The backend compares the signed journal's owner with the installed
        // provider under the same target pin. A frontend boolean is no proof.
        await invoke('provider_gui_verify_resume_owner', {
            binding: deps.resumeJournal.owner_binding, isProvider: config.isProvider,
        });
    }
    const delay = deps.delay ?? defaultDelay;
    const now = deps.now ?? Date.now;
    const makeId = deps.makeId ?? defaultMakeId;

    const localBase = stripTrailingSlash(config.localRoot);
    const remoteBase = stripTrailingSlash(config.remoteRoot);
    const errorCorrectionEnabled =
        config.errorCorrection?.enabled === true
        && config.isLocalLocal !== true;

    const setStatus = (path: string, status: SyncRunFileStatus, message?: string): void => {
        callbacks.onFileStatus?.(path, status, message);
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
            // Issue #332: a user Cancel must not trigger further RETRIES of a
            // file. The first attempt is already admitted by the caller's cancel
            // gate (the main loop checks isCancelled before calling), so guard
            // only re-attempts (attempt > 1). The short-circuit also avoids
            // polling isCancelled an extra time on the first attempt.
            if (attempt > 1 && isCancelled()) {
                return { success: false, attempts: attempt - 1 };
            }
            try {
                if (config.retryPolicy.timeout_ms > 0) {
                    let timerId: ReturnType<typeof setTimeout>;
                    const timeoutPromise = new Promise<never>((_, reject) => {
                        timerId = setTimeout(
                            () => { transferTimedOut = true; reject(new Error('Operation timed out')); },
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
                // Promise.race does not cancel IPC. Its write may still commit
                // after this run ends, so no saved status can authorize replay.
                if (transferTimedOut) {
                    journal.resume_safe = false;
                    return { success: false, attempts: attempt, error: {
                        kind: 'unknown', message: String(err), retryable: false, file_path: filePath,
                    } };
                }
                if (targetError) return { success: false, attempts: attempt, error: {
                    kind: 'unknown', message: String(targetError), retryable: false, file_path: filePath,
                } };
                const rawMsg = (err as { toString?: () => string })?.toString?.()
                    || 'Unknown error';
                const errorInfo = await invoke<SyncErrorInfo>('classify_transfer_error', {
                    rawError: rawMsg,
                    filePath,
                }).catch(() => ({
                    kind: 'unknown' as SyncErrorKind,
                    message: 'Transfer failed; compare again before retrying',
                    retryable: false,
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
        }).catch((error) => ({
            path: localFilePath, passed: false, policy: config.verifyPolicy,
            expected_size: expectedSize, actual_size: null, size_match: false,
            mtime_match: null, hash_match: null, message: String(error),
        }));
    };

    // ── Versioned backup: move the destination copy aside first ────────────
    // One stamp for the whole run, asked once, so every copy this run keeps
    // lands in the same `<dir>/<stamp>/` folder.
    let backupStamp: string | null = deps.resumeJournal?.backup_stamp ?? null;
    /** Move one path into this run's backup folder; null when it is not there. */
    const archiveOne = async (
        side: 'local' | 'remote',
        dir: string,
        relativePath: string,
    ): Promise<string | null | undefined> => {
        backupStamp ??= await invoke<string>('sync_backup_run_stamp');
        if (side === 'local' || config.isLocalLocal) {
            return invoke<string | null>('sync_backup_archive_local', {
                root: side === 'local' ? localRoot : remoteRoot,
                dir,
                stamp: backupStamp,
                rel: relativePath,
            });
        }
        return invoke<string | null>('sync_backup_archive_remote', {
            useProvider: config.isProvider,
            root: remoteRoot,
            dir,
            stamp: backupStamp,
            rel: relativePath,
        });
    };
    const archiveBeforeMutation = async (
        side: 'local' | 'remote',
        relativePath: string,
        allowMissing = false,
    ): Promise<void> => {
        const backup = config.versionedBackup;
        if (!backup) return;
        const index = journalEntryMap.get(relativePath);
        const entry = index === undefined ? undefined : journal.entries[index];
        if (!entry) throw new Error(`Backup checkpoint missing for ${relativePath}`);
        const previousReceipt = entry.archive_state === 'archived' ? entry.archive_receipt : null;
        // Persist intent before the remote move. If the process dies before it
        // can record the receipt, a later run must not guess whether it moved.
        backupStamp ??= await invoke<string>('sync_backup_run_stamp');
        journal.backup_stamp = backupStamp;
        entry.archive_state = 'started';
        await saveRequiredCheckpoint();
        let kept: string | null | undefined;
        try {
            kept = await archiveOne(side, backup.dir, relativePath);
        } catch (e) {
            throw new Error(`Backup failed for ${relativePath}: ${String(e)}`);
        }
        if (kept == null && previousReceipt) {
            const verified = await invoke<boolean>('sync_backup_verify_receipt', {
                target: side === 'local' || config.isLocalLocal ? 'local' : config.isProvider ? 'provider' : 'ftp',
                root: side === 'local' ? localRoot : remoteRoot,
                dir: backup.dir, stamp: backupStamp, rel: relativePath, receipt: previousReceipt,
            });
            if (!verified) throw new Error(`Backup receipt could not be verified for ${relativePath}`);
            kept = previousReceipt;
        }
        if (kept == null) {
            // On the local disk "nothing to keep" is a fact: the file is not
            // there, and the transfer replaces nothing. A resumed download
            // meets it when the stopped run had already moved its copy into
            // the backup folder (a resume archives every download again).
            if (side === 'local' || config.isLocalLocal || allowMissing) {
                entry.archive_state = null;
                await saveRequiredCheckpoint();
                return;
            }
            // On a remote it can be a server that answers "not there" for a
            // file it holds: only files the compare saw on the destination
            // reach this point, and writing anyway would lose the one copy
            // the user asked to keep.
            throw new Error(`Backup failed for ${relativePath}: the destination copy was not found, so it was left as it is`);
        }
        entry.archive_state = 'archived';
        entry.archive_receipt = kept;
        await saveRequiredCheckpoint();
        // The `.aerocorrect` parity sidecar belongs to the copy just kept, so
        // it goes into the backup folder with it; most files have none. The
        // compare never lists sidecars, so one left behind would stay beside a
        // file that is gone or replaced for good: if it cannot be moved it is
        // deleted, and the kept copy goes without its parity.
        if (side === 'remote') {
            const sidecarRel = `${relativePath}.aerocorrect`;
            await archiveOne(side, backup.dir, sidecarRel).catch((error) => {
                if (targetError) throw error;
                return deleteRemoteSidecar(`${remoteBase}/${relativePath}`);
            },
            );
        }
    };
    /** Best-effort removal of the `.aerocorrect` sidecar of a destination file. */
    const deleteRemoteSidecar = async (remoteFilePath: string): Promise<void> => {
        const sidecarPath = `${remoteFilePath}.aerocorrect`;
        const sidecarInvoke = config.isLocalLocal
            ? invoke('delete_local_file', { path: sidecarPath })
            : config.isProvider
                ? invoke('provider_delete_file', { path: sidecarPath })
                : invoke('delete_remote_file', { path: sidecarPath, isDir: false });
        await sidecarInvoke.catch((error) => { if (targetError) throw error; });
    };
    /** Record a file refused before writing, by the backup step or for its name:
     *  its destination copy is untouched. */
    const failBackup = (item: SyncRunFile, journalEntry: SyncJournalEntry | undefined, e: unknown): void => {
        const errInfo: SyncErrorInfo = {
            kind: 'disk_error',
            message: (e as Error)?.message || String(e),
            retryable: false,
            file_path: item.relativePath,
        };
        if (journalEntry) {
            journalEntry.status = 'failed';
            journalEntry.last_error = errInfo;
        }
        errors.push(errInfo);
        setStatus(item.relativePath, 'error', errInfo.message);
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
    const keptDirs: SyncKeptDir[] = [];
    let dirsCreated = 0;
    let retried = 0;
    let verifyFailed = 0;
    let totalBytes = 0;
    let ecGenerated = 0;
    let ecVerified = 0;
    let ecRepaired = 0;
    let ecSkippedTooLarge = 0;
    let ecGenerateFailed = 0;
    let runCancelled = false;
    const errors: SyncErrorInfo[] = [];

    const recordEcStatus = (
        status: SyncEcStatus,
        journalEntry?: SyncJournalEntry,
    ): void => {
        if (journalEntry) journalEntry.ec_status = status;
        if (status === 'generated') ecGenerated++;
        else if (status === 'verified') ecVerified++;
        else if (status === 'repaired') ecRepaired++;
        else if (status === 'skipped_too_large') ecSkippedTooLarge++;
        else if (status === 'generate_failed') ecGenerateFailed++;
    };

    // Reset the backend cancel flag: a stale cancel from a prior run would
    // otherwise fail every upload/download immediately.
    await invoke('reset_cancel_flag').catch(() => undefined);

    // Apply bandwidth caps for the duration of the run. Skipped entirely for
    // a local-local run: throttling a filesystem copy has no meaning.
    if (
        !config.isLocalLocal
        && ((config.uploadLimitKbps ?? 0) > 0 || (config.downloadLimitKbps ?? 0) > 0)
    ) {
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
        local_path: localRoot,
        remote_path: remoteRoot,
        ...(config.profileId ? { profile_id: config.profileId } : {}),
        resume_safe: true,
        intent_protocol: 1,
        ...(!config.isLocalLocal ? { owner_binding: deps.remoteTarget!.ownerBinding } : {}),
        backup_dir: config.versionedBackup?.dir ?? null,
        backup_stamp: null,
        ec_enabled: errorCorrectionEnabled,
        ec_pct: errorCorrectionEnabled ? config.errorCorrection!.pct : null,
        ec_max_file_size: errorCorrectionEnabled ? config.errorCorrection!.maxFileSize ?? null : null,
        remote_dirs: [...dirs.remote],
        local_dirs: [...dirs.local],
        direction: config.direction,
        retry_policy: config.retryPolicy,
        verify_policy: config.verifyPolicy,
        entries: files.map<SyncJournalEntry>((f) => ({
            relative_path: f.relativePath,
            intent_token: observedIntents[(f.baselineKey ?? f.relativePath).normalize('NFC')] ?? null,
            overwrites_existing: f.overwritesExisting ?? false,
            source_size: f.size,
            source_mtime: f.mtime,
            source_sha256: f.expectedSha256 ?? null,
            source_path: f.sourcePath ?? null,
            ...(f.baselineKey != null ? { baseline_key: f.baselineKey } : {}),
            action: journalAction(f.action),
            status: 'pending',
            attempts: 0,
            last_error: null,
            verified: null,
            bytes_transferred: 0,
            ec_status: null,
        })),
        completed: false,
    };
    if (!deps.resumeJournal) deps.onJournalCreated?.(journal.id);
    const journalEntryMap = new Map<string, number>();
    // The local file of each transfer as the index records it, for the files
    // this run knows no time of (a remote that lists none, a run resumed from
    // its journal): read when a download completed, and before an upload.
    const landedStates = new Map<string, { size: number | null; modified: string | null }>();
    // When each upload ended. The index reads a remote date up to this moment as
    // the stamp of our own upload, so a remote edit made later in the run stays
    // an edit. A bulk reports its files together, so they share the end of its
    // call. The journal keeps it for a resumed run, which sees no such upload.
    const uploadsEnded = new Map<string, string>();
    const readLocalState = async (path: string) => {
        const props = await invoke<{ size?: number; modified: string | null } | undefined>(
            'get_file_properties',
            { path },
        ).catch(() => undefined);
        // get_file_properties formats UTC without the zone.
        return {
            size: typeof props?.size === 'number' ? props.size : null,
            modified: props?.modified ? `${props.modified}Z` : null,
        };
    };
    // What the index records for a completed transfer, kept in its journal
    // entry too: a run resumed from the journal reads nothing of the
    // transfers it skips, and takes this for them.
    const recordLanded = (item: SyncRunFile, entry: SyncJournalEntry | undefined) => {
        if (!entry) return;
        const landed = landedStates.get(item.relativePath);
        entry.local_size = landed?.size ?? item.size;
        entry.local_modified = landed ? landed.modified : item.mtime;
        const ended = uploadsEnded.get(item.relativePath);
        if (ended) entry.uploaded_at = ended;
    };
    journal.entries.forEach((entry, idx) => journalEntryMap.set(entry.relative_path, idx));

    const beginIntents = async (entries: SyncJournalEntry[]): Promise<void> => {
        try {
            const tokens = await invoke<string[]>('sync_intent_begin_cmd', {
                localPath: localRoot, remotePath: remoteRoot, paths: entries.map(intentKey),
            });
            if (!Array.isArray(tokens) || tokens.length !== entries.length || tokens.some(token => typeof token !== 'string' || !token)) {
                throw new Error('Invalid durable sync intent response');
            }
            entries.forEach((entry,index) => { entry.intent_token=tokens[index]; });
        } catch (error) {
            checkpointError = new Error(`SYNC_RESUME_NEEDS_COMPARE: intent failed: ${String(error)}`);
            journal.resume_safe = false;
            throw checkpointError;
        }
    };

    // GAP-9a — Maniac mode disables journal persistence. The in-memory
    // `journal` object is still maintained (it backs the sync-index merge);
    // only the `save_sync_journal_cmd` round-trips are skipped.
    const journalEnabled = config.journalEnabled !== false;
    const saveJournal = async (): Promise<void> => {
        if (!journalEnabled) return;
        await invoke('save_sync_journal_cmd', { journal }).catch(() => undefined);
    };
    // Unlike routine progress, archive and acknowledgement checkpoints must persist before
    // another phase can act on the destination.
    const saveRequiredCheckpoint = async (): Promise<void> => {
        if (!journalEnabled) return;
        try { await invoke('save_sync_journal_cmd', { journal }); }
        catch (error) {
            checkpointError = new Error(`SYNC_RESUME_NEEDS_COMPARE: checkpoint failed: ${String(error)}`);
            journal.resume_safe = false;
            throw checkpointError;
        }
    };
    await saveJournal();

    callbacks.onProgress?.(0, files.length);

    // ── Native bulk (Requested 77) ─────────────────────────────────────────
    // A provider that sends several files in one native call (Proton Drive
    // in CLI mode starts one program per call) gets the uploads and the
    // downloads of the run that come in a row as one call per group. The
    // capability is not in the run's config, so it is asked once here: a
    // provider without the bulk pays this one call, and every file of the
    // run keeps its single-file command.
    let bulk: { upload: boolean; download: boolean } | null = null;
    if (config.isProvider && !config.isFtp && !config.isLocalLocal) {
        const support = await invoke<{ upload: boolean; download: boolean } | null>('provider_bulk_support')
            .catch(() => null);
        bulk = support && (support.upload || support.download) ? support : null;
    }
    // Requested 124: a name this disk cannot hold, or cannot tell from another
    // one in this run, fails on its own and leaves nothing behind. Written
    // anyway, two files would become one and the contents of the first be lost.
    const localNameProblems = new Map<string, string>();
    const downloads = files.filter((f) => f.action === 'download' && !f.isDir).map((f) => f.relativePath);
    const localTargets = [...downloads, ...dirs.local];
    if (localTargets.length > 0) {
        try {
            const problems = await invoke<{ path: string; reason: string }[]>('local_target_problems', {
                destination: localRoot,
                paths: localTargets,
                files: downloads,
            });
            if (Array.isArray(problems)) {
                problems.forEach((problem) => localNameProblems.set(problem.path, problem.reason));
            }
        } catch (error) {
            // Not knowing is not a pass: writing could lose a file.
            localTargets.forEach((path) => localNameProblems.set(path, `Could not check the local name: ${String(error)}`));
        }
    }
    // On a local pair an upload writes to a disk too, with the same limits.
    const localPairUploads = config.isLocalLocal
        ? files.filter((f) => f.action === 'upload' && !f.isDir).map((f) => f.relativePath)
        : [];
    if (localPairUploads.length > 0) {
        try {
            const problems = await invoke<{ path: string; reason: string }[]>('local_target_problems', {
                destination: remoteRoot,
                paths: localPairUploads,
                files: localPairUploads,
            });
            if (Array.isArray(problems)) {
                problems.forEach((problem) => localNameProblems.set(problem.path, problem.reason));
            }
        } catch (error) {
            localPairUploads.forEach((path) => localNameProblems.set(path, `Could not check the local name: ${String(error)}`));
        }
    }
    const bulkEligible = (f: SyncRunFile): boolean =>
        bulk !== null
        && !f.isDir
        && !localNameProblems.has(f.relativePath)
        // A source spelled differently (keep-both, Unicode counterpart) keeps
        // the single-file command, which writes under the destination name.
        && f.sourcePath == null
        && ((f.action === 'upload' && bulk.upload) || (f.action === 'download' && bulk.download));
    const bulkPrepared = new Map<string, BulkPrepared>();
    const bulkPreparationRan = new Set<string>();
    /**
     * Send the files from `start` on that go the same way and can travel
     * together, up to the one that reaches the transfer budget (the last file
     * the budget lets through, as on the single-file path). Each is prepared
     * as its single-file transfer would be (backup of the copy it replaces,
     * the local state an upload records), then one call sends them. The main
     * loop reads the outcomes in plan order and finishes each file as before.
     */
    const runBulkGroup = async (start: number): Promise<void> => {
        const action = files[start].action;
        const group: SyncRunFile[] = [];
        let bytes = totalBytes;
        for (let j = start; j < files.length && group.length < BULK_CHUNK_FILES; j++) {
            const f = files[j];
            if (f.action !== action || !bulkEligible(f) || bulkPrepared.has(f.relativePath)) break;
            const idx = journalEntryMap.get(f.relativePath);
            if (deps.resumeJournal && idx !== undefined && ['completed', 'transferred'].includes(journal.entries[idx].status)) break;
            if (config.transferBudget > 0 && bytes >= config.transferBudget) break;
            bytes += f.size;
            group.push(f);
        }
        if (group.length < 2) {
            bulkPrepared.set(files[start].relativePath, { status: 'not_attempted' });
            return;
        }
        const send: SyncRunFile[] = [];
        await beginIntents(group.map(file => journal.entries[journalEntryMap.get(file.relativePath)!]));
        for (const f of group) {
            // A Stop during the preparation leaves the files not prepared yet
            // as they are: nothing moved into the backup folder, nothing
            // marked in progress.
            if (isCancelled()) break;
            const idx = journalEntryMap.get(f.relativePath);
            setStatus(f.relativePath, 'syncing');
            if (idx !== undefined) journal.entries[idx].status = 'in_progress';
            if (config.versionedBackup && (f.overwritesExisting || deps.resumeJournal)) {
                try {
                    await archiveBeforeMutation(action === 'upload' ? 'remote' : 'local', f.relativePath, !f.overwritesExisting);
                } catch (error) {
                    if (targetError || checkpointError) throw error;
                    bulkPrepared.set(f.relativePath, { status: 'backup_failed', error });
                    continue;
                }
            }
            if (action === 'upload' && deps.writeIndex && (f.mtime == null || deps.resumeJournal)) {
                const source = await readLocalState(`${localBase}/${f.relativePath}`);
                landedStates.set(f.relativePath, source);
                if (deps.resumeJournal) {
                    f.size = source.size ?? f.size;
                    f.mtime = source.modified;
                }
            }
            bulkPreparationRan.add(f.relativePath);
            send.push(f);
        }
        try { await saveRequiredCheckpoint(); }
        catch {
            journal.resume_safe = false;
            checkpointError = new Error('SYNC_RESUME_NEEDS_COMPARE: transfer intent checkpoint failed');
            return;
        }
        let results: SyncBulkResult[] | null = null;
        // Nor does anything go after a Stop. The files prepared stay pending
        // for a resume, their replaced copies in the backup folder.
        if (send.length > 0 && !isCancelled()) {
            try {
                results = await invoke<SyncBulkResult[]>('provider_sync_transfer_bulk', {
                    upload: action === 'upload',
                    items: send.map((f) => ({
                        localPath: `${localBase}/${f.relativePath}`,
                        remotePath: `${remoteBase}/${f.relativePath}`,
                        size: f.size,
                        modified: action === 'download' ? (f.mtime || null) : null,
                    })),
                    // The single-file timeout, per file: the bulk's own limit
                    // grows with its files. No race here: a timer in the page
                    // would leave the native program running.
                    timeoutSeconds: config.retryPolicy.timeout_ms > 0
                        ? Math.ceil(config.retryPolicy.timeout_ms / 1000)
                        : null,
                });
            } catch (error) {
                if (targetError) throw error;
                // Refused before any file went (connection changed, no bulk
                // any more): these files and the rest of the run go one by one.
                bulk = null;
            }
        }
        const bulkEnded = new Date().toISOString();
        send.forEach((f, k) => {
            if (action === 'upload' && results?.[k]?.status === 'completed') uploadsEnded.set(f.relativePath, bulkEnded);
            bulkPrepared.set(f.relativePath, results?.[k] ?? { status: 'not_attempted' });
        });
        // Native acknowledgements are facts even when the UI changed while
        // this command held its backend pin. Persist every completed transfer
        // BEFORE checking the live target or performing any follow-up work.
        const ownerChanged = results?.some(result => result.status === 'target_changed');
        if (ownerChanged) targetError = new QueuedTargetChangedError('CONNECTION_CHANGED: reconnect and compare again');
        for (const [k, file] of send.entries()) {
            const result = results?.[k];
            const index = journalEntryMap.get(file.relativePath);
            if (index === undefined) continue;
            const entry = journal.entries[index];
            if (result?.status === 'completed') {
                entry.status = 'transferred';
                entry.attempts = 1;
                entry.bytes_transferred = file.size;
                entry.verified = null;
                if (action === 'download' && deps.writeIndex && file.mtime == null) {
                    landedStates.set(file.relativePath, await readLocalState(`${localBase}/${file.relativePath}`));
                }
                recordLanded(file, entry);
            } else if (!results || result?.status === 'not_attempted') {
                entry.status = 'pending';
            } else if (ownerChanged) {
                entry.status = 'failed';
                entry.last_error = result?.status === 'target_changed' || result?.status === 'failed'
                    ? { kind: 'unknown', message: result.status === 'target_changed' ? String(targetError) : result.message,
                        retryable: false, file_path: file.relativePath } : null;
            }
        }
        try {
            await saveRequiredCheckpoint();
        } catch {
            // The on-disk write-ahead snapshot remains in_progress, which is
            // rejected on Resume even if this unsafe marker also cannot save.
            journal.resume_safe = false;
            await saveJournal();
            checkpointError = new Error('SYNC_RESUME_NEEDS_COMPARE: acknowledgement checkpoint failed');
        }
    };
    /** The outcome of a file the bulk sent or failed, as the single-file path reports it. */
    const bulkOutcome = async (result: SyncBulkResult, filePath: string): Promise<TransferOutcome> => {
        if (result.status === 'completed') return { success: true, attempts: 1 };
        const rawError = result.status === 'target_changed' ? 'CONNECTION_CHANGED'
            : result.status === 'failed' ? result.message : 'Not sent';
        const error = await invoke<SyncErrorInfo>('classify_transfer_error', { rawError, filePath })
            .catch(() => ({
                kind: 'unknown' as SyncErrorKind,
                message: rawError,
                retryable: false,
                file_path: filePath,
            }));
        return { success: false, attempts: 1, error };
    };

    // ── Pre-create parent directories for file transfers ───────────────────
    const remoteParentDirs = new Set<string>();
    const localParentDirs = new Set<string>();
    for (const f of files) {
        if (!/\//.test(f.relativePath)) continue;
        const parts = f.relativePath.split('/');
        for (let i = 1; i < parts.length; i++) {
            const dir = parts.slice(0, i).join('/');
            if (f.action === 'upload') remoteParentDirs.add(dir);
            if (f.action === 'download' && !localNameProblems.has(f.relativePath)) localParentDirs.add(dir);
        }
    }
    const byDepth = (a: string, b: string): number =>
        a.split('/').length - b.split('/').length;
    // GAP-10: a local-local run creates the "remote" side with the same
    // local-filesystem command as the "local" side.
    const remoteMkdirCmd = config.isLocalLocal
        ? 'create_local_folder'
        : config.isProvider
            ? 'provider_mkdir'
            : 'create_remote_folder';
    // Count the parent directories we actually create (mkdir succeeds) into
    // `dirsCreated`, mirroring the standalone-dir loop below. Without this the
    // receipt only counted standalone empty dirs, so a mirror of `docs/a.txt`
    // reported "1 file" while the compare reported 2 differences (the file + the
    // `docs` folder). An mkdir that throws (the dir already exists, or a real
    // error) is not a new folder, so it is not counted -- the receipt now
    // reconciles with the compare's directory-inclusive difference count.
    for (const dir of [...remoteParentDirs].sort(byDepth)) {
        if (targetError) break;
        try {
            await invoke(remoteMkdirCmd, { path: `${remoteBase}/${dir}` });
            dirsCreated++;
        } catch {
            // Already exists or failed: not a newly-created folder.
        }
    }
    for (const dir of [...localParentDirs].sort(byDepth)) {
        if (targetError) break;
        try {
            await invoke('create_local_folder', { path: `${localBase}/${dir}` });
            dirsCreated++;
        } catch {
            // Already exists or failed: not a newly-created folder.
        }
    }

    // ── Create standalone empty directories (counted in dirsCreated) ───────
    for (const dir of [...dirs.remote].sort(byDepth)) {
        if (targetError) break;
        try {
            await invoke(remoteMkdirCmd, { path: `${remoteBase}/${dir}` });
            dirsCreated++;
            setStatus(dir, 'success');
        } catch {
            setStatus(dir, 'skipped');
        }
    }
    for (const dir of [...dirs.local].sort(byDepth)) {
        if (targetError) break;
        const problem = localNameProblems.get(dir);
        if (problem) {
            const errInfo: SyncErrorInfo = {
                kind: 'disk_error',
                message: problem,
                retryable: false,
                file_path: dir,
            };
            errors.push(errInfo);
            setStatus(dir, 'error', problem);
            continue;
        }
        try {
            await invoke('create_local_folder', { path: `${localBase}/${dir}` });
            dirsCreated++;
            setStatus(dir, 'success');
        } catch {
            setStatus(dir, 'skipped');
        }
    }

    // ── Folders that hold a row which did not complete ─────────────────────
    // Deletes run deepest first, so every row under a folder row comes before
    // it. The rows are read once, in order, up to the folder row asking; a row
    // whose journal entry is not `completed` (failed, kept, skipped) holds
    // every folder above it.
    const heldDirs = new Set<string>();
    let heldScanned = 0;
    const holdsUnfinishedRow = (dir: string, upTo: number): boolean => {
        for (; heldScanned < upTo; heldScanned++) {
            const path = files[heldScanned].relativePath;
            const idx = journalEntryMap.get(path);
            if (idx !== undefined && journal.entries[idx].status === 'completed') continue;
            const parts = path.split('/').filter(Boolean);
            for (let k = 1; k < parts.length; k++) heldDirs.add(parts.slice(0, k).join('/'));
        }
        return heldDirs.has(dir.split('/').filter(Boolean).join('/'));
    };

    // ── Main per-file loop ─────────────────────────────────────────────────
    let completed = 0;
    for (let i = 0; i < files.length; i++) {
        if (!config.isLocalLocal && !targetError) {
            try { assertTarget(); } catch { /* stop and checkpoint below */ }
        }
        if (targetError || checkpointError || transferTimedOut) {
            runCancelled = true;
            for (let j = i; j < files.length; j++) setStatus(files[j].relativePath, 'skipped');
            skipped += files.length - i;
            break;
        }
        const item = files[i];
        const entryIdx = journalEntryMap.get(item.relativePath);
        const journalEntry = entryIdx !== undefined ? journal.entries[entryIdx] : undefined;

        // Resume: skip only entries the prior run already finished.
        // `skipped` is retried: cancellation used to stamp remaining pending
        // entries as skipped, and journals already on disk still have that
        // status, so treating skipped like completed would drop the files
        // Resume is supposed to transfer.
        if (deps.resumeJournal && journalEntry?.status === 'completed') {
            if (journalEntry.action === 'upload') {
                uploaded++;
                if (journalEntry.uploaded_at) uploadsEnded.set(item.relativePath, journalEntry.uploaded_at);
            } else if (journalEntry.action === 'download') downloaded++;
            else deleted++;
            totalBytes += journalEntry.bytes_transferred;
            completed++;
            callbacks.onProgress?.(completed, files.length);
            continue;
        }

        const nameProblem = item.action === 'download' || (item.action === 'upload' && config.isLocalLocal)
            ? localNameProblems.get(item.relativePath)
            : undefined;
        if (nameProblem) {
            failBackup(item, journalEntry, new Error(nameProblem));
            completed++;
            callbacks.onProgress?.(completed, files.length);
            continue;
        }

        // A file the bulk already sent (or failed) is finished below as it
        // is, whatever happened since: it went before the budget or a Stop.
        const transferAcknowledged = journalEntry?.status === 'transferred';
        if (transferAcknowledged && journalEntry?.uploaded_at) uploadsEnded.set(item.relativePath, journalEntry.uploaded_at);
        if (!transferAcknowledged && bulkEligible(item) && !bulkPrepared.has(item.relativePath)
            && !(config.transferBudget > 0 && totalBytes >= config.transferBudget) && !isCancelled()) {
            try { await runBulkGroup(i); } catch (error) { if (!targetError && !checkpointError) throw error; }
            if (!targetError) { try { assertTarget(); } catch { /* acknowledgements are already durable */ } }
            if (targetError || checkpointError) { i--; continue; }
        }
        const fromBulk: BulkPrepared | undefined = transferAcknowledged ? { status: 'completed' } : bulkPrepared.get(item.relativePath);
        const sentByBulk = fromBulk !== undefined && fromBulk.status !== 'not_attempted';
        const preparedByBulk = bulkPreparationRan.has(item.relativePath);

        // Transfer budget: stop once the cap is reached.
        const budgetReached =
            config.transferBudget > 0 && totalBytes >= config.transferBudget;

        if (!sentByBulk && (budgetReached || isCancelled())) {
            runCancelled = true;
            for (let j = i; j < files.length; j++) {
                // Leave journal entries pending so a later resume transfers
                // them. The live UI of this run still shows skipped.
                setStatus(files[j].relativePath, 'skipped');
            }
            skipped += files.length - i;
            await saveJournal();
            break;
        }

        setStatus(item.relativePath, 'syncing');
        if (journalEntry && !transferAcknowledged) journalEntry.status = 'in_progress';
        // Deletes also invalidate the old baseline before their first effect.
        // A later timeout/index failure must not revive deletion authority for
        // a new file that appears at the deleted path.
        if (!sentByBulk && !transferAcknowledged && journalEntry) {
            try { await beginIntents([journalEntry]); }
            catch { i--; continue; }
        }


        // GAP-7: for a keep-both rename the source side differs from the
        // destination. `relativePath` is always the destination; `sourcePath`
        // (when set) is the source. Normal entries share `relativePath`.
        const srcRel = item.sourcePath ?? item.relativePath;
        const localFilePath = item.action === 'download'
            ? `${localBase}/${item.relativePath}`
            : `${localBase}/${srcRel}`;
        const remoteFilePath = item.action === 'upload'
            ? `${remoteBase}/${item.relativePath}`
            : `${remoteBase}/${srcRel}`;
        let didTransfer = false;

        if (item.action === 'upload') {
            if (fromBulk?.status === 'backup_failed') {
                failBackup(item, journalEntry, fromBulk.error);
                completed++;
                callbacks.onProgress?.(completed, files.length);
                continue;
            }
            if (!sentByBulk && !preparedByBulk && config.versionedBackup && (item.overwritesExisting || deps.resumeJournal)) {
                try {
                    await archiveBeforeMutation('remote', item.relativePath, !item.overwritesExisting);
                } catch (e) {
                    if (checkpointError) { i--; continue; }
                    failBackup(item, journalEntry, e);
                    completed++;
                    callbacks.onProgress?.(completed, files.length);
                    continue;
                }
            }
            // GAP-10: a local-local upload is a filesystem copy left → right.
            let cmd: string;
            let args: Record<string, unknown>;
            if (config.isLocalLocal) {
                cmd = 'copy_local_file';
                args = { from: localFilePath, to: remoteFilePath };
            } else if (config.isProvider) {
                cmd = 'provider_upload_file';
                args = { localPath: localFilePath, remotePath: remoteFilePath, useDelta: config.deltaSyncEnabled };
            } else {
                cmd = 'upload_file';
                args = { params: { local_path: localFilePath, remote_path: remoteFilePath, use_delta: config.deltaSyncEnabled } };
            }
            // A run resumed from its journal knows no time of the file it
            // uploads: read it before the upload, the copy that goes up.
            if (!sentByBulk && !preparedByBulk && deps.writeIndex && (item.mtime == null || deps.resumeJournal)) {
                const source = await readLocalState(localFilePath);
                landedStates.set(item.relativePath, source);
                if (deps.resumeJournal) {
                    item.size = source.size ?? item.size;
                    item.mtime = source.modified;
                }
            }
            const result = sentByBulk
                ? await bulkOutcome(fromBulk, item.relativePath)
                : await executeTransferWithRetry(cmd, args, item.relativePath);
            if (journalEntry) {
                journalEntry.attempts = result.attempts;
                if (result.attempts > 1) retried++;
            }
            if (result.success) {
                didTransfer = true;
                uploaded++;
                if (!sentByBulk) uploadsEnded.set(item.relativePath, new Date().toISOString());
                totalBytes += item.size;
                if (journalEntry) {
                    journalEntry.status = 'transferred';
                    journalEntry.bytes_transferred = item.size;
                    journalEntry.verified = true;
                }
                if (!transferAcknowledged) recordLanded(item, journalEntry);
                if (errorCorrectionEnabled) {
                    try {
                        const ecResult = await invoke<SyncEcCommandResult>('sync_ec_generate', {
                            localPath: localFilePath,
                            remotePath: remoteFilePath,
                            relativePath: item.relativePath,
                            pct: config.errorCorrection?.pct,
                            maxFileSize: config.errorCorrection?.maxFileSize,
                            isProvider: config.isProvider,
                        });
                        recordEcStatus(ecResult.status, journalEntry);
                    } catch {
                        recordEcStatus('generate_failed', journalEntry);
                    }
                }
                if (targetError) {
                    if (journalEntry) journalEntry.status = 'transferred';
                    setStatus(item.relativePath, 'error', String(targetError));
                    continue;
                }
                if (journalEntry) journalEntry.status = 'completed';
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
                setStatus(item.relativePath, 'error', errInfo.message);
            }
        } else if (item.action === 'download') {
            if (fromBulk?.status === 'backup_failed') {
                failBackup(item, journalEntry, fromBulk.error);
                completed++;
                callbacks.onProgress?.(completed, files.length);
                continue;
            }
            if (!sentByBulk && !preparedByBulk && config.versionedBackup && (item.overwritesExisting || deps.resumeJournal)) {
                try {
                    await archiveBeforeMutation('local', item.relativePath, !item.overwritesExisting);
                } catch (e) {
                    if (checkpointError) { i--; continue; }
                    failBackup(item, journalEntry, e);
                    completed++;
                    callbacks.onProgress?.(completed, files.length);
                    continue;
                }
            }
            // GAP-10: a local-local download is a filesystem copy right → left.
            let cmd: string;
            let args: Record<string, unknown>;
            if (config.isLocalLocal) {
                cmd = 'copy_local_file';
                args = { from: remoteFilePath, to: localFilePath };
            } else if (config.isProvider) {
                cmd = 'provider_download_file';
                args = { remotePath: remoteFilePath, localPath: localFilePath, modified: item.mtime || undefined, useDelta: config.deltaSyncEnabled };
            } else {
                cmd = 'download_file';
                args = { params: { remote_path: remoteFilePath, local_path: localFilePath, modified: item.mtime || undefined, use_delta: config.deltaSyncEnabled } };
            }
            const result = sentByBulk
                ? await bulkOutcome(fromBulk, item.relativePath)
                : await executeTransferWithRetry(cmd, args, item.relativePath);
            if (journalEntry) {
                journalEntry.attempts = result.attempts;
                if (result.attempts > 1) retried++;
            }
            if (result.success) {
                didTransfer = true;
                if (journalEntry) {
                    journalEntry.status = 'transferred';
                    journalEntry.bytes_transferred = item.size;
                }
                setStatus(item.relativePath, 'verifying');
                if (errorCorrectionEnabled) {
                    try {
                        const ecResult = await invoke<SyncEcCommandResult>('sync_ec_verify_repair', {
                            localPath: localFilePath,
                            remotePath: remoteFilePath,
                            relativePath: srcRel,
                            expectedSha256: item.expectedSha256 ?? null,
                            expectedMtime: item.mtime || undefined,
                            isProvider: config.isProvider,
                        });
                        recordEcStatus(ecResult.status, journalEntry);
                    } catch {
                        recordEcStatus('verify_failed', journalEntry);
                    }
                }
                if (targetError) {
                    if (journalEntry) journalEntry.status = 'transferred';
                    setStatus(item.relativePath, 'error', String(targetError));
                    continue;
                }
                const vResult = config.authenticatedRemoteContent
                    ? null
                    : await verifyDownload(localFilePath, item.size, item.mtime);
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
                    setStatus(item.relativePath, 'verify_failed', errInfo.message);
                } else {
                    // Read now, not when the index is saved after the run: a
                    // same-size edit made in between is a change the next run
                    // must see, not the synced state.
                    if (deps.writeIndex && item.mtime == null) {
                        landedStates.set(item.relativePath, await readLocalState(localFilePath));
                    }
                    downloaded++;
                    totalBytes += item.size;
                    if (journalEntry) {
                        journalEntry.status = 'completed';
                        journalEntry.bytes_transferred = item.size;
                        journalEntry.verified = true;
                    }
                    recordLanded(item, journalEntry);
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
                setStatus(item.relativePath, 'error', errInfo.message);
            }
        } else if ((item.action === 'delete-remote' || item.action === 'delete-local') && item.isDir) {
            // A folder is removed only when it is empty, and not even tried
            // when a row under it did not complete: a file whose move into the
            // backup folder failed is still in it (B2), and so is a file the
            // compare excluded, which has no row at all (H6). A recursive
            // delete took both along. What stays keeps its folder, and every
            // folder above it, as skipped rather than failed.
            let outcome: 'removed' | 'kept' | 'failed' = 'kept';
            let keptReason: SyncKeptDirReason = 'unfinished_rows';
            if (!holdsUnfinishedRow(item.relativePath, i)) {
                const side = item.action === 'delete-remote' ? 'remote' : 'local';
                try {
                    const answer = readEmptyDirAnswer(await invoke<unknown>('sync_remove_empty_dir', {
                        target: side === 'local' || config.isLocalLocal
                            ? 'local'
                            : config.isProvider ? 'provider' : 'ftp',
                        path: side === 'remote' ? remoteFilePath : localFilePath,
                    }));
                    outcome = answer.outcome;
                    if (answer.outcome === 'kept') keptReason = answer.reason;
                } catch (e) {
                    outcome = 'failed';
                    const errInfo: SyncErrorInfo = {
                        kind: 'unknown',
                        message: `Delete failed: ${item.relativePath} - ${(e as { toString?: () => string })?.toString?.() || 'unknown'}`,
                        retryable: false,
                        file_path: item.relativePath,
                    };
                    if (journalEntry) {
                        journalEntry.status = 'failed';
                        journalEntry.last_error = errInfo;
                    }
                    errors.push(errInfo);
                    setStatus(item.relativePath, 'error', errInfo.message);
                }
            }
            if (outcome === 'removed') {
                deleted++;
                didTransfer = true;
                if (journalEntry) journalEntry.status = 'completed';
                setStatus(item.relativePath, 'success');
            } else if (outcome === 'kept') {
                skipped++;
                keptDirs.push({ file_path: item.relativePath, reason: keptReason });
                if (journalEntry) journalEntry.status = 'skipped';
                setStatus(item.relativePath, 'skipped');
            }
        } else if (item.action === 'delete-remote' || item.action === 'delete-local') {
            // Versioned backup: for a file the move into the backup folder IS
            // the delete. It fails the file on its own, without the delete
            // running, so the destination copy stays where it was.
            if (config.versionedBackup) {
                try {
                    await archiveBeforeMutation(item.action === 'delete-remote' ? 'remote' : 'local', item.relativePath);
                } catch (e) {
                    if (checkpointError) { i--; continue; }
                    failBackup(item, journalEntry, e);
                    completed++;
                    callbacks.onProgress?.(completed, files.length);
                    continue;
                }
                deleted++;
                didTransfer = true;
                if (journalEntry) journalEntry.status = 'completed';
                setStatus(item.relativePath, 'success');
            } else try {
                if (item.action === 'delete-remote') {
                    if (config.isLocalLocal) {
                        // GAP-10: the "remote" side is a local directory,
                        // so the file goes through `delete_local_file`.
                        await invoke('delete_local_file', { path: remoteFilePath });
                    } else if (config.isProvider) {
                        await invoke('provider_delete_file', { path: remoteFilePath });
                    } else {
                        await invoke('delete_remote_file', { path: remoteFilePath, isDir: false });
                    }
                    // Best-effort: remove the `.aerocorrect` EC parity sidecar alongside the
                    // deleted file so a removed backup never orphans its sidecar. The
                    // sidecar is excluded from comparison, so it is never its own action.
                    await deleteRemoteSidecar(remoteFilePath);
                } else {
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
                setStatus(item.relativePath, 'error', errInfo.message);
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
    // completed is true only when the run was not cancelled AND no file
    // failed (transfer or verification). App.tsx offers Resume only when
    // `journal && !journal.completed`, so a partially failed run must keep
    // the journal; a fully successful run still deletes it.
    if (!config.isLocalLocal && !targetError) {
        try { assertTarget(); } catch { /* record below */ }
    }
    if (targetError) {
        runCancelled = true;
        errors.push({ kind: 'unknown', message: String(targetError), retryable: false, file_path: '' });
    }
    if (checkpointError) {
        runCancelled = true;
        errors.push({ kind: 'disk_error', message: String(checkpointError), retryable: false, file_path: '' });
    }
    journal.completed = !runCancelled && errors.length === 0;
    await saveJournal();
    if (journalEnabled && journal.completed) {
        await invoke('delete_sync_journal_cmd', {
            localPath: localRoot,
            remotePath: remoteRoot,
        }).catch(() => undefined);
    }

    // ── GAP-9a: post-sync verification sweep (Maniac mode) ─────────────────
    // Maniac transfers run with verify off for speed; this mandatory pass
    // re-checks every completed download with a size+mtime probe so silent
    // corruption is still surfaced. It never mutates the run outcome — the
    // counts are purely informational.
    let postSyncVerification:
        | { ok: number; mismatches: number; failed: number }
        | undefined;
    if (config.postSyncVerification && !runCancelled && !config.authenticatedRemoteContent) {
        let pv_ok = 0;
        let pv_mismatches = 0;
        let pv_failed = 0;
        for (const f of files) {
            if (f.action !== 'download') continue;
            const idx = journalEntryMap.get(f.relativePath);
            const entry = idx !== undefined ? journal.entries[idx] : undefined;
            if (entry?.status !== 'completed') continue;
            const localFilePath = `${localBase}/${f.relativePath}`;
            try {
                const vr = await invoke<VerifyResult>('verify_local_transfer', {
                    localPath: localFilePath,
                    expectedSize: f.size,
                    expectedMtime: f.mtime,
                    policy: 'size_and_mtime',
                });
                if (vr.passed) pv_ok++;
                else pv_mismatches++;
            } catch {
                pv_failed++;
            }
        }
        postSyncVerification = { ok: pv_ok, mismatches: pv_mismatches, failed: pv_failed };
    }

    // ── GAP-6: merge the run into the persisted sync index ─────────────────
    // The index records every entry that landed (size / mtime / is_dir) and
    // drops every entry a successful delete removed. It is a pure
    // optimisation + bisync-state aid: a failure here never fails the run.
    if (deps.writeIndex && !targetError && !transferTimedOut) {
        try {
            const existing = await invoke<SyncIndex | null>('load_sync_index_cmd', {
                localPath: localRoot,
                remotePath: remoteRoot,
                profileId: config.profileId ?? null,
            });
            assertTarget();
            const mergedFiles: Record<string, SyncIndexEntry> = {
                ...(existing?.files ?? {}),
            };
            const spellings: Record<string, string> = {
                ...(existing?.remote_spellings ?? {}),
            };
            // Resume omits completed deletes from its transfer list. Apply
            // their durable tombstones before retiring their intent tokens,
            // otherwise a recreated file could inherit stale deletion rights.
            for (const entry of journal.entries) {
                if (entry.status !== 'completed' || entry.action !== 'delete') continue;
                const key = entry.baseline_key ?? entry.relative_path;
                delete mergedFiles[key];
                delete spellings[key];
            }
            // Retire prior identities before inserting landed files: a
            // download may reuse the local name of a deleted mapped object.
            for (const f of files) {
                const idx = journalEntryMap.get(f.relativePath);
                const entry = idx !== undefined ? journal.entries[idx] : undefined;
                if (entry?.status !== 'completed') continue;
                const baselineKey = f.baselineKey ?? entry.baseline_key;
                if (f.action === 'delete-local' || f.action === 'delete-remote') {
                    const key = baselineKey ?? f.relativePath;
                    delete mergedFiles[key];
                    delete spellings[key];
                } else if (baselineKey != null) {
                    delete mergedFiles[baselineKey];
                    delete spellings[baselineKey];
                }
            }
            for (const f of files) {
                const idx = journalEntryMap.get(f.relativePath);
                const entry = idx !== undefined ? journal.entries[idx] : undefined;
                if (entry?.status !== 'completed') continue;
                if (f.action === 'upload' || f.action === 'download') {
                    // The next compare reads the local side against this time
                    // with the local clock. A download keeps the remote time,
                    // which it stamped on the local copy, and an upload the
                    // time the scan read. With neither (a backend that lists no
                    // time, such as FTP LIST dates, or a run resumed from its
                    // journal) the file's own is recorded, read when the
                    // download completed or before the upload, or the local side
                    // would be compared by size alone and a same-size edit would
                    // go unseen. A transfer the resumed journal had finished was
                    // not read in this run: its journal entry says what the run
                    // that made it recorded. An entry written before these
                    // fields existed gives no time.
                    let size = f.size;
                    let modified = f.mtime;
                    if (f.mtime == null) {
                        const landed = landedStates.get(f.relativePath);
                        if (landed) {
                            modified = landed.modified;
                            size = landed.size ?? f.size;
                        } else {
                            modified = entry.local_modified ?? null;
                            size = entry.local_size ?? f.size;
                        }
                    }
                    // A canonical upload carries the local spelling in sourcePath
                    // and the remote spelling in relativePath. Those two are
                    // the same name, so the index keeps the local spelling. A
                    // keep-both upload also sets sourcePath, but relativePath
                    // is a new suffixed file: indexing the source would mark
                    // the still-divergent original as synced.
                    const localRel = f.action === 'upload'
                        && f.sourcePath != null
                        && f.sourcePath.normalize('NFC') === f.relativePath.normalize('NFC')
                        ? f.sourcePath
                        : f.relativePath;
                    const sameName = localRel.normalize('NFC');
                    mergedFiles[localRel] = {
                        size,
                        modified,
                        is_dir: false,
                        ...(uploadsEnded.has(f.relativePath) ? { uploaded_at: uploadsEnded.get(f.relativePath) } : {}),
                    };
                    // Upload: relativePath is the remote spelling. Download:
                    // sourcePath is. Keep-both fails the NFC test above, so
                    // localRel is the suffixed name and must not record the
                    // original as its remote path.
                    const mirrorRemote = f.action === 'upload'
                        ? f.relativePath
                        : (f.sourcePath ?? f.relativePath);
                    if (sameName === mirrorRemote.normalize('NFC') && localRel !== mirrorRemote) {
                        spellings[localRel] = mirrorRemote;
                    } else {
                        delete spellings[localRel];
                    }
                }
            }
            for (const dir of [...dirs.remote, ...dirs.local]) {
                if (localNameProblems.has(dir)) continue;
                mergedFiles[dir] = { size: 0, modified: null, is_dir: true };
            }
            for (const key of Object.keys(spellings)) {
                if (!(key in mergedFiles)) delete spellings[key];
            }
            const index: SyncIndex = {
                // G111: keep in step with SYNC_INDEX_VERSION in src-tauri/src/sync.rs.
                // Stamping 1 here would send every index this writer produces
                // through the Rust-side migration on each read.
                version: 2,
                last_sync: new Date().toISOString(),
                local_path: localRoot,
                remote_path: remoteRoot,
                ...(config.profileId ? { profile_id: config.profileId } : {}),
                files: mergedFiles,
                ...(Object.keys(spellings).length > 0 ? { remote_spellings: spellings } : {}),
            };
            await invoke('save_sync_index_cmd', { index, intentTokens: journal.entries.filter(entry => entry.status === 'completed' && entry.intent_token).map(entry => entry.intent_token!) });
        } catch {
            // Index is an optimisation; never let it break the run.
        }
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
        keptDirs,
        postSyncVerification,
        delta_savings,
        delta_bytes_on_wire: delta_savings?.total_bytes_sent,
        delta_batch_files: delta_savings?.files_using_delta,
        ec_generated: ecGenerated,
        ec_verified: ecVerified,
        ec_repaired: ecRepaired,
        ec_skipped_too_large: ecSkippedTooLarge,
        ec_generate_failed: ecGenerateFailed,
    };
};
