// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import type { CompareResult, CompareResultEntry } from '../../utils/compareEndpoints';
import type { PresetPlan } from '../../utils/syncPresets';
import type { RetryPolicy, SyncJournal } from '../../types';

export type AeroSyncTab = 'compare' | 'plan' | 'sync';

export type AeroSyncPairKind = 'local-local' | 'local-remote' | 'remote-local';

// CO-1: runtime hints attached to onExecute. Carry the Plan tab knobs
// (speed mode + verify policy) into App.tsx so the unified executor can
// forward them to the Rust planner. Kept separate from PresetPlan to
// avoid mutating the shared syncPresets surface.
export type AeroSyncSpeedMode = 'normal' | 'fast' | 'turbo' | 'extreme';
export type AeroSyncVerifyPolicy =
    | 'none'
    | 'size_only'
    | 'size_and_mtime'
    | 'full_checksum';

// GAP-7: keep-both / canary sampling strategy passed to `sync_canary_run`.
export type AeroSyncCanarySelection = 'random' | 'newest' | 'largest';

export interface AeroSyncRuntime {
    speedMode: AeroSyncSpeedMode;
    verifyPolicy: AeroSyncVerifyPolicy;
    /**
     * GAP-7: when set, Execute runs a sample-based canary trial instead of
     * the full sync; the result dialog's Approve button then runs it for real.
     */
    canary?: { percent: number; selection: AeroSyncCanarySelection };
    /**
     * GAP-8: Plan-tab parity controls threaded into the connected-remote
     * runner. `retryPolicy` is derived from the speed mode; `transferBudget`
     * caps the bytes moved (0 = unlimited); `versioningStrategy` enables
     * archive-before-mutation when the versioned-backup toggle is on.
     */
    retryPolicy?: RetryPolicy;
    transferBudget?: number;
    versioningStrategy?: string | null;
}

export interface AeroSyncContext {
    pairKind: AeroSyncPairKind | null;
    leftLabel: string;
    rightLabel: string;
    leftPanelId?: 'local' | 'local2';
    rightPanelId?: 'local' | 'local2';
    compareResult: CompareResult | null;
    /**
     * GAP-5: true while a recursive connected-remote compare
     * (`compare_directories`) is still running. The Compare and Plan tabs
     * render a scanning placeholder instead of the empty-state hint.
     */
    compareLoading?: boolean;
    /**
     * GAP-6: an interrupted connected-remote sync journal for this path
     * pair, if one exists. When set, the dialog surfaces a resume banner.
     */
    pendingJournal?: SyncJournal | null;
    initialSource?: string;
    initialDestination?: string;
    /**
     * Optional rich-sync context (SLICE 3+). When the dialog is opened
     * against a connected remote, these fields enable the header
     * launchers (Templates, Multi-Path, Rollback) and the Verify /
     * Speed-mode controls in the Plan tab. They are populated by
     * App.tsx based on the active remote profile; for local-only or
     * disconnected sessions they remain undefined and the launchers
     * stay hidden.
     */
    activeProfileId?: string;
    isProvider?: boolean;
    excludePatterns?: string[];
}

export interface AeroSyncDialogProps {
    isOpen: boolean;
    onClose: () => void;
    initialTab?: AeroSyncTab;
    context: AeroSyncContext;
    onApplyMirrorLeftToRight: (entries: CompareResultEntry[]) => void;
    onApplyMirrorRightToLeft: (entries: CompareResultEntry[]) => void;
    onExecutePreset: (plan: PresetPlan, runtime: AeroSyncRuntime) => void;
    /** GAP-6: resume an interrupted journal from the resume banner. */
    onResumeJournal: (journal: SyncJournal) => void;
    /** GAP-6: discard the interrupted journal and clear the banner. */
    onDismissJournal: () => void;
}
