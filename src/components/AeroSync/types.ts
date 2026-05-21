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
