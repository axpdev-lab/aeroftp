// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useEffect, useState } from 'react';
import { TrendingDown, TrendingUp } from 'lucide-react';
import { formatSize } from '../../utils/formatters';
import { computeCompressionRatio } from '../../utils/archiveSizeReport';
import { useTranslation } from '../../i18n';

/**
 * Post-create compression report for an AeroVault Zip, mirroring the standard
 * CompressDialog completion stats: real before/after sizes with an "after" bar
 * that drains from full to the measured output/input ratio, and three honest
 * outcomes (saved / grew / incompressible). Styled with the vault's Tailwind
 * palette so it sits under the success banner without theming dependencies.
 */
export const ZipCompressionReport: React.FC<{ inputBytes: number; outputBytes: number }> = ({ inputBytes, outputBytes }) => {
    const t = useTranslation();
    const { savedBytes, savedPercent } = computeCompressionRatio(inputBytes, outputBytes);
    const pct = Math.round(savedPercent);
    const outcome: 'saved' | 'grew' | 'incompressible' = pct >= 1 ? 'saved' : pct <= -1 ? 'grew' : 'incompressible';
    const afterFraction = inputBytes > 0 ? Math.max(0, Math.min(1, outputBytes / inputBytes)) : 1;

    const [settled, setSettled] = useState(false);
    useEffect(() => {
        const h = requestAnimationFrame(() => setSettled(true));
        return () => cancelAnimationFrame(h);
    }, []);
    const afterWidth = settled ? afterFraction * 100 : 100;
    const afterFill = outcome === 'saved' ? 'bg-emerald-500' : outcome === 'grew' ? 'bg-orange-500' : 'bg-gray-400 dark:bg-gray-500';
    const badge = outcome === 'saved' ? 'text-emerald-500' : outcome === 'grew' ? 'text-orange-500' : 'text-gray-500 dark:text-gray-400';

    return (
        <div className="px-4 py-3 border-b border-gray-200 dark:border-gray-700">
            <div className="flex items-center justify-between mb-2">
                <span className="text-xs font-medium text-gray-600 dark:text-gray-300">{t('compress.complete')}</span>
                <span className={`text-xs font-bold flex items-center gap-1 ${badge}`}>
                    {outcome === 'saved' && <><TrendingDown size={13} />-{pct}%</>}
                    {outcome === 'grew' && <><TrendingUp size={13} />+{Math.abs(pct)}%</>}
                    {outcome === 'incompressible' && <>≈0%</>}
                </span>
            </div>
            <div className="flex items-center gap-2 mb-1">
                <span className="text-[10px] w-12 shrink-0 text-gray-500 dark:text-gray-400">{t('compress.before')}</span>
                <div className="flex-1 h-2.5 rounded-full overflow-hidden bg-gray-200 dark:bg-gray-700">
                    <div className="h-full rounded-full bg-gray-400 dark:bg-gray-500" style={{ width: '100%' }} />
                </div>
                <span className="text-[10px] w-16 text-right shrink-0 text-gray-600 dark:text-gray-300">{formatSize(inputBytes)}</span>
            </div>
            <div className="flex items-center gap-2">
                <span className="text-[10px] w-12 shrink-0 text-gray-500 dark:text-gray-400">{t('compress.after')}</span>
                <div className="flex-1 h-2.5 rounded-full overflow-hidden bg-gray-200 dark:bg-gray-700">
                    <div className={`h-full rounded-full transition-all duration-700 ease-out ${afterFill}`} style={{ width: `${afterWidth}%` }} />
                </div>
                <span className="text-[10px] w-16 text-right shrink-0 text-gray-600 dark:text-gray-300">{formatSize(outputBytes)}</span>
            </div>
            <div className="mt-2 text-center text-[11px] text-gray-500 dark:text-gray-400">
                {outcome === 'saved' && `${t('compress.saved')} ${formatSize(savedBytes)}`}
                {outcome === 'grew' && `${t('compress.increased')} ${formatSize(Math.abs(savedBytes))}`}
                {outcome === 'incompressible' && t('compress.incompressible')}
            </div>
        </div>
    );
};
