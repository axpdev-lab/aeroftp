// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { Loader2, Play, Square } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { formatBytes } from '../../utils/formatters';
import { useScanProgress, formatElapsed } from '../../hooks/useScanProgress';

interface CompareScanStateProps {
    /** A recursive compare is running. */
    loading: boolean;
    scanProgressId?: string;
    /** The last compare was stopped by the user. */
    stopped?: boolean;
    leftLabel: string;
    rightLabel: string;
    /** Start the compare of the pair. Absent: nothing can be compared yet. */
    onStart?: () => void;
    /** Stop the compare that is running. */
    onStop?: () => void;
}

/**
 * What the Compare and Plan tabs show before there is a result: a running
 * scan with its counters and a Stop button, or the pair and a Start button.
 * A recursive compare reads both folders and every folder under them, so it
 * never starts by itself when AeroSync opens: on a home folder that is
 * hundreds of thousands of files, and it used to run on with no way to stop
 * it, even after the dialog closed.
 */
export const CompareScanState: React.FC<CompareScanStateProps> = ({
    loading,
    scanProgressId,
    stopped,
    leftLabel,
    rightLabel,
    onStart,
    onStop,
}) => {
    const t = useTranslation();
    const { totals, elapsedMs } = useScanProgress(loading, scanProgressId);
    const buttonCls = 'mt-1 inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-medium text-white';

    if (loading) {
        return (
            <div className="flex flex-col items-center gap-2 p-8 text-center text-sm text-gray-500 dark:text-gray-400">
                <Loader2 size={24} className="animate-spin text-blue-500" />
                {t('syncPanel.scanning') || 'Scanning directories'}
                {/* The same numbers the summary will show when the scan
                    finishes, shown while they are still the only thing to
                    go on: a scan that is working and a scan that is stuck
                    used to look identical. */}
                <div className="flex flex-wrap items-center justify-center gap-x-4 gap-y-1 text-xs tabular-nums text-gray-400 dark:text-gray-500">
                    <span>⏱ {formatElapsed(elapsedMs)}</span>
                    <span>📄 {totals.files.toLocaleString()}</span>
                    <span>📁 {totals.dirs.toLocaleString()}</span>
                    <span>{formatBytes(totals.bytes)}</span>
                </div>
                {onStop && (
                    <button type="button" onClick={onStop} className={`${buttonCls} bg-red-500 hover:bg-red-600`}>
                        <Square size={12} />
                        {t('aerosync.stopCompare') || 'Stop'}
                    </button>
                )}
            </div>
        );
    }

    if (!onStart) return null;

    return (
        <div className="flex flex-col items-center gap-2 p-8 text-center text-sm text-gray-500 dark:text-gray-400">
            {stopped && (
                <span className="font-medium text-amber-600 dark:text-amber-400">
                    {t('aerosync.compareStopped') || 'Compare stopped.'}
                </span>
            )}
            <span className="break-all">
                {t('aerosync.comparePair', { left: leftLabel, right: rightLabel }) || `${leftLabel} ↔ ${rightLabel}`}
            </span>
            <span className="max-w-md text-xs text-gray-400 dark:text-gray-500">
                {t('aerosync.compareStartHint') || 'The compare reads both folders and every folder under them. On a large tree it can take a while, and you can stop it.'}
            </span>
            <button type="button" onClick={onStart} className={`${buttonCls} bg-blue-500 hover:bg-blue-600`}>
                <Play size={12} />
                {t('aerosync.startCompare') || 'Start compare'}
            </button>
        </div>
    );
};
