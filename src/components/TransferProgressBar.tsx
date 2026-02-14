/**
 * Unified Transfer Progress Bar
 *
 * Reusable progress bar for all file transfer operations: sync, upload/download,
 * auto-update, cloud sync, model pull, etc.
 *
 * Features:
 * - 4 levels: base (bar only) → details (filename/speed/ETA) → batch (X/Y files) → graph
 * - Theme-aware animated shimmer (light/dark/tokyo/cyber)
 * - Optional real-time speed graph (canvas-based)
 * - Slide-down/up animation for mount/unmount
 */

import React, { useState, useRef, useEffect } from 'react';
import { ChevronDown, ChevronUp, Activity } from 'lucide-react';
import { formatBytes, formatSpeed, formatETA } from '../utils/formatters';
import { SpeedGraph } from './SpeedGraph';
import './TransferProgressBar.css';

type EffectiveTheme = 'light' | 'dark' | 'tokyo' | 'cyber';

function resolveThemeFromDom(): EffectiveTheme {
    if (typeof document === 'undefined') return 'dark';
    const root = document.documentElement;
    if (root.classList.contains('tokyo')) return 'tokyo';
    if (root.classList.contains('cyber')) return 'cyber';
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

    /** Enable expandable speed graph */
    showGraph?: boolean;
    /** Speed history samples for graph (bytes/sec values) */
    speedHistory?: number[];

    /** Additional CSS class */
    className?: string;

    /** Optional resolved app theme (recommended from parent) */
    effectiveTheme?: EffectiveTheme;
}

/** Theme-specific gradient colors for the progress fill */
function getThemeGradient(theme: string): { from: string; to: string; shimmer: string } {
    switch (theme) {
        case 'tokyo':
            return { from: '#a855f7', to: '#ec4899', shimmer: 'rgba(168,85,247,0.3)' }; // purple→pink
        case 'cyber':
            return { from: '#22d3ee', to: '#10b981', shimmer: 'rgba(34,211,238,0.3)' }; // cyan→green
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
    speedHistory,
    className = '',
    effectiveTheme,
}) => {
    const resolvedTheme = effectiveTheme ?? resolveThemeFromDom();
    const [graphExpanded, setGraphExpanded] = useState(false);
    const containerRef = useRef<HTMLDivElement>(null);
    const [mounted, setMounted] = useState(!slideAnimation);

    // Slide animation on mount
    useEffect(() => {
        if (slideAnimation) {
            requestAnimationFrame(() => setMounted(true));
        }
    }, [slideAnimation]);

    const colors = getThemeGradient(resolvedTheme);
    const clampedPct = Math.max(0, Math.min(100, percentage));
    const hasDetails = filename || speedBps !== undefined || etaSeconds !== undefined;
    const hasBatch = currentFile !== undefined && totalFiles !== undefined;
    const hasBytes = transferredBytes !== undefined && totalBytes !== undefined;

    // Determine theme class for CSS animations (use resolved theme, not raw 'auto')
    const themeClass = resolvedTheme === 'tokyo' ? 'tpb-tokyo'
        : resolvedTheme === 'cyber' ? 'tpb-cyber'
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
                        {speedBps !== undefined && speedBps > 0 && (
                            <span>
                                {(hasBytes || hasBatch) && ' · '}
                                {formatSpeed(speedBps)}
                            </span>
                        )}
                        {etaSeconds !== undefined && etaSeconds > 0 && (
                            <span> · {formatETA(etaSeconds)}</span>
                        )}
                        {!hasBytes && !hasBatch && speedBps === undefined && (
                            <span>{clampedPct}%</span>
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
                    <span className="tpb-pct-label">{clampedPct}%</span>
                </div>
            )}

            {/* Graph toggle + graph area */}
            {showGraph && speedHistory && speedHistory.length > 0 && (
                <>
                    <button
                        type="button"
                        onClick={() => setGraphExpanded(prev => !prev)}
                        className="tpb-graph-toggle"
                    >
                        <Activity size={10} />
                        <span>{graphExpanded ? 'Hide graph' : 'Speed graph'}</span>
                        {graphExpanded ? <ChevronUp size={10} /> : <ChevronDown size={10} />}
                    </button>
                    <div className={`tpb-graph-wrapper ${graphExpanded ? 'tpb-graph-open' : 'tpb-graph-closed'}`}>
                        <SpeedGraph
                            speedHistory={speedHistory}
                            theme={resolvedTheme}
                        />
                    </div>
                </>
            )}
        </div>
    );
};

export default TransferProgressBar;
