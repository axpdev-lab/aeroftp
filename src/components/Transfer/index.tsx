// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Transfer progress components
 */

import React, { useState, useEffect } from 'react';
import { Download, Upload, Folder, X } from 'lucide-react';
import { formatBytes } from '../../utils/formatters';
import { useTheme, getEffectiveTheme } from '../../hooks/useTheme';

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

