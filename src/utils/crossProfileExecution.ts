// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

// Shared cross-profile transfer driver. Wraps the Tauri commands
// `cross_profile_plan`, `cross_profile_execute` and `cross_profile_cancel`
// so the unified transfer planner (Z.3.5) and the legacy CrossProfilePanel
// modal share the same execution path and the same progress events.
//
// The helper is intentionally framework-free: it returns plain summaries and
// exposes per-entry callbacks so each caller can render its own progress UI.
// Global progress widgets (TransferToast) already listen to the
// `transfer_event` stream emitted by the backend, so they keep working
// untouched.

import { invoke } from '@tauri-apps/api/core';

export interface CrossProfileEntryRequest {
    /** Absolute remote path on the source profile (file or directory). */
    sourcePath: string;
    /** Absolute remote path on the destination profile. */
    destPath: string;
    /** Treat `sourcePath` as a directory and walk it recursively. */
    recursive: boolean;
    /** Skip entries that already exist on the destination. */
    skipExisting: boolean;
}

export interface CrossProfilePlanEntry {
    source_path: string;
    dest_path: string;
    display_name: string;
    size: number;
    is_dir: boolean;
}

export interface CrossProfilePlanResult {
    planId: string;
    sourceProfile: string;
    destProfile: string;
    totalFiles: number;
    totalBytes: number;
    entries: CrossProfilePlanEntry[];
}

export interface CrossProfileEntrySummary {
    transferId: string;
    plannedFiles: number;
    transferredFiles: number;
    skippedFiles: number;
    failedFiles: number;
    totalBytes: number;
    durationMs: number;
}

interface CrossProfilePlanRawResponse {
    plan_id: string;
    source_profile_id: string;
    dest_profile_id: string;
    source_profile: string;
    dest_profile: string;
    entries: CrossProfilePlanEntry[];
    total_files: number;
    total_bytes: number;
}

interface CrossProfileExecuteRawResponse {
    transfer_id: string;
    planned_files: number;
    transferred_files: number;
    skipped_files: number;
    failed_files: number;
    total_bytes: number;
    duration_ms: number;
}

export const planCrossProfileEntry = async (
    sourceProfileId: string,
    destProfileId: string,
    entry: CrossProfileEntryRequest,
): Promise<CrossProfilePlanResult> => {
    const response = await invoke<CrossProfilePlanRawResponse>('cross_profile_plan', {
        request: {
            source_profile_id: sourceProfileId,
            dest_profile_id: destProfileId,
            source_path: entry.sourcePath,
            dest_path: entry.destPath,
            recursive: entry.recursive,
            skip_existing: entry.skipExisting,
        },
    });
    return {
        planId: response.plan_id,
        sourceProfile: response.source_profile,
        destProfile: response.dest_profile,
        totalFiles: response.total_files,
        totalBytes: response.total_bytes,
        entries: response.entries,
    };
};

export const executeCrossProfilePlan = async (
    planId: string,
): Promise<CrossProfileEntrySummary> => {
    const response = await invoke<CrossProfileExecuteRawResponse>('cross_profile_execute', {
        request: { plan_id: planId },
    });
    return {
        transferId: response.transfer_id,
        plannedFiles: response.planned_files,
        transferredFiles: response.transferred_files,
        skippedFiles: response.skipped_files,
        failedFiles: response.failed_files,
        totalBytes: response.total_bytes,
        durationMs: response.duration_ms,
    };
};

export const cancelCrossProfileTransfer = async (planId: string): Promise<void> => {
    try {
        await invoke('cross_profile_cancel', { transferId: planId });
    } catch {
        // Cancellation is best-effort: backend events are authoritative.
    }
};

export interface CrossProfileAggregateSummary {
    succeeded: number;
    failed: number;
    cancelled: number;
    transferredFiles: number;
    transferredBytes: number;
    durationMs: number;
    failures: Array<{ entry: CrossProfileEntryRequest; error: string }>;
}

export interface RunCrossProfileTransferOptions {
    /** Fired before each entry's plan() call. */
    onEntryStart?: (entry: CrossProfileEntryRequest, index: number) => void;
    /** Fired after each entry completes (plan + execute). */
    onEntryDone?: (
        entry: CrossProfileEntryRequest,
        summary: CrossProfileEntrySummary,
        index: number,
    ) => void;
    /** Fired when an entry fails; the loop continues with the next entry. */
    onEntryError?: (entry: CrossProfileEntryRequest, error: string, index: number) => void;
    /** Polled between entries; returning true aborts the remaining entries. */
    shouldCancel?: () => boolean;
}

/**
 * Plan-and-execute every entry sequentially against a single
 * source/destination profile pair. The backend uses one plan per entry so we
 * can mix file and directory selections without re-walking unrelated siblings.
 */
export const runCrossProfileTransfer = async (
    sourceProfileId: string,
    destProfileId: string,
    entries: CrossProfileEntryRequest[],
    options: RunCrossProfileTransferOptions = {},
): Promise<CrossProfileAggregateSummary> => {
    const start = Date.now();
    let succeeded = 0;
    let failed = 0;
    let cancelled = 0;
    let transferredFiles = 0;
    let transferredBytes = 0;
    const failures: Array<{ entry: CrossProfileEntryRequest; error: string }> = [];

    for (let i = 0; i < entries.length; i += 1) {
        if (options.shouldCancel?.()) {
            cancelled = entries.length - i;
            break;
        }
        const entry = entries[i];
        options.onEntryStart?.(entry, i);
        try {
            const plan = await planCrossProfileEntry(sourceProfileId, destProfileId, entry);
            const summary = await executeCrossProfilePlan(plan.planId);
            succeeded += 1;
            transferredFiles += summary.transferredFiles;
            transferredBytes += summary.totalBytes;
            options.onEntryDone?.(entry, summary, i);
        } catch (err) {
            failed += 1;
            const message = err instanceof Error ? err.message : String(err);
            failures.push({ entry, error: message });
            options.onEntryError?.(entry, message, i);
        }
    }

    return {
        succeeded,
        failed,
        cancelled,
        transferredFiles,
        transferredBytes,
        durationMs: Date.now() - start,
        failures,
    };
};

// ── Path helpers ───────────────────────────────────────────────────────────

/**
 * Join a remote directory path with an entry name without doubling slashes.
 * Mirrors what `cross_profile_plan` would do server-side when recursing into
 * a directory. Keeps a leading slash if the directory already had one and
 * tolerates Windows-flavoured separators surfaced by some providers.
 */
export const joinRemotePath = (dir: string, name: string): string => {
    const sanitizedDir = (dir ?? '').replace(/\\/g, '/').replace(/\/+$/, '');
    const sanitizedName = (name ?? '').replace(/\\/g, '/').replace(/^\/+/, '');
    if (sanitizedDir === '') return `/${sanitizedName}`;
    if (sanitizedDir === '/') return `/${sanitizedName}`;
    return `${sanitizedDir}/${sanitizedName}`;
};

export interface BuildEntryListInput {
    sourceDir: string;
    destDir: string;
    selections: Array<{ name: string; isDir?: boolean; is_dir?: boolean }>;
    skipExisting: boolean;
}

/**
 * Build the per-entry request list a unified planner needs to call
 * `runCrossProfileTransfer`. Directories become recursive transfers, files
 * stay scoped to themselves. Empty selections return [].
 */
export const buildCrossProfileEntries = ({
    sourceDir,
    destDir,
    selections,
    skipExisting,
}: BuildEntryListInput): CrossProfileEntryRequest[] => {
    if (!Array.isArray(selections) || selections.length === 0) return [];
    return selections
        .filter((selection) => selection && typeof selection.name === 'string' && selection.name !== '')
        .map((selection) => {
            const isDir = selection.isDir ?? selection.is_dir ?? false;
            return {
                sourcePath: joinRemotePath(sourceDir, selection.name),
                destPath: joinRemotePath(destDir, selection.name),
                recursive: isDir,
                skipExisting,
            };
        });
};
