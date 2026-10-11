// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Unified Transfer Progress Bar
 *
 * Reusable progress bar for all file transfer operations: sync, upload/download,
 * auto-update, cloud sync, model pull, etc.
 *
 * Features:
 * - 4 levels: base (bar only) → details (filename/speed/ETA) → batch (X/Y files) → graph
 * - Theme-aware animated shimmer (light/dark/tokyo/cyber)
 * - Optional speed graph over the whole transfer (canvas-based, `SpeedGraph`)
 * - The speed and ETA shown fall while no progress frame arrives (`useLiveSpeed`)
 * - Slide-down/up animation for mount/unmount
 */

import React, { useState, useRef, useEffect } from 'react';
import { ChevronDown, ChevronUp, Activity } from 'lucide-react';
import { formatBytes, formatSpeed, formatETA } from '../utils/formatters';
import { liveEta, useLiveSpeed } from '../utils/liveSpeed';
import { hasSpeedData, type SpeedProfile } from '../utils/speedProfile';
import { useTranslation } from '../i18n';
import { SpeedGraph } from './SpeedGraph';
import './TransferProgressBar.css';

/** Remembers whether the user left the speed graph open, across transfers. */
const GRAPH_OPEN_KEY = 'aeroftp.speedGraphOpen';

function readGraphOpen(): boolean {
    try {
        return localStorage.getItem(GRAPH_OPEN_KEY) === '1';
    } catch {
        return false;
    }
}

function writeGraphOpen(open: boolean): void {
    try {
        localStorage.setItem(GRAPH_OPEN_KEY, open ? '1' : '0');
    } catch {
        // Storage unavailable: the choice lasts for this card only.
    }
}

type EffectiveTheme = 'light' | 'dark' | 'truedark' | 'tokyo' | 'cyber' | 'green' | 'ice' | 'redhorse';

function resolveThemeFromDom(): EffectiveTheme {
    if (typeof document === 'undefined') return 'dark';
    const root = document.documentElement;
    // Specific theme classes first: truedark/green/redhorse also carry `.dark`,
    // and `ice` is a light theme with no `.dark`, so the generic checks come last.
    if (root.classList.contains('truedark')) return 'truedark';
    if (root.classList.contains('tokyo')) return 'tokyo';
    if (root.classList.contains('cyber')) return 'cyber';
    if (root.classList.contains('green')) return 'green';
    if (root.classList.contains('ice')) return 'ice';
    if (root.classList.contains('redhorse')) return 'redhorse';
    if (root.classList.contains('dark')) return 'dark';
    return 'light';
}

export interface TransferProgressBarProps {
    /** Progress percentage 0-100 */
    percentage: number;

    /** Current filename being transferred */
    filename?: string;
    /** Transfer speed in bytes/sec */
    speedBps?: number;
    /** Estimated time remaining in seconds */
    etaSeconds?: number;
    /** Bytes transferred so far */
    transferredBytes?: number;
    /** Total bytes to transfer */
    totalBytes?: number;

    /** Current file number in batch */
    currentFile?: number;
    /** Total files in batch */
    totalFiles?: number;

    /** Bar height: sm=4px, md=6px, lg=8px */
    size?: 'sm' | 'md' | 'lg';
    /** Visual variant */
    variant?: 'default' | 'gradient' | 'indeterminate';
    /** Enable shimmer animation (default: true) */
    animated?: boolean;
    /** Show slide animation on mount/unmount */
    slideAnimation?: boolean;

    /** Enable expandable speed graph (needs `speedProfile`) */
    showGraph?: boolean;
    /** Speed over the whole transfer for the graph (`utils/speedProfile.ts`) */
    speedProfile?: SpeedProfile;

    /** Additional CSS class */
    className?: string;

    /** Optional resolved app theme (recommended from parent) */
    effectiveTheme?: EffectiveTheme;
    /** Optional fill tone override */
    tone?: 'default' | 'success' | 'error';
}

/** Theme-specific gradient colors for the progress fill */
function getThemeGradient(theme: string, tone: 'default' | 'success' | 'error'): { from: string; to: string; shimmer: string } {
    if (tone === 'success') {
        return { from: '#10b981', to: '#22c55e', shimmer: 'rgba(34,197,94,0.28)' };
    }
    if (tone === 'error') {
        return { from: '#f97316', to: '#ef4444', shimmer: 'rgba(239,68,68,0.28)' };
    }
    switch (theme) {
        case 'tokyo':
            return { from: '#a855f7', to: '#ec4899', shimmer: 'rgba(168,85,247,0.3)' }; // purple→pink
        case 'cyber':
            return { from: '#22d3ee', to: '#10b981', shimmer: 'rgba(34,211,238,0.3)' }; // cyan→green
        case 'truedark':
            return { from: '#58a6ff', to: '#1f6feb', shimmer: 'rgba(88,166,255,0.3)' }; // GitHub-dark blue
        case 'green':
            return { from: '#22c55e', to: '#4ade80', shimmer: 'rgba(34,197,94,0.3)' }; // forest green
        case 'ice':
            return { from: '#0ea5e9', to: '#38bdf8', shimmer: 'rgba(14,165,233,0.3)' }; // sky/frost
        case 'redhorse':
            return { from: '#E32636', to: '#FF2800', shimmer: 'rgba(227,38,54,0.3)' }; // racing red
        default:
            return { from: '#3b82f6', to: '#06b6d4', shimmer: 'rgba(59,130,246,0.3)' }; // blue→cyan
    }
}

/** Size to CSS height class */
const sizeMap = { sm: 'h-1', md: 'h-1.5', lg: 'h-2' };

export const TransferProgressBar: React.FC<TransferProgressBarProps> = ({
    percentage,
    filename,
    speedBps,
    etaSeconds,
    transferredBytes,
    totalBytes,
    currentFile,
    totalFiles,
    size = 'md',
    variant = 'gradient',
    animated = true,
    slideAnimation = false,
    showGraph = false,
    speedProfile,
    className = '',
    effectiveTheme,
    tone = 'default',
}) => {
    const t = useTranslation();
    const resolvedTheme = effectiveTheme ?? resolveThemeFromDom();
    const [graphExpanded, setGraphExpanded] = useState(readGraphOpen);
    const containerRef = useRef<HTMLDivElement>(null);
    const [mounted, setMounted] = useState(!slideAnimation);

    // Slide animation on mount: cancel on unmount to avoid setState-after-unmount
    useEffect(() => {
        if (!slideAnimation) return;
        const handle = requestAnimationFrame(() => setMounted(true));
        return () => cancelAnimationFrame(handle);
    }, [slideAnimation]);

    const colors = getThemeGradient(resolvedTheme, tone);
    const clampedPct = Math.max(0, Math.min(100, Number.isFinite(percentage) ? percentage : 0));
    // Width stays fractional for a smooth fill; the printed label is rounded to
    // a whole percent so a file-count ratio never shows as "73.3333%".
    const pctLabel = Math.round(clampedPct);
    const hasDetails = filename || speedBps !== undefined || etaSeconds !== undefined;
    const hasBatch = currentFile !== undefined && totalFiles !== undefined;
    const hasBytes = transferredBytes !== undefined && totalBytes !== undefined && totalBytes > 0;
    // Size unknown (a stream): the bytes moved so far, without a "/ 0 B".
    const hasBytesOnly = !hasBytes && transferredBytes !== undefined && transferredBytes > 0;
    // The speed and ETA shown fall while no frame arrives (a stalled server
    // sends none); a finished or failed transfer has nothing left to fall.
    const live = useLiveSpeed(
        speedBps,
        `${transferredBytes ?? ''}|${percentage}|${currentFile ?? ''}|${speedBps ?? ''}`,
        clampedPct < 100 && tone === 'default',
    );
    const shownEta = etaSeconds === undefined ? undefined : liveEta(etaSeconds, live.factor);
    const graphReady = showGraph && !!speedProfile && hasSpeedData(speedProfile);
    const toggleGraph = () => {
        const open = !graphExpanded;
        setGraphExpanded(open);
        writeGraphOpen(open);
    };

    // Determine theme class for CSS animations (use resolved theme, not raw 'auto')
    const themeClass = resolvedTheme === 'tokyo' ? 'tpb-tokyo'
        : resolvedTheme === 'cyber' ? 'tpb-cyber'
        : resolvedTheme === 'truedark' ? 'tpb-truedark'
        : resolvedTheme === 'green' ? 'tpb-green'
        : resolvedTheme === 'ice' ? 'tpb-ice'
        : resolvedTheme === 'redhorse' ? 'tpb-redhorse'
        : resolvedTheme === 'light' ? 'tpb-light'
        : 'tpb-dark';

    return (
        <div
            ref={containerRef}
            className={`tpb-container ${themeClass} ${slideAnimation ? (mounted ? 'tpb-slide-enter' : 'tpb-slide-initial') : ''} ${className}`}
        >
            {/* Details row: filename + speed/ETA */}
            {hasDetails && (
                <div className="flex items-center justify-between text-xs mb-1">
                    <span className="tpb-filename truncate max-w-[60%]">
                        {filename || ''}
                    </span>
                    <span className="tpb-stats">
                        {hasBatch && (
                            <span className="tpb-batch">{currentFile}/{totalFiles}</span>
                        )}
                        {hasBytes && (
                            <span>{formatBytes(transferredBytes)} / {formatBytes(totalBytes)}</span>
                        )}
                        {hasBytesOnly && (
                            <span>{formatBytes(transferredBytes)}</span>
                        )}
                        {speedBps !== undefined && speedBps > 0 && (
                            <span>
                                {(hasBytes || hasBytesOnly || hasBatch) && ' · '}
                                {formatSpeed(live.bps)}
                            </span>
                        )}
                        {shownEta !== undefined && shownEta > 0 && (
                            <span> · {formatETA(shownEta)}</span>
                        )}
                        {!hasBytes && !hasBytesOnly && !hasBatch && speedBps === undefined && (
                            <span>{pctLabel}%</span>
                        )}
                    </span>
                </div>
            )}

            {/* Progress bar track */}
            <div className={`tpb-track ${sizeMap[size]} rounded-full overflow-hidden`}>
                {variant === 'indeterminate' ? (
                    <div className="tpb-fill-indeterminate h-full w-1/3 rounded-full" />
                ) : (
                    <div
                        className={`tpb-fill h-full rounded-full transition-all duration-300 ${animated ? 'tpb-shimmer' : ''}`}
                        style={{
                            width: `${Math.max(clampedPct, clampedPct > 0 ? 2 : 0)}%`,
                            background: variant === 'gradient'
                                ? `linear-gradient(90deg, ${colors.from}, ${colors.to})`
                                : undefined,
                        }}
                    />
                )}
            </div>

            {/* Batch counter (below bar, if no details row) */}
            {hasBatch && !hasDetails && (
                <div className="flex justify-between text-[10px] mt-0.5">
                    <span className="tpb-batch-label">{currentFile}/{totalFiles}</span>
                    <span className="tpb-pct-label">{pctLabel}%</span>
                </div>
            )}

            {/* Graph toggle + graph area */}
            {graphReady && speedProfile && (
                <>
                    <button
                        type="button"
                        onClick={toggleGraph}
                        className="tpb-graph-toggle"
                        aria-expanded={graphExpanded}
                        style={{
                            color: colors.from,
                            borderColor: colors.shimmer,
                            ['--tpb-toggle-bg' as string]: colors.shimmer,
                        }}
                    >
                        <Activity size={12} />
                        <span>{graphExpanded ? t('transfer.hideSpeedGraph') : t('transfer.speedGraph')}</span>
                        {graphExpanded ? <ChevronUp size={12} /> : <ChevronDown size={12} />}
                    </button>
                    <div className={`tpb-graph-wrapper ${graphExpanded ? 'tpb-graph-open' : 'tpb-graph-closed'}`}>
                        <SpeedGraph
                            profile={speedProfile}
                            currentBps={live.bps}
                            theme={resolvedTheme}
                            active={graphExpanded}
                        />
                    </div>
                </>
            )}
        </div>
    );
};

export default TransferProgressBar;
