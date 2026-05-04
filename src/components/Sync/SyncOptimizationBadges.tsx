// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * SyncOptimizationBadges: Read-only badges showing per-provider capabilities
 * Reused in both Quick Sync and Advanced tabs
 */

import React from 'react';
import {
    Zap, ArrowUpDown, HardDrive, Layers, FileCheck, Shrink
} from 'lucide-react';
import { TransferOptimizationHints } from '../../types';
import { useTranslation } from '../../i18n';

interface SyncOptimizationBadgesProps {
    hints: TransferOptimizationHints | null;
    loading?: boolean;
    compact?: boolean;
}

interface Badge {
    label: string;
    visible: boolean;
    Icon: typeof Zap;
    detail?: string;
    tone?: 'active' | 'eligible' | 'inactive';
}

export const SyncOptimizationBadges: React.FC<SyncOptimizationBadgesProps> = React.memo(({
    hints,
    loading = false,
    compact = false,
}) => {
    const t = useTranslation();

    if (loading || !hints) return null;

    // "available" used to be the fallback label for "neither active
    // nor eligible right now": which reads to users as "I can enable
    // this", the opposite of its intent. Rename the tone to
    // "inactive" so the badge text mirrors reality when the delta
    // path is off; the colour remains the neutral slate fallback.
    const deltaTone: Badge['tone'] = hints.delta_sync_active
        ? 'active'
        : hints.delta_sync_eligible
            ? 'eligible'
            : 'inactive';

    const badges: Badge[] = [
        {
            label: t('syncPanel.optimizationMultipart'),
            visible: hints.supports_multipart,
            Icon: Layers,
            detail: hints.supports_multipart
                ? `>${Math.round(hints.multipart_threshold / 1_048_576)}MB, ${hints.multipart_max_parallel}x`
                : undefined,
            tone: 'active',
        },
        {
            label: t('syncPanel.optimizationResume'),
            visible: hints.supports_resume_download || hints.supports_resume_upload,
            Icon: ArrowUpDown,
            tone: 'active',
        },
        {
            label: t('syncPanel.optimizationChecksum'),
            visible: hints.supports_server_checksum,
            Icon: FileCheck,
            detail: hints.preferred_checksum_algo || undefined,
            tone: 'active',
        },
        {
            label: t('syncPanel.optimizationCompression'),
            visible: hints.supports_compression,
            Icon: Shrink,
            tone: 'active',
        },
        {
            label: `${t('syncPanel.optimizationDelta')} · ${deltaTone}`,
            visible: hints.supports_delta_sync,
            Icon: HardDrive,
            detail: hints.delta_sync_note || undefined,
            tone: deltaTone,
        },
    ];

    const activeBadges = badges.filter(b => b.visible);
    if (activeBadges.length === 0) return null;

    const toneClass = (tone: Badge['tone']) => {
        switch (tone) {
            case 'eligible':
                return 'bg-amber-500/15 text-amber-300 border border-amber-500/20';
            case 'inactive':
                return 'bg-slate-500/15 text-slate-300 border border-slate-500/20';
            case 'active':
            default:
                return 'bg-emerald-500/15 text-emerald-400 border border-emerald-500/20';
        }
    };

    return (
        <div className={`flex flex-wrap gap-1.5 ${compact ? '' : 'mt-2'}`}>
            {activeBadges.map(badge => (
                <span
                    key={badge.label}
                    className={`inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-medium ${toneClass(badge.tone)}`}
                    title={badge.detail}
                >
                    <badge.Icon size={10} />
                    {badge.label}
                </span>
            ))}
        </div>
    );
});

SyncOptimizationBadges.displayName = 'SyncOptimizationBadges';
