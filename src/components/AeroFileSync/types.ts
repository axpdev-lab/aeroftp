// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import type { CompareResult, CompareResultEntry } from '../../utils/compareEndpoints';
import type { PresetPlan } from '../../utils/syncPresets';

export type AeroFileSyncTab = 'compare' | 'plan' | 'sync';

export type AeroFileSyncPairKind = 'local-local' | 'local-remote' | 'remote-local';

export interface AeroFileSyncContext {
    pairKind: AeroFileSyncPairKind | null;
    leftLabel: string;
    rightLabel: string;
    leftPanelId?: 'local' | 'local2';
    rightPanelId?: 'local' | 'local2';
    compareResult: CompareResult | null;
    initialSource?: string;
    initialDestination?: string;
}

export interface AeroFileSyncDialogProps {
    isOpen: boolean;
    onClose: () => void;
    initialTab?: AeroFileSyncTab;
    context: AeroFileSyncContext;
    onApplyMirrorLeftToRight: (entries: CompareResultEntry[]) => void;
    onApplyMirrorRightToLeft: (entries: CompareResultEntry[]) => void;
    onExecutePreset: (plan: PresetPlan) => void;
}
