// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Transfer progress components
 */

import React, { useState, useEffect } from 'react';
import { Download, Upload, Folder, X } from 'lucide-react';
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

// ============ Transfer Toast (floating notification) ============
interface TransferToastProps {
    transfer: TransferToastState;
    onCancel: () => void;
}

/** Theme-specific styles for the transfer toast */
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

export const TransferToast: React.FC<TransferToastProps> = ({ transfer, onCancel }) => {
    const { theme, isDark } = useTheme();
    const effectiveTheme = getEffectiveTheme(theme, isDark);
    const summary = transfer.summary;
    const isUpload = summary.direction === 'upload';
    const isFolderTransfer = summary.total_files != null && summary.total_files > 0;
    const isIndeterminate = !isFolderTransfer && summary.total <= 0;
    const styles = getToastStyles(effectiveTheme);

    // Display name: use truncated path if available, otherwise just filename
    const displayName = summary.path
        ? truncatePath(summary.path)
        : summary.filename;
    const transferModeLabel = isUpload ? 'UPLOAD' : 'DOWNLOAD';
    const transferStateLabel = isFolderTransfer ? 'BATCH' : (isIndeterminate ? 'STREAM' : 'LIVE');

    // Auto-dismiss safety: if stuck at 100% for 3 seconds, dismiss the toast
    useEffect(() => {
        if (summary.percentage >= 100) {
            const timer = setTimeout(() => onCancel(), 3000);
            return () => clearTimeout(timer);
        }
    }, [summary.percentage, onCancel]);

    return (
        <div
            className={`fixed bottom-12 left-1/2 transform -translate-x-1/2 z-40 rounded-xl border px-4 py-3 w-[30rem] max-w-[calc(100vw-2rem)] text-xs ${styles.container}`}
            style={{ isolation: 'isolate', contain: 'layout paint' }}
        >
            <div className="flex items-start gap-3">
                <div className={`mt-0.5 rounded-xl p-2 ${styles.panel} ${isUpload && !isFolderTransfer ? 'animate-pulse' : ''}`}>
                    {isFolderTransfer ? (
                        <Folder size={18} className={isUpload ? 'text-cyan-400' : 'text-orange-400'} />
                    ) : summary.direction === 'download' ? (
                        <Download size={18} className="text-orange-400" />
                    ) : (
                        <Upload size={18} className="text-cyan-400" />
                    )}
                </div>
                <div className="flex-1 min-w-0">
                    <div className="flex items-start justify-between gap-3">
                        <div className="min-w-0">
                            <div
                                className={`font-semibold truncate ${styles.title}`}
                                title={summary.path || summary.filename}
                            >
                                {displayName}
                            </div>
                            <div className="mt-1 flex flex-wrap items-center gap-1.5 text-[10px]">
                                <span className={`rounded-md px-1.5 py-0.5 font-medium ${styles.badge}`}>
                                    {transferModeLabel}
                                </span>
                                <span className={`rounded-md px-1.5 py-0.5 font-medium ${styles.badge}`}>
                                    {transferStateLabel}
                                </span>
                                {isFolderTransfer && (
                                    <span className={`rounded-md px-1.5 py-0.5 ${styles.badgeMuted}`}>
                                        {summary.transferred}/{summary.total} files
                                    </span>
                                )}
                            </div>
                        </div>
                        <div className="text-right shrink-0">
                            <div className={`text-base font-semibold tabular-nums ${styles.title}`}>
                                {isIndeterminate ? '...' : `${summary.percentage}%`}
                            </div>
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
                        />
                        <div className={`mt-1.5 flex justify-between gap-3 text-[11px] ${styles.subtitle}`}>
                            <span className="tabular-nums">
                                {isFolderTransfer ? (
                                    <>{summary.transferred} / {summary.total} files</>
                                ) : isIndeterminate ? (
                                    formatBytes(summary.total)
                                ) : (
                                    <>{formatBytes(summary.transferred)} / {formatBytes(summary.total)}</>
                                )}
                            </span>
                            <span className="tabular-nums text-right">
                                {isFolderTransfer
                                    ? (summary.transferred < summary.total
                                        ? (summary.speed_bps > 0
                                            ? `${formatSpeed(summary.speed_bps)}${summary.eta_seconds > 0 ? ` - ETA ${formatETA(summary.eta_seconds)}` : ''}`
                                            : (isUpload ? 'Uploading...' : 'Downloading...'))
                                        : 'Complete'
                                    )
                                    : isIndeterminate
                                        ? 'Streaming...'
                                        : (summary.speed_bps > 0
                                            ? `${formatSpeed(summary.speed_bps)} - ETA ${formatETA(summary.eta_seconds)}`
                                            : 'Transferring...'
                                        )
                                }
                            </span>
                        </div>
                    </div>

                </div>
                <button
                    onClick={onCancel}
                    className={`shrink-0 p-1 rounded-full transition-colors ${styles.cancel}`}
                >
                    <X size={16} />
                </button>
            </div>
        </div>
    );
};
