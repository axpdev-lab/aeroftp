/**
 * SyncOptimizationBadges — Read-only badges showing per-provider capabilities
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
    supported: boolean;
    Icon: typeof Zap;
    detail?: string;
}

export const SyncOptimizationBadges: React.FC<SyncOptimizationBadgesProps> = React.memo(({
    hints,
    loading = false,
    compact = false,
}) => {
    const t = useTranslation();

    if (loading || !hints) return null;

    const badges: Badge[] = [
        {
            label: t('syncPanel.optimizationMultipart'),
            supported: hints.supports_multipart,
            Icon: Layers,
            detail: hints.supports_multipart
                ? `>${Math.round(hints.multipart_threshold / 1_048_576)}MB, ${hints.multipart_max_parallel}x`
                : undefined,
        },
        {
            label: t('syncPanel.optimizationResume'),
            supported: hints.supports_resume_download || hints.supports_resume_upload,
            Icon: ArrowUpDown,
        },
        {
            label: t('syncPanel.optimizationChecksum'),
            supported: hints.supports_server_checksum,
            Icon: FileCheck,
            detail: hints.preferred_checksum_algo || undefined,
        },
        {
            label: t('syncPanel.optimizationCompression'),
            supported: hints.supports_compression,
            Icon: Shrink,
        },
        {
            label: t('syncPanel.optimizationDelta'),
            supported: hints.supports_delta_sync,
            Icon: HardDrive,
        },
    ];

    const activeBadges = badges.filter(b => b.supported);
    if (activeBadges.length === 0) return null;

    return (
        <div className={`flex flex-wrap gap-1.5 ${compact ? '' : 'mt-2'}`}>
            {activeBadges.map(badge => (
                <span
                    key={badge.label}
                    className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-medium bg-emerald-500/15 text-emerald-400 border border-emerald-500/20"
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
