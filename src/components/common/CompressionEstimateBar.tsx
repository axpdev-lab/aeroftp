// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Loader2, Archive } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { formatBytes } from '../../utils/formatters';

interface CompressionEstimateBarProps {
    /** Total uncompressed input size. */
    originalBytes: number;
    /** Estimated compressed size (the payload, before any recovery parity). */
    estimatedBytes: number;
    /** Optional recovery-parity overhead (e.g. .aerozip), shown as a 2nd segment. */
    parityBytes?: number;
    /** True when the whole input was measured (no extrapolation). */
    exact?: boolean;
    /** Estimate is being (re)computed. */
    loading?: boolean;
    className?: string;
}

/**
 * Graphical compression estimate: a horizontal track sized to the ORIGINAL
 * input, with an inner fill showing the estimated COMPRESSED size (and an
 * optional recovery-parity segment). The numbers come from a real canary
 * sample compressed with the actual codec, so the bar reflects measured
 * behaviour rather than a fixed per-format guess.
 */
export const CompressionEstimateBar: React.FC<CompressionEstimateBarProps> = ({
    originalBytes,
    estimatedBytes,
    parityBytes = 0,
    exact = false,
    loading = false,
    className,
}) => {
    const t = useTranslation();

    if (originalBytes <= 0) return null;

    const compressed = Math.max(0, Math.min(estimatedBytes, originalBytes));
    const parity = Math.max(0, parityBytes);
    const total = compressed + parity;
    // Widths are relative to the original; the parity can push the bar past 100%.
    const denom = Math.max(originalBytes, total);
    const compressedPct = Math.min(100, (compressed / denom) * 100);
    const parityPct = Math.min(100 - compressedPct, (parity / denom) * 100);
    const savedPct = Math.max(0, Math.round((1 - total / originalBytes) * 100));
    const prefix = exact ? '' : '≈ ';

    return (
        <div className={`space-y-1 ${className ?? ''}`}>
            <div className="flex items-center justify-between text-[11px]">
                <span className="flex items-center gap-1 font-medium text-emerald-600 dark:text-emerald-400">
                    <Archive size={11} />
                    {loading ? (
                        <span className="flex items-center gap-1 text-gray-500 dark:text-gray-400">
                            <Loader2 size={11} className="animate-spin" />
                            {t('compress.estimating')}
                        </span>
                    ) : (
                        <>
                            {prefix}
                            {formatBytes(total)}
                            {parity > 0 && (
                                <span className="text-amber-600 dark:text-amber-400">
                                    {' '}(+{formatBytes(parity)})
                                </span>
                            )}
                        </>
                    )}
                </span>
                <span className="text-gray-500 dark:text-gray-400">{formatBytes(originalBytes)}</span>
            </div>

            <div className="relative h-3 w-full overflow-hidden rounded-full bg-gray-200 dark:bg-gray-700">
                {!loading && (
                    <>
                        <div
                            className="absolute left-0 top-0 h-full rounded-l-full bg-emerald-500 transition-all duration-300"
                            style={{ width: `${compressedPct}%` }}
                        />
                        {parityPct > 0 && (
                            <div
                                className="absolute top-0 h-full bg-amber-400 transition-all duration-300"
                                style={{ left: `${compressedPct}%`, width: `${parityPct}%` }}
                            />
                        )}
                    </>
                )}
            </div>

            {!loading && (
                <div className="flex items-center justify-between text-[10px] text-gray-400 dark:text-gray-500">
                    <span>{t('compress.spaceSaved', { pct: savedPct })}</span>
                    <span>{exact ? t('compress.estimateExact') : t('compress.estimateApprox')}</span>
                </div>
            )}
        </div>
    );
};

export default CompressionEstimateBar;
