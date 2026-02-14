import * as React from 'react';
import { useState, useEffect, useRef } from 'react';
import { Upload, Check, X, Clock, Loader2 } from 'lucide-react';
import { formatBytes } from '../utils/formatters';
import { TransferProgressBar } from './TransferProgressBar';

export type UploadStatus = 'pending' | 'uploading' | 'completed' | 'error';

export interface UploadItem {
    id: string;
    filename: string;
    path: string;
    size: number;
    status: UploadStatus;
    progress?: number;
    error?: string;
    startTime?: number;
    endTime?: number;
}

interface UploadQueueProps {
    items: UploadItem[];
    onClear?: () => void;
    isVisible: boolean;
    onToggle: () => void;
}

const StatusIcon: React.FC<{ status: UploadStatus }> = ({ status }) => {
    switch (status) {
        case 'pending':
            return <Clock size={12} className="text-gray-400" />;
        case 'uploading':
            return <Loader2 size={12} className="text-cyan-400 animate-spin" />;
        case 'completed':
            return <Check size={12} className="text-green-400" />;
        case 'error':
            return <X size={12} className="text-red-400" />;
    }
};

const formatTime = (ms: number): string => {
    if (ms < 1000) return `${ms}ms`;
    if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
    return `${Math.floor(ms / 60000)}m ${Math.floor((ms % 60000) / 1000)}s`;
};

export const UploadQueue: React.FC<UploadQueueProps> = ({
    items,
    onClear,
    isVisible,
    onToggle
}) => {
    const scrollRef = useRef<HTMLDivElement>(null);
    const [autoScroll, setAutoScroll] = useState(true);

    // Auto-scroll to bottom when new items added
    useEffect(() => {
        if (autoScroll && scrollRef.current) {
            scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
        }
    }, [items, autoScroll]);

    const handleScroll = () => {
        if (scrollRef.current) {
            const { scrollTop, scrollHeight, clientHeight } = scrollRef.current;
            setAutoScroll(scrollTop + clientHeight >= scrollHeight - 20);
        }
    };

    const completedCount = items.filter(i => i.status === 'completed').length;
    const errorCount = items.filter(i => i.status === 'error').length;
    const uploadingCount = items.filter(i => i.status === 'uploading').length;
    const pendingCount = items.filter(i => i.status === 'pending').length;

    if (items.length === 0) return null;

    return (
        <div className="fixed bottom-12 right-4 z-40 w-96 max-h-80 flex flex-col bg-gray-900 border border-gray-700 rounded-lg shadow-2xl overflow-hidden font-mono text-xs">
            {/* Header - Terminal Style */}
            <div
                className="flex items-center justify-between px-3 py-2 bg-gray-800 border-b border-gray-700 cursor-pointer select-none"
                onClick={onToggle}
            >
                <div className="flex items-center gap-2">
                    <Upload size={14} className="text-cyan-400" />
                    <span className="text-gray-300 font-medium">Upload Queue</span>
                    <span className="text-gray-500">
                        [{completedCount}/{items.length}]
                    </span>
                </div>
                <div className="flex items-center gap-3">
                    {uploadingCount > 0 && (
                        <span className="text-cyan-400 flex items-center gap-1">
                            <Loader2 size={10} className="animate-spin" />
                            {uploadingCount}
                        </span>
                    )}
                    {pendingCount > 0 && (
                        <span className="text-gray-400">{pendingCount} pending</span>
                    )}
                    {errorCount > 0 && (
                        <span className="text-red-400">{errorCount} failed</span>
                    )}
                    {onClear && completedCount === items.length && (
                        <button
                            onClick={(e) => { e.stopPropagation(); onClear(); }}
                            className="text-gray-500 hover:text-gray-300 transition-colors"
                        >
                            clear
                        </button>
                    )}
                </div>
            </div>

            {/* Queue List - Terminal Output */}
            {isVisible && (
                <div
                    ref={scrollRef}
                    onScroll={handleScroll}
                    className="flex-1 overflow-y-auto p-2 space-y-0.5 max-h-60 scrollbar-thin scrollbar-thumb-gray-700 scrollbar-track-transparent"
                >
                    {items.map((item, index) => (
                        <div
                            key={item.id}
                            className={`flex items-center gap-2 px-2 py-1 rounded transition-all duration-300 ${item.status === 'uploading'
                                    ? 'bg-cyan-900/20 border-l-2 border-cyan-400'
                                    : item.status === 'error'
                                        ? 'bg-red-900/20'
                                        : item.status === 'completed'
                                            ? 'text-gray-500'
                                            : 'text-gray-400'
                                }`}
                            style={{
                                animation: item.status === 'uploading' ? 'pulse 2s infinite' : 'none'
                            }}
                        >
                            {/* Line Number */}
                            <span className="text-gray-600 w-6 text-right shrink-0">
                                {String(index + 1).padStart(3, '0')}
                            </span>

                            {/* Status Icon */}
                            <StatusIcon status={item.status} />

                            {/* Filename */}
                            <span className={`flex-1 truncate ${item.status === 'completed' ? 'text-gray-500' : 'text-gray-300'
                                }`}>
                                {item.filename}
                            </span>

                            {/* Size */}
                            <span className="text-gray-600 shrink-0">
                                {formatBytes(item.size)}
                            </span>

                            {/* Progress or Time */}
                            <span className="text-gray-500 w-14 text-right shrink-0">
                                {item.status === 'uploading' && item.progress !== undefined
                                    ? `${item.progress}%`
                                    : item.status === 'completed' && item.startTime && item.endTime
                                        ? formatTime(item.endTime - item.startTime)
                                        : item.status === 'error'
                                            ? 'FAIL'
                                            : '—'
                                }
                            </span>
                        </div>
                    ))}
                </div>
            )}

            {/* Footer Progress Bar */}
            {uploadingCount > 0 && (
                <TransferProgressBar
                    percentage={(completedCount / items.length) * 100}
                    size="sm"
                    variant="gradient"
                    animated={false}
                />
            )}
        </div>
    );
};

// Hook for managing upload queue
let uploadId = 0;

export const useUploadQueue = () => {
    const [items, setItems] = useState<UploadItem[]>([]);
    const [isVisible, setIsVisible] = useState(true);

    const addItem = (filename: string, path: string, size: number): string => {
        const id = `upload-${++uploadId}`;
        setItems(prev => [...prev, {
            id,
            filename,
            path,
            size,
            status: 'pending',
            startTime: Date.now()
        }]);
        return id;
    };

    const updateStatus = (id: string, status: UploadStatus, progress?: number, error?: string) => {
        setItems(prev => prev.map(item =>
            item.id === id
                ? {
                    ...item,
                    status,
                    progress,
                    error,
                    endTime: status === 'completed' || status === 'error' ? Date.now() : item.endTime
                }
                : item
        ));
    };

    const startUpload = (id: string) => updateStatus(id, 'uploading', 0);
    const setProgress = (id: string, progress: number) => updateStatus(id, 'uploading', progress);
    const completeUpload = (id: string) => updateStatus(id, 'completed', 100);
    const failUpload = (id: string, error: string) => updateStatus(id, 'error', undefined, error);

    const clear = () => {
        setItems(prev => prev.filter(i => i.status === 'uploading' || i.status === 'pending'));
    };

    const toggle = () => setIsVisible(v => !v);

    return {
        items,
        isVisible,
        addItem,
        startUpload,
        setProgress,
        completeUpload,
        failUpload,
        clear,
        toggle
    };
};

export default UploadQueue;
