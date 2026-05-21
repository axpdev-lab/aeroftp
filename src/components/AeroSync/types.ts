// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import type { CompareResult, CompareResultEntry } from '../../utils/compareEndpoints';
import type { PresetPlan } from '../../utils/syncPresets';

export type AeroSyncTab = 'compare' | 'plan' | 'sync';

export type AeroSyncPairKind = 'local-local' | 'local-remote' | 'remote-local';

export interface AeroSyncContext {
    pairKind: AeroSyncPairKind | null;
    leftLabel: string;
    rightLabel: string;
    leftPanelId?: 'local' | 'local2';
    rightPanelId?: 'local' | 'local2';
    compareResult: CompareResult | null;
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
    onExecutePreset: (plan: PresetPlan) => void;
}
