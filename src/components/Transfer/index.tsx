// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Transfer progress components
 */

import React, { useState, useEffect } from 'react';
import { Download, Upload, Folder, X, Minus, Maximize2 } from 'lucide-react';
import { formatBytes, formatSpeed, formatETA } from '../../utils/formatters';
import { useTheme, getEffectiveTheme } from '../../hooks/useTheme';
import { TransferProgressBar } from '../TransferProgressBar';

/**
 * Truncate a path smartly: always show the last 2 segments with ellipsis prefix.
 * e.g. "/var/www/html/progetto_eric/src/css" → ".../src/css"
 */
function truncatePath(path: string, maxLen = 36): string {
    if (!path || path.length <= maxLen) return path;
    const parts = path.split('/').filter(Boolean);
    if (parts.length <= 2) return path;
    const tail = parts.slice(-2).join('/');
    if (tail.length + 4 >= maxLen) return `.../${parts[parts.length - 1]}`;
    return `.../${tail}`;
}

// Transfer progress data structure
export interface TransferProgress {
    transfer_id?: string;
    filename: string;
    total: number;
    transferred: number;
    percentage: number;
    speed_bps: number;
    eta_seconds: number;
    direction: 'download' | 'upload';
    total_files?: number; // When set, transferred/total are file counts (folder transfer)
    path?: string;        // Full path for context
}

export interface TransferToastLane {
    id: string;
    filename: string;
    total: number;
    transferred: number;
    percentage: number;
    speed_bps: number;
    eta_seconds: number;
    direction: 'download' | 'upload';
    path?: string;
    state?: 'active' | 'completed' | 'error';
}

export interface TransferToastState {
    summary: TransferProgress;
    lanes?: TransferToastLane[];
    reservedLaneSlots?: number;
    maxChannels?: number;
    /** Rolling aggregate speed samples (bytes/sec) for the collapsible graph. */
    speedHistory?: number[];
}

// ============ Animated Bytes (Matrix-style for uploads) ============
interface AnimatedBytesProps {
    bytes: number;
    isAnimated: boolean;
}

export const AnimatedBytes: React.FC<AnimatedBytesProps> = ({ bytes, isAnimated }) => {
    const [displayText, setDisplayText] = useState(formatBytes(bytes));

    useEffect(() => {
        if (!isAnimated) {
            setDisplayText(formatBytes(bytes));
            return;
        }

        const chars = '0123456789ABCDEF';
        let frame = 0;
        const targetText = formatBytes(bytes);

        const interval = setInterval(() => {
            frame++;
            const glitched = targetText.split('').map((char) => {
                if (char === ' ' || char === '.' || char === '/') return char;
                if (frame < 3 || (Math.random() > 0.7 && frame < 8)) {
                    return chars[Math.floor(Math.random() * chars.length)];
                }
                return char;
            }).join('');
            setDisplayText(glitched);

            if (frame > 10) {
                setDisplayText(targetText);
            }
        }, 80);

        return () => clearInterval(interval);
    }, [bytes, isAnimated]);

    return <span className={isAnimated ? 'font-mono text-green-400' : ''}>{displayText}</span>;
};

// AeroProgress: the rich bar supports 4 gradient themes today; map the app's
// wider EffectiveTheme set onto them deterministically (extra themes fall back
// to the dark gradient until per-theme gradients land, tracked in the audit).
type BarTheme = 'light' | 'dark' | 'tokyo' | 'cyber';
function toBarTheme(effective: string): BarTheme {
    if (effective === 'tokyo') return 'tokyo';
    if (effective === 'cyber') return 'cyber';
    if (effective === 'light') return 'light';
    return 'dark';
}

// ============ Progress Lanes (per-file parallel channels) ============
// One compact row per active / just-finished transfer lane (`lanes[]`), each
// carrying its OWN real bytes/speed from the per-file executor events. This is
// what makes parallel upload+download visible with real, synced data.
interface ProgressLanesProps {
    lanes: TransferToastLane[];
    styles: ReturnType<typeof getToastStyles>;
    barTheme: BarTheme;
}

export const ProgressLanes: React.FC<ProgressLanesProps> = ({ lanes, styles, barTheme }) => {
    if (!lanes || lanes.length === 0) return null;
    return (
        <div className="mt-2.5 border-t border-current/10 pt-2 space-y-1.5 max-h-44 overflow-y-auto pr-1">
            {lanes.map((lane) => {
                const laneUpload = lane.direction === 'upload';
                const tone = lane.state === 'error' ? 'error' : lane.state === 'completed' ? 'success' : 'default';
                const laneName = lane.path ? truncatePath(lane.path, 30) : lane.filename;
                const right = lane.state === 'error'
                    ? 'error'
                    : lane.state === 'completed'
                        ? 'done'
                        : lane.speed_bps > 0
                            ? formatSpeed(lane.speed_bps)
                            : `${lane.percentage}%`;
                return (
                    <div key={lane.id} className="flex items-center gap-2">
                        <div className="shrink-0">
                            {laneUpload
                                ? <Upload size={11} className="text-cyan-400" />
                                : <Download size={11} className="text-orange-400" />}
                        </div>
                        <div className="flex-1 min-w-0">
                            <div className="flex items-center justify-between gap-2 mb-0.5">
                                <span className={`truncate text-[11px] ${styles.subtitle}`} title={lane.path || lane.filename}>
                                    {laneName}
                                </span>
                                <span className={`shrink-0 tabular-nums text-[10px] ${styles.subtitle}`}>{right}</span>
                            </div>
                            <TransferProgressBar
                                percentage={lane.percentage}
                                size="sm"
                                variant={lane.total <= 0 && lane.state !== 'completed' ? 'indeterminate' : 'gradient'}
                                animated={lane.state === 'active' || !lane.state}
                                effectiveTheme={barTheme}
                                tone={tone}
                            />
                        </div>
                    </div>
                );
            })}
        </div>
    );
};

// ============ Progress Card (AeroProgress flagship floating card) ============
// Aggregate header + aggregate bar (speed/ETA/bytes) + per-file lanes + the
// collapsible advanced speed graph (built into TransferProgressBar via
// showGraph/speedHistory). Restored and extended from the pre-TQ-5 TransferToast.
interface ProgressCardProps {
    transfer: TransferToastState;
    onCancel: () => void;
    onMinimize: () => void;
    /** Open the full Transfer Queue panel (optional secondary action). */
    onOpenPanel?: () => void;
}

export const ProgressCard: React.FC<ProgressCardProps> = ({ transfer, onCancel, onMinimize, onOpenPanel }) => {
    const { theme, isDark } = useTheme();
    const effectiveTheme = getEffectiveTheme(theme, isDark);
    const styles = getToastStyles(effectiveTheme);
    const barTheme = toBarTheme(effectiveTheme);
    const summary = transfer.summary;
    const lanes = transfer.lanes ?? [];
    const isUpload = summary.direction === 'upload';
    const isFolderTransfer = summary.total_files != null && summary.total_files > 0;
    const isIndeterminate = !isFolderTransfer && summary.total <= 0;
    const displayName = summary.path ? truncatePath(summary.path) : summary.filename;
    const transferModeLabel = isUpload ? 'UPLOAD' : 'DOWNLOAD';
    const transferStateLabel = isFolderTransfer ? 'BATCH' : (isIndeterminate ? 'STREAM' : 'LIVE');
    const hasGraph = !!transfer.speedHistory && transfer.speedHistory.length > 1;

    // Auto-dismiss safety: once the summary is complete AND no lane is still
    // active, collapse after 3s (mirrors the legacy toast's behaviour).
    useEffect(() => {
        if (summary.percentage >= 100 && lanes.every((l) => l.state !== 'active')) {
            const timer = setTimeout(() => onCancel(), 3000);
            return () => clearTimeout(timer);
        }
    }, [summary.percentage, lanes, onCancel]);

    return (
        <div
            className={`fixed bottom-12 left-1/2 transform -translate-x-1/2 z-40 rounded-xl border px-4 py-3 w-[30rem] max-w-[calc(100vw-2rem)] text-xs ${styles.container}`}
            style={{ isolation: 'isolate', contain: 'layout paint' }}
        >
            <div className="flex items-start gap-3">
                <div className={`mt-0.5 rounded-xl p-2 ${styles.panel} ${isUpload && !isFolderTransfer ? 'animate-pulse' : ''}`}>
                    {isFolderTransfer
                        ? <Folder size={18} className={isUpload ? 'text-cyan-400' : 'text-orange-400'} />
                        : summary.direction === 'download'
                            ? <Download size={18} className="text-orange-400" />
                            : <Upload size={18} className="text-cyan-400" />}
                </div>
                <div className="flex-1 min-w-0">
                    <div className="flex items-start justify-between gap-2">
                        <div className="min-w-0">
                            <div className={`font-semibold truncate ${styles.title}`} title={summary.path || summary.filename}>
                                {displayName}
                            </div>
                            <div className="mt-1 flex flex-wrap items-center gap-1.5 text-[10px]">
                                <span className={`rounded-md px-1.5 py-0.5 font-medium ${styles.badge}`}>{transferModeLabel}</span>
                                <span className={`rounded-md px-1.5 py-0.5 font-medium ${styles.badge}`}>{transferStateLabel}</span>
                                {isFolderTransfer && (
                                    <span className={`rounded-md px-1.5 py-0.5 ${styles.badgeMuted}`}>{summary.transferred}/{summary.total} files</span>
                                )}
                            </div>
                        </div>
                        <div className="flex items-center gap-0.5 shrink-0">
                            <span className={`text-base font-semibold tabular-nums mr-1 ${styles.title}`}>
                                {isIndeterminate ? '...' : `${summary.percentage}%`}
                            </span>
                            {onOpenPanel && (
                                <button onClick={onOpenPanel} className={`p-1 rounded-md transition-opacity opacity-60 hover:opacity-100 ${styles.title}`} title="Open transfer queue">
                                    <Maximize2 size={13} />
                                </button>
                            )}
                            <button onClick={onMinimize} className={`p-1 rounded-md transition-opacity opacity-60 hover:opacity-100 ${styles.title}`} title="Minimize">
                                <Minus size={14} />
                            </button>
                            <button onClick={onCancel} className={`p-1 rounded-full transition-colors ${styles.cancel}`} title="Dismiss">
                                <X size={14} />
                            </button>
                        </div>
                    </div>

                    <div className="mt-2.5">
                        <TransferProgressBar
                            percentage={summary.percentage}
                            speedBps={summary.speed_bps}
                            etaSeconds={summary.eta_seconds}
                            transferredBytes={isFolderTransfer ? undefined : summary.transferred}
                            totalBytes={isFolderTransfer ? undefined : summary.total}
                            currentFile={isFolderTransfer ? summary.transferred : undefined}
                            totalFiles={isFolderTransfer ? summary.total : undefined}
                            size="lg"
                            variant={isIndeterminate ? 'indeterminate' : 'gradient'}
                            animated={!isIndeterminate}
                            effectiveTheme={barTheme}
                            showGraph={hasGraph}
                            speedHistory={transfer.speedHistory}
                        />
                        <div className={`mt-1.5 flex justify-between gap-3 text-[11px] ${styles.subtitle}`}>
                            <span className="tabular-nums">
                                {isFolderTransfer
                                    ? <>{summary.transferred} / {summary.total} files</>
                                    : isIndeterminate
                                        ? formatBytes(summary.total)
                                        : <>{formatBytes(summary.transferred)} / {formatBytes(summary.total)}</>}
                            </span>
                            <span className="tabular-nums text-right">
                                {summary.speed_bps > 0
                                    ? `${formatSpeed(summary.speed_bps)}${summary.eta_seconds > 0 ? ` - ETA ${formatETA(summary.eta_seconds)}` : ''}`
                                    : (isIndeterminate ? 'Streaming...' : (isUpload ? 'Uploading...' : 'Downloading...'))}
                            </span>
                        </div>
                    </div>

                    <ProgressLanes lanes={lanes} styles={styles} barTheme={barTheme} />
                </div>
            </div>
        </div>
    );
};

// ============ Minimized Transfer Indicator (TQ-5) ============
// The Transfer Queue panel is the primary surface from TQ-5 onward; this
// is the demoted affordance shown when the panel is hidden. Click to
// reopen the panel.
interface MinimizedTransferIndicatorProps {
    transfer: TransferToastState;
    onOpen: () => void;
    onCancel: () => void;
}

export const MinimizedTransferIndicator: React.FC<MinimizedTransferIndicatorProps> = ({ transfer, onOpen, onCancel }) => {
    const { theme, isDark } = useTheme();
    const effectiveTheme = getEffectiveTheme(theme, isDark);
    const summary = transfer.summary;
    const isUpload = summary.direction === 'upload';
    const isFolderTransfer = summary.total_files != null && summary.total_files > 0;
    const isIndeterminate = !isFolderTransfer && summary.total <= 0;
    const styles = getToastStyles(effectiveTheme);

    const displayName = summary.path
        ? truncatePath(summary.path, 28)
        : summary.filename;
    const pct = summary.percentage;

    // Auto-dismiss safety: 100% for 3s collapses the chip
    useEffect(() => {
        if (pct >= 100) {
            const timer = setTimeout(() => onCancel(), 3000);
            return () => clearTimeout(timer);
        }
    }, [pct, onCancel]);

    return (
        <div
            className={`fixed bottom-12 left-1/2 transform -translate-x-1/2 z-40 rounded-full border px-3 py-1.5 flex items-center gap-2 text-xs cursor-pointer ${styles.container}`}
            style={{ isolation: 'isolate', contain: 'layout paint' }}
            onClick={onOpen}
            role="button"
            tabIndex={0}
            onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    onOpen();
                }
            }}
            title={summary.path || summary.filename}
        >
            <div className={`rounded-full p-1 ${styles.panel}`}>
                {isFolderTransfer ? (
                    <Folder size={12} className={isUpload ? 'text-cyan-400' : 'text-orange-400'} />
                ) : isUpload ? (
                    <Upload size={12} className="text-cyan-400" />
                ) : (
                    <Download size={12} className="text-orange-400" />
                )}
            </div>
            <span className={`truncate max-w-[14rem] ${styles.title}`}>
                {displayName}
            </span>
            <span className={`tabular-nums font-semibold ${styles.title}`}>
                {isFolderTransfer
                    ? `${summary.transferred}/${summary.total}`
                    : isIndeterminate
                        ? '...'
                        : `${pct}%`}
            </span>
            <button
                onClick={(e) => { e.stopPropagation(); onCancel(); }}
                className={`shrink-0 p-0.5 rounded-full transition-colors ${styles.cancel}`}
                title="Dismiss"
            >
                <X size={12} />
            </button>
        </div>
    );
};

/** Theme-specific styles for the minimized transfer indicator (TQ-5).
 *  The Transfer Queue panel is the primary surface; this only styles the
 *  small affordance that pops the panel open after a manual dismiss. */
function getToastStyles(theme: string) {
    switch (theme) {
        case 'cyber':
            return {
                container: 'bg-[#0a0e17] border-cyan-900/40 shadow-2xl',
                panel: 'bg-cyan-950/30',
                title: 'text-cyan-100',
                subtitle: 'text-cyan-400/65',
                badge: 'bg-cyan-950/40 text-cyan-300',
                badgeMuted: 'bg-cyan-950/30 text-cyan-100/60',
                cancel: 'text-cyan-700/70 hover:text-red-400 hover:bg-red-900/20',
            };
        case 'tokyo':
            return {
                container: 'bg-[#1a1b2e] border-purple-900/40 shadow-2xl',
                panel: 'bg-purple-950/30',
                title: 'text-purple-100',
                subtitle: 'text-purple-300/65',
                badge: 'bg-purple-950/40 text-purple-200',
                badgeMuted: 'bg-purple-950/30 text-purple-100/60',
                cancel: 'text-purple-600/70 hover:text-red-400 hover:bg-red-900/20',
            };
        case 'light':
            return {
                container: 'bg-white border-gray-200 shadow-2xl',
                panel: 'bg-gray-50',
                title: 'text-gray-900',
                subtitle: 'text-gray-500',
                badge: 'bg-gray-100 text-gray-700',
                badgeMuted: 'bg-gray-100 text-gray-500',
                cancel: 'text-gray-400 hover:text-red-500 hover:bg-red-50',
            };
        default: // dark
            return {
                container: 'bg-gray-800 border-gray-700/50 shadow-2xl',
                panel: 'bg-gray-900/50',
                title: 'text-gray-100',
                subtitle: 'text-gray-400',
                badge: 'bg-gray-700/50 text-gray-200',
                badgeMuted: 'bg-gray-700/30 text-gray-400',
                cancel: 'text-gray-500/70 hover:text-red-400 hover:bg-red-900/30',
            };
    }
}

