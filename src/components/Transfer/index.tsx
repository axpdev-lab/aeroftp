/**
 * Transfer progress components
 */

import React, { useState, useEffect } from 'react';
import { Download, Upload, X } from 'lucide-react';
import { formatBytes, formatSpeed, formatETA } from '../../utils/formatters';

// Transfer progress data structure
export interface TransferProgress {
    filename: string;
    total: number;
    transferred: number;
    percentage: number;
    speed_bps: number;
    eta_seconds: number;
    direction: 'download' | 'upload';
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
            // Create glitchy effect by replacing some chars with random ones
            const glitched = targetText.split('').map((char, i) => {
                if (char === ' ' || char === '.' || char === '/') return char;
                // More glitch at start, stabilize over time
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

// ============ Progress Bar (Apple-style) ============
interface TransferProgressBarProps {
    transfer: TransferProgress;
    onCancel: () => void;
}

export const TransferProgressBar: React.FC<TransferProgressBarProps> = ({ transfer, onCancel }) => {
    const isUpload = transfer.direction === 'upload';
    const isIndeterminate = isUpload && transfer.percentage < 5;

    // Auto-dismiss safety: if stuck at 100% for 3 seconds, dismiss the toast
    useEffect(() => {
        if (transfer.percentage >= 100) {
            const timer = setTimeout(() => onCancel(), 3000);
            return () => clearTimeout(timer);
        }
    }, [transfer.percentage, onCancel]);

    return (
        <div className="fixed bottom-4 left-1/2 transform -translate-x-1/2 z-40 bg-white/90 dark:bg-gray-800/90 backdrop-blur-xl rounded-2xl shadow-2xl border border-gray-200 dark:border-gray-700 p-4 min-w-96">
            <div className="flex items-center gap-4">
                <div className={`text-2xl ${isUpload ? 'animate-pulse' : ''}`}>
                    {transfer.direction === 'download' ? (
                        <Download size={24} className="text-blue-500" />
                    ) : (
                        <Upload size={24} className="text-green-500" />
                    )}
                </div>
                <div className="flex-1">
                    <div className="flex justify-between items-center mb-2">
                        <span className="font-medium text-gray-900 dark:text-gray-100 truncate max-w-48">
                            {transfer.filename}
                        </span>
                        <span className="text-sm text-gray-500 dark:text-gray-400">
                            {isIndeterminate ? '∞' : `${transfer.percentage}%`}
                        </span>
                    </div>
                    <div className="w-full h-2 bg-gray-200 dark:bg-gray-700 rounded-full overflow-hidden">
                        {isIndeterminate ? (
                            <div
                                className="h-full w-1/3 bg-gradient-to-r from-green-500 to-emerald-400 rounded-full"
                                style={{ animation: 'indeterminate 1.5s ease-in-out infinite' }}
                            />
                        ) : (
                            <div
                                className="h-full bg-gradient-to-r from-blue-500 to-cyan-400 rounded-full transition-all duration-300"
                                style={{ width: `${Math.max(transfer.percentage, 2)}%` }}
                            />
                        )}
                    </div>
                    <div className="flex justify-between mt-1.5 text-xs text-gray-500 dark:text-gray-400">
                        <span>
                            <AnimatedBytes bytes={transfer.transferred} isAnimated={isIndeterminate} />
                            {' / '}
                            {formatBytes(transfer.total)}
                        </span>
                        <span>
                            {isIndeterminate
                                ? '⚡ Streaming...'
                                : (transfer.speed_bps > 0
                                    ? `${formatSpeed(transfer.speed_bps)} • ETA ${formatETA(transfer.eta_seconds)}`
                                    : 'Transferring...'
                                )
                            }
                        </span>
                    </div>
                </div>
                <button
                    onClick={onCancel}
                    className="p-2 text-gray-500 hover:text-red-500 hover:bg-red-50 dark:hover:bg-red-900/30 rounded-lg transition-colors"
                >
                    <X size={18} />
                </button>
            </div>
        </div>
    );
};
