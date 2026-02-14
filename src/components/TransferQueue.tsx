import * as React from 'react';
import { useState, useEffect, useRef, useCallback } from 'react';
import { Upload, Download, Check, X, Clock, Loader2, Folder, RotateCcw, Trash2, Copy, Square, ChevronDown } from 'lucide-react';
import { formatBytes } from '../utils/formatters';
import { useTranslation } from '../i18n';
import { TransferProgressBar } from './TransferProgressBar';

export type TransferStatus = 'pending' | 'transferring' | 'completed' | 'error';
export type TransferType = 'upload' | 'download';

export interface TransferItem {
    id: string;
    filename: string;
    path: string;
    size: number;
    status: TransferStatus;
    type: TransferType;
    progress?: number;
    error?: string;
    startTime?: number;
    endTime?: number;
    // For folder transfers
    isFolder?: boolean;
    totalFiles?: number;
    completedFiles?: number;
}

interface TransferQueueProps {
    items: TransferItem[];
    onClear?: () => void;
    onClearCompleted?: () => void;
    onStopAll?: () => void;
    onRemoveItem?: (id: string) => void;
    onRetryItem?: (id: string) => void;
    isVisible: boolean;
    onToggle: () => void;
}

const StatusIcon: React.FC<{ status: TransferStatus }> = ({ status }) => {
    switch (status) {
        case 'pending':
            return <Clock size={12} className="text-gray-400" />;
        case 'transferring':
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

// ============ Context Menu for Queue Items ============
interface QueueContextMenuProps {
    x: number;
    y: number;
    item: TransferItem;
    onRetry?: (id: string) => void;
    onRemove?: (id: string) => void;
    onClose: () => void;
}

const QueueContextMenu: React.FC<QueueContextMenuProps> = ({ x, y, item, onRetry, onRemove, onClose }) => {
    const t = useTranslation();
    const menuRef = useRef<HTMLDivElement>(null);

    useEffect(() => {
        const handleClick = (e: MouseEvent) => {
            if (menuRef.current && !menuRef.current.contains(e.target as Node)) onClose();
        };
        const handleKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        document.addEventListener('mousedown', handleClick);
        document.addEventListener('keydown', handleKey);
        return () => { document.removeEventListener('mousedown', handleClick); document.removeEventListener('keydown', handleKey); };
    }, [onClose]);

    // Adjust position to stay within viewport
    const adjustedX = Math.min(x, window.innerWidth - 200);
    const adjustedY = Math.min(y, window.innerHeight - 150);

    const menuItem = (icon: React.ReactNode, label: string, onClick: () => void, className = 'text-gray-300 hover:bg-gray-700') => (
        <button
            className={`flex items-center gap-2 w-full px-3 py-1.5 text-xs ${className} transition-colors`}
            onClick={() => { onClick(); onClose(); }}
        >
            {icon}{label}
        </button>
    );

    const copyItemDetails = () => {
        const dir = item.type === 'upload' ? 'UPLOAD' : 'DOWNLOAD';
        const st = item.status.toUpperCase();
        const sz = item.size > 0 ? formatBytes(item.size) : '0 B';
        const time = item.startTime && item.endTime ? ` ${((item.endTime - item.startTime) / 1000).toFixed(1)}s` : '';
        const err = item.error ? ` [${item.error}]` : '';
        const path = item.path ? ` (${item.path})` : '';
        navigator.clipboard.writeText(`${dir} ${st} ${item.filename}${path} ${sz}${time}${err}`);
    };

    return (
        <div
            ref={menuRef}
            className="fixed z-[200] bg-gray-800 border border-gray-600 rounded-lg shadow-xl py-1 min-w-[160px]"
            style={{ left: adjustedX, top: adjustedY }}
        >
            {menuItem(<Copy size={12} />, t('transfer.copy'), copyItemDetails)}
            {item.status === 'error' && onRetry && (
                menuItem(<RotateCcw size={12} />, t('transfer.retry'), () => onRetry(item.id))
            )}
            {item.status === 'error' && item.error && (
                menuItem(<Copy size={12} />, t('transfer.copyError'), () => {
                    navigator.clipboard.writeText(item.error || '');
                })
            )}
            {onRemove && item.status !== 'transferring' && (
                menuItem(<Trash2 size={12} />, t('transfer.remove'), () => onRemove(item.id), 'text-gray-300 hover:bg-red-900/40 hover:text-red-300')
            )}
        </div>
    );
};

// ============ Header Dropdown Menu ============
interface HeaderDropdownProps {
    onClear?: () => void;
    onClearCompleted?: () => void;
    onStopAll?: () => void;
    hasCompleted: boolean;
    hasPending: boolean;
    hasItems: boolean;
}

const HeaderDropdown: React.FC<HeaderDropdownProps> = ({ onClear, onClearCompleted, onStopAll, hasCompleted, hasPending, hasItems }) => {
    const t = useTranslation();
    const [isOpen, setIsOpen] = useState(false);
    const buttonRef = useRef<HTMLButtonElement>(null);
    const dropdownRef = useRef<HTMLDivElement>(null);
    const [pos, setPos] = useState({ top: 0, right: 0 });

    useEffect(() => {
        if (!isOpen) return;
        const handleClick = (e: MouseEvent) => {
            if (dropdownRef.current && !dropdownRef.current.contains(e.target as Node) &&
                buttonRef.current && !buttonRef.current.contains(e.target as Node)) setIsOpen(false);
        };
        document.addEventListener('mousedown', handleClick);
        return () => document.removeEventListener('mousedown', handleClick);
    }, [isOpen]);

    const handleToggle = (e: React.MouseEvent) => {
        e.stopPropagation();
        if (!isOpen && buttonRef.current) {
            const rect = buttonRef.current.getBoundingClientRect();
            setPos({ top: rect.bottom + 4, right: window.innerWidth - rect.right });
        }
        setIsOpen(!isOpen);
    };

    const menuItem = (icon: React.ReactNode, label: string, onClick: () => void, disabled: boolean, className = '') => (
        <button
            className={`flex items-center gap-2 w-full px-3 py-1.5 text-xs transition-colors ${disabled ? 'text-gray-600 cursor-not-allowed' : `text-gray-300 hover:bg-gray-700 ${className}`}`}
            onClick={() => { if (!disabled) { onClick(); setIsOpen(false); } }}
            disabled={disabled}
        >
            {icon}{label}
        </button>
    );

    return (
        <>
            <button
                ref={buttonRef}
                onClick={handleToggle}
                className="text-gray-500 hover:text-gray-300 transition-colors flex items-center gap-0.5"
            >
                <ChevronDown size={14} />
            </button>
            {isOpen && (
                <div ref={dropdownRef} className="fixed bg-gray-800 border border-gray-600 rounded-lg shadow-xl py-1 min-w-[170px] z-[200]"
                    style={{ top: pos.top, right: pos.right }}
                >
                    {onClearCompleted && menuItem(<Check size={12} />, t('transfer.clearCompleted'), onClearCompleted, !hasCompleted)}
                    {onStopAll && menuItem(<Square size={12} />, t('transfer.stopAllPending'), onStopAll, !hasPending)}
                    {onClear && menuItem(<Trash2 size={12} />, t('transfer.clearAll'), onClear, !hasItems, 'hover:text-red-300')}
                </div>
            )}
        </>
    );
};

// ============ Main TransferQueue Component ============
export const TransferQueue: React.FC<TransferQueueProps> = ({
    items,
    onClear,
    onClearCompleted,
    onStopAll,
    onRemoveItem,
    onRetryItem,
    isVisible,
    onToggle
}) => {
    const t = useTranslation();
    const scrollRef = useRef<HTMLDivElement>(null);
    const [autoScroll, setAutoScroll] = useState(true);
    const [contextMenu, setContextMenu] = useState<{ x: number; y: number; item: TransferItem } | null>(null);

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

    const handleContextMenu = useCallback((e: React.MouseEvent, item: TransferItem) => {
        e.preventDefault();
        e.stopPropagation();
        setContextMenu({ x: e.clientX, y: e.clientY, item });
    }, []);

    const completedCount = items.filter(i => i.status === 'completed').length;
    const errorCount = items.filter(i => i.status === 'error').length;
    const transferringCount = items.filter(i => i.status === 'transferring').length;
    const pendingCount = items.filter(i => i.status === 'pending').length;

    // Determine icon based on majority transfer type
    const uploadCount = items.filter(i => i.type === 'upload').length;
    const downloadCount = items.filter(i => i.type === 'download').length;
    const primaryType: TransferType = uploadCount >= downloadCount ? 'upload' : 'download';

    // Only render if visible AND has items
    if (!isVisible || items.length === 0) return null;

    return (
        <>
            <div className="fixed bottom-12 right-4 z-40 w-[28rem] max-h-[28rem] flex flex-col bg-gray-900 border border-gray-700 rounded-lg shadow-2xl overflow-hidden font-mono text-xs">
                {/* Header - Terminal Style */}
                <div
                    className="flex items-center justify-between px-3 py-2 bg-gray-800 border-b border-gray-700 cursor-pointer select-none"
                    onClick={onToggle}
                >
                    <div className="flex items-center gap-2">
                        {primaryType === 'upload'
                            ? <Upload size={14} className="text-cyan-400" />
                            : <Download size={14} className="text-orange-400" />
                        }
                        <span className="text-gray-300 font-medium">{t('transfer.queue')}</span>
                        <span className="text-gray-500">
                            [{completedCount}/{items.length}]
                        </span>
                    </div>
                    <div className="flex items-center gap-2">
                        {transferringCount > 0 && (
                            <span className="text-cyan-400 flex items-center gap-1">
                                <Loader2 size={10} className="animate-spin" />
                                {transferringCount}
                            </span>
                        )}
                        {pendingCount > 0 && (
                            <span className="text-gray-400">{pendingCount} {t('transfer.pending')}</span>
                        )}
                        {errorCount > 0 && (
                            <span className="text-red-400">{errorCount} {t('transfer.failed')}</span>
                        )}

                        {/* Quick action buttons */}
                        <div className="flex items-center gap-0.5 ml-1 border-l border-gray-700 pl-2">
                            {onClearCompleted && (
                                <button
                                    onClick={(e) => { e.stopPropagation(); onClearCompleted(); }}
                                    disabled={completedCount === 0}
                                    className={`p-1 rounded transition-colors ${completedCount > 0 ? 'text-gray-400 hover:text-green-400 hover:bg-gray-700' : 'text-gray-700 cursor-not-allowed'}`}
                                    title={t('transfer.clearCompleted')}
                                >
                                    <Check size={13} />
                                </button>
                            )}
                            {onStopAll && (
                                <button
                                    onClick={(e) => { e.stopPropagation(); onStopAll(); }}
                                    disabled={transferringCount === 0 && pendingCount === 0}
                                    className={`p-1 rounded transition-colors ${(transferringCount > 0 || pendingCount > 0) ? 'text-red-400 hover:text-red-300 hover:bg-red-900/30 animate-pulse' : 'text-gray-700 cursor-not-allowed'}`}
                                    title={t('transfer.stopAll')}
                                >
                                    <Square size={13} />
                                </button>
                            )}
                            {onClear && (
                                <button
                                    onClick={(e) => { e.stopPropagation(); onClear(); }}
                                    disabled={items.length === 0}
                                    className={`p-1 rounded transition-colors ${items.length > 0 ? 'text-gray-400 hover:text-red-400 hover:bg-gray-700' : 'text-gray-700 cursor-not-allowed'}`}
                                    title={t('transfer.clearAll')}
                                >
                                    <Trash2 size={13} />
                                </button>
                            )}
                            <button
                                onClick={(e) => {
                                    e.stopPropagation();
                                    const lines = items.map((item, idx) => {
                                        const num = String(idx + 1).padStart(3, '0');
                                        const dir = item.type === 'upload' ? '\u2B06' : '\u2B07';
                                        const st = item.status === 'completed' ? '\u2714' : item.status === 'error' ? '\u2718' : item.status === 'transferring' ? '\u25B6' : '\u25CB';
                                        const folder = item.isFolder ? '\uD83D\uDCC1' : '';
                                        const count = item.isFolder && item.totalFiles ? ` ${item.completedFiles || 0}/${item.totalFiles}` : '';
                                        const sz = item.size > 0 ? ` ${(item.size / 1024).toFixed(1)} KB` : ' 0 B';
                                        const err = item.error ? ` [${item.error}]` : '';
                                        const time = item.startTime && item.endTime ? ` ${((item.endTime - item.startTime) / 1000).toFixed(1)}s` : '';
                                        return `${num} ${dir} ${st} ${folder}${item.filename}${count}${sz}${time}${err}`;
                                    }).join('\n');
                                    const header = `Transfer Queue [${completedCount}/${items.length}]`;
                                    navigator.clipboard.writeText(`${header}\n${lines}`);
                                }}
                                disabled={items.length === 0}
                                className={`p-1 rounded transition-colors ${items.length > 0 ? 'text-gray-400 hover:text-blue-400 hover:bg-gray-700' : 'text-gray-700 cursor-not-allowed'}`}
                                title={t('transfer.copyQueueToClipboard')}
                            >
                                <Copy size={13} />
                            </button>
                            <HeaderDropdown
                                onClear={onClear}
                                onClearCompleted={onClearCompleted}
                                onStopAll={onStopAll}
                                hasCompleted={completedCount > 0}
                                hasPending={transferringCount > 0 || pendingCount > 0}
                                hasItems={items.length > 0}
                            />
                        </div>
                    </div>
                </div>

                {/* Queue List - Terminal Output */}
                <div
                    ref={scrollRef}
                    onScroll={handleScroll}
                    className="flex-1 overflow-y-auto p-2 space-y-0.5 max-h-[24rem] scrollbar-thin scrollbar-thumb-gray-700 scrollbar-track-transparent"
                >
                    {items.map((item, index) => (
                        <div
                            key={item.id}
                            className={`group flex items-center gap-2 px-2 py-1 rounded transition-all duration-300 ${item.status === 'transferring'
                                ? 'bg-cyan-900/20 border-l-2 border-cyan-400'
                                : item.status === 'error'
                                    ? 'bg-red-900/20'
                                    : item.status === 'completed'
                                        ? 'text-gray-500'
                                        : 'text-gray-400'
                                }`}
                            style={{
                                animation: item.status === 'transferring' ? 'pulse 2s infinite' : 'none'
                            }}
                            onContextMenu={(e) => handleContextMenu(e, item)}
                        >
                            {/* Line Number */}
                            <span className="text-gray-600 w-6 text-right shrink-0">
                                {String(index + 1).padStart(3, '0')}
                            </span>

                            {/* Type Icon */}
                            {item.isFolder
                                ? <Folder size={10} className={item.type === 'upload' ? 'text-cyan-500 shrink-0' : 'text-orange-500 shrink-0'} />
                                : item.type === 'upload'
                                    ? <Upload size={10} className="text-cyan-500 shrink-0" />
                                    : <Download size={10} className="text-orange-500 shrink-0" />
                            }

                            {/* Status Icon */}
                            <StatusIcon status={item.status} />

                            {/* Filename — truncate from start to always show extension */}
                            <span className={`flex-1 overflow-hidden ${item.status === 'completed' ? 'text-gray-500' : 'text-gray-300'}`}
                                title={item.filename}
                            >
                                <span className="block" style={{ direction: 'rtl', textAlign: 'left', textOverflow: 'ellipsis', overflow: 'hidden', whiteSpace: 'nowrap' }}>
                                    <bdi>{item.filename}</bdi>
                                </span>
                            </span>

                            {/* Folder file count badge */}
                            {item.isFolder && item.totalFiles !== undefined && item.totalFiles > 0 && (
                                <span className={`px-1.5 py-0.5 rounded text-[10px] font-medium shrink-0 ${item.status === 'completed'
                                    ? 'bg-green-900/50 text-green-400'
                                    : item.status === 'transferring'
                                        ? 'bg-cyan-900/50 text-cyan-400'
                                        : 'bg-gray-700 text-gray-400'
                                    }`}>
                                    {item.completedFiles || 0}/{item.totalFiles}
                                </span>
                            )}

                            {/* Size */}
                            <span className="text-gray-600 shrink-0">
                                {formatBytes(item.size)}
                            </span>

                            {/* Progress or Time or Error with tooltip */}
                            <span className={`w-14 text-right shrink-0 ${item.status === 'error' ? 'text-red-400 cursor-help' : 'text-gray-500'}`}
                                title={item.status === 'error' && item.error ? item.error : undefined}
                            >
                                {item.status === 'transferring' && item.progress !== undefined
                                    ? `${item.progress}%`
                                    : item.status === 'completed' && item.startTime && item.endTime
                                        ? formatTime(item.endTime - item.startTime)
                                        : item.status === 'error'
                                            ? t('transfer.fail')
                                            : '—'
                                }
                            </span>

                            {/* Inline action buttons — hover only */}
                            <span className="hidden group-hover:flex items-center gap-1 shrink-0 ml-1">
                                <button
                                    onClick={(e) => {
                                        e.stopPropagation();
                                        const dir = item.type === 'upload' ? 'UPLOAD' : 'DOWNLOAD';
                                        const st = item.status.toUpperCase();
                                        const sz = item.size > 0 ? formatBytes(item.size) : '0 B';
                                        const time = item.startTime && item.endTime ? ` ${((item.endTime - item.startTime) / 1000).toFixed(1)}s` : '';
                                        const err = item.error ? ` [${item.error}]` : '';
                                        navigator.clipboard.writeText(`${dir} ${st} ${item.filename} ${sz}${time}${err}`);
                                    }}
                                    className="p-0.5 text-gray-600 hover:text-blue-400 transition-colors"
                                    title={t('transfer.copy')}
                                >
                                    <Copy size={10} />
                                </button>
                                {item.status === 'error' && onRetryItem && (
                                    <button
                                        onClick={(e) => { e.stopPropagation(); onRetryItem(item.id); }}
                                        className="p-0.5 text-gray-600 hover:text-cyan-400 transition-colors"
                                        title={t('transfer.retry')}
                                    >
                                        <RotateCcw size={10} />
                                    </button>
                                )}
                                {item.status !== 'transferring' && onRemoveItem && (
                                    <button
                                        onClick={(e) => { e.stopPropagation(); onRemoveItem(item.id); }}
                                        className="p-0.5 text-gray-600 hover:text-red-400 transition-colors"
                                        title={t('transfer.remove')}
                                    >
                                        <X size={10} />
                                    </button>
                                )}
                            </span>
                        </div>
                    ))}
                </div>

                {/* Footer Progress Bar */}
                {transferringCount > 0 && (
                    <TransferProgressBar
                        percentage={(completedCount / items.length) * 100}
                        size="sm"
                        variant="gradient"
                        animated={false}
                    />
                )}
            </div>

            {/* Context menu overlay */}
            {contextMenu && (
                <QueueContextMenu
                    x={contextMenu.x}
                    y={contextMenu.y}
                    item={contextMenu.item}
                    onRetry={onRetryItem}
                    onRemove={onRemoveItem}
                    onClose={() => setContextMenu(null)}
                />
            )}
        </>
    );
};

// Hook for managing transfer queue
let transferId = 0;

export const useTransferQueue = () => {
    const [items, setItems] = useState<TransferItem[]>([]);
    const [isVisible, setIsVisible] = useState(true);
    const autoHideTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

    // Auto-hide after all transfers complete (5 seconds)
    useEffect(() => {
        const allCompleted = items.length > 0 && items.every(i => i.status === 'completed' || i.status === 'error');
        const hasActive = items.some(i => i.status === 'transferring' || i.status === 'pending');

        // Clear any existing timeout
        if (autoHideTimeoutRef.current) {
            clearTimeout(autoHideTimeoutRef.current);
            autoHideTimeoutRef.current = null;
        }

        if (hasActive) {
            // Show queue when transfers are active
            setIsVisible(true);
        } else if (allCompleted) {
            // Auto-hide after 5 seconds when all done
            autoHideTimeoutRef.current = setTimeout(() => {
                setIsVisible(false);
            }, 5000);
        }

        return () => {
            if (autoHideTimeoutRef.current) {
                clearTimeout(autoHideTimeoutRef.current);
            }
        };
    }, [items]);

    const addItem = (filename: string, path: string, size: number, type: TransferType): string => {
        const id = `transfer-${++transferId}`;
        setItems(prev => [...prev, {
            id,
            filename,
            path,
            size,
            type,
            status: 'pending',
            startTime: Date.now()
        }]);
        // Show queue when adding items
        setIsVisible(true);
        return id;
    };

    const updateStatus = (id: string, status: TransferStatus, progress?: number, error?: string) => {
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

    const startTransfer = (id: string) => updateStatus(id, 'transferring', 0);
    const setProgress = (id: string, progress: number) => updateStatus(id, 'transferring', progress);
    const completeTransfer = (id: string) => updateStatus(id, 'completed', 100);
    const failTransfer = (id: string, error: string) => updateStatus(id, 'error', undefined, error);

    // Update folder transfer progress (total files and completed count)
    const updateFolderProgress = (id: string, totalFiles: number, completedFiles: number) => {
        setItems(prev => prev.map(item =>
            item.id === id
                ? {
                    ...item,
                    totalFiles,
                    completedFiles,
                    progress: totalFiles > 0 ? Math.round((completedFiles / totalFiles) * 100) : 0
                }
                : item
        ));
    };

    // Mark item as folder
    const markAsFolder = (id: string, totalFiles?: number) => {
        setItems(prev => prev.map(item =>
            item.id === id
                ? { ...item, isFolder: true, totalFiles: totalFiles || 0, completedFiles: 0 }
                : item
        ));
    };

    const clear = () => {
        setItems([]);
        setIsVisible(false);
    };

    const clearCompleted = () => {
        setItems(prev => prev.filter(item => item.status !== 'completed'));
    };

    const stopAll = () => {
        setItems(prev => prev.map(item =>
            item.status === 'pending'
                ? { ...item, status: 'error' as TransferStatus, error: 'Stopped by user', endTime: Date.now() }
                : item
        ));
    };

    const removeItem = (id: string) => {
        setItems(prev => prev.filter(item => item.id !== id));
    };

    const retryItem = (id: string) => {
        setItems(prev => prev.map(item =>
            item.id === id
                ? { ...item, status: 'pending' as TransferStatus, error: undefined, progress: undefined, startTime: Date.now(), endTime: undefined }
                : item
        ));
    };

    // Simple toggle for manual control
    const toggle = () => {
        // Cancel any auto-hide when user manually toggles
        if (autoHideTimeoutRef.current) {
            clearTimeout(autoHideTimeoutRef.current);
            autoHideTimeoutRef.current = null;
        }
        setIsVisible(v => !v);
    };

    // Check if there's any active transfer
    const hasActiveTransfers = items.some(i => i.status === 'transferring' || i.status === 'pending');

    return {
        items,
        isVisible,
        hasActiveTransfers,
        addItem,
        startTransfer,
        setProgress,
        completeTransfer,
        failTransfer,
        updateFolderProgress,
        markAsFolder,
        clear,
        clearCompleted,
        stopAll,
        removeItem,
        retryItem,
        toggle
    };
};

export default TransferQueue;
