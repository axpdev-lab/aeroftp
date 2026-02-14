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
    transfer: TransferProgress;
    onCancel: () => void;
}

/** Theme-specific styles for the transfer toast */
function getToastStyles(theme: string) {
    switch (theme) {
        case 'cyber':
            return {
                container: 'bg-[#0a0e17]/95 border-cyan-900/50 shadow-[0_0_30px_rgba(34,211,238,0.15)]',
                title: 'text-cyan-100',
                subtitle: 'text-cyan-400/70',
                cancel: 'text-cyan-700 hover:text-red-400 hover:bg-red-900/30',
            };
        case 'tokyo':
            return {
                container: 'bg-[#1a1b2e]/95 border-purple-800/50 shadow-[0_0_30px_rgba(168,85,247,0.15)]',
                title: 'text-purple-100',
                subtitle: 'text-purple-300/70',
                cancel: 'text-purple-600 hover:text-red-400 hover:bg-red-900/30',
            };
        case 'light':
            return {
                container: 'bg-white/95 border-gray-200 shadow-2xl',
                title: 'text-gray-900',
                subtitle: 'text-gray-500',
                cancel: 'text-gray-400 hover:text-red-500 hover:bg-red-50',
            };
        default: // dark
            return {
                container: 'bg-gray-900/95 border-gray-700 shadow-2xl',
                title: 'text-gray-100',
                subtitle: 'text-gray-400',
                cancel: 'text-gray-500 hover:text-red-400 hover:bg-red-900/30',
            };
    }
}

export const TransferToast: React.FC<TransferToastProps> = ({ transfer, onCancel }) => {
    const { theme, isDark } = useTheme();
    const effectiveTheme = getEffectiveTheme(theme, isDark);
    const isUpload = transfer.direction === 'upload';
    const isFolderTransfer = transfer.total_files != null && transfer.total_files > 0;
    const isIndeterminate = isUpload && transfer.percentage < 5 && !isFolderTransfer;
    const styles = getToastStyles(effectiveTheme);

    // Display name: use truncated path if available, otherwise just filename
    const displayName = transfer.path
        ? truncatePath(transfer.path)
        : transfer.filename;

    // Auto-dismiss safety: if stuck at 100% for 3 seconds, dismiss the toast
    useEffect(() => {
        if (transfer.percentage >= 100) {
            const timer = setTimeout(() => onCancel(), 3000);
            return () => clearTimeout(timer);
        }
    }, [transfer.percentage, onCancel]);

    return (
        <div
            className={`fixed bottom-12 left-1/2 transform -translate-x-1/2 z-40 backdrop-blur-xl rounded-2xl border p-4 min-w-96 ${styles.container}`}
            style={{ isolation: 'isolate', contain: 'layout paint' }}
        >
            <div className="flex items-center gap-4">
                <div className={`text-2xl ${isUpload && !isFolderTransfer ? 'animate-pulse' : ''}`}>
                    {isFolderTransfer ? (
                        <Folder size={24} className={isUpload ? 'text-green-500' : 'text-blue-500'} />
                    ) : transfer.direction === 'download' ? (
                        <Download size={24} className="text-blue-500" />
                    ) : (
                        <Upload size={24} className="text-green-500" />
                    )}
                </div>
                <div className="flex-1">
                    <div className="flex justify-between items-center mb-2">
                        <span
                            className={`font-medium truncate max-w-64 ${styles.title}`}
                            title={transfer.path || transfer.filename}
                        >
                            {displayName}
                        </span>
                        <span className={`text-sm ${styles.subtitle}`}>
                            {isIndeterminate ? '...' : `${transfer.percentage}%`}
                        </span>
                    </div>
                    <TransferProgressBar
                        percentage={transfer.percentage}
                        speedBps={transfer.speed_bps}
                        etaSeconds={transfer.eta_seconds}
                        transferredBytes={isFolderTransfer ? undefined : transfer.transferred}
                        totalBytes={isFolderTransfer ? undefined : transfer.total}
                        currentFile={isFolderTransfer ? transfer.transferred : undefined}
                        totalFiles={isFolderTransfer ? transfer.total : undefined}
                        size="lg"
                        variant={isIndeterminate ? 'indeterminate' : 'gradient'}
                        animated={!isIndeterminate}
                    />
                    <div className={`flex justify-between mt-1.5 text-xs ${styles.subtitle}`}>
                        <span>
                            {isFolderTransfer ? (
                                <>{transfer.transferred} / {transfer.total} files</>
                            ) : isIndeterminate ? (
                                formatBytes(transfer.total)
                            ) : (
                                <>{formatBytes(transfer.transferred)} / {formatBytes(transfer.total)}</>
                            )}
                        </span>
                        <span>
                            {isFolderTransfer
                                ? (transfer.transferred < transfer.total
                                    ? (transfer.speed_bps > 0
                                        ? `${formatSpeed(transfer.speed_bps)} \u2022 ${isUpload ? 'Uploading...' : 'Downloading...'}`
                                        : (isUpload ? 'Uploading...' : 'Downloading...'))
                                    : 'Complete'
                                )
                                : isIndeterminate
                                    ? 'Streaming...'
                                    : (transfer.speed_bps > 0
                                        ? `${formatSpeed(transfer.speed_bps)} \u2022 ETA ${formatETA(transfer.eta_seconds)}`
                                        : 'Transferring...'
                                    )
                            }
                        </span>
                    </div>
                </div>
                <button
                    onClick={onCancel}
                    className={`p-2 rounded-lg transition-colors ${styles.cancel}`}
                >
                    <X size={18} />
                </button>
            </div>
        </div>
    );
};
