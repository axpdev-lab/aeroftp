/**
 * Activity Log Panel Component
 * FileZilla-style horizontal panel displaying all app operations
 * 
 * Features:
 * - Terminal-like auto-scrolling log view
 * - Color-coded entries by status
 * - Clear and filter functionality
 * - Collapsible design
 * - Resizable height with drag handle
 * - Typewriter effect for live writing animation
 * - Theme follows app theme (light/dark) or can use cyber mode
 */

import * as React from 'react';
import { useEffect, useRef, useCallback, useState, useMemo } from 'react';
import {
    X, Trash2, ChevronDown, GripHorizontal, Terminal, Zap, Cloud,
    Plug, Unplug, Upload, Download, FolderPlus, FolderOpen, Pencil,
    AlertCircle, Info, CheckCircle, Copy, Move,
    type LucideIcon
} from 'lucide-react';
import { useActivityLog, LogEntry, OperationType, getOperationIcon, formatTimestamp } from '../hooks/useActivityLog';
import { useTranslation } from '../i18n';

// ============================================================================
// Theme Types
// ============================================================================

type LogTheme = 'light' | 'dark' | 'cyber';

// ============================================================================
// Icon Mapping - Lucide icons for operations
// ============================================================================

const OPERATION_ICONS: Record<string, LucideIcon> = {
    plug: Plug,
    unplug: Unplug,
    upload: Upload,
    download: Download,
    'trash-2': Trash2,
    pencil: Pencil,
    move: Move,
    'folder-plus': FolderPlus,
    'folder-open': FolderOpen,
    'alert-circle': AlertCircle,
    info: Info,
    'check-circle': CheckCircle,
};

/**
 * Render a Lucide icon component from icon name
 */
const OperationIcon: React.FC<{ iconName: string; className?: string }> = ({ iconName, className }) => {
    const IconComponent = OPERATION_ICONS[iconName];
    if (!IconComponent) {
        return <span className={className}>•</span>;
    }
    return <IconComponent size={12} className={className} />;
};

// ============================================================================
// Theme Configurations
// ============================================================================

const THEMES = {
    light: {
        // Light theme - clean and professional
        panel: 'bg-gray-50 border-t border-gray-200',
        header: 'bg-white border-b border-gray-200',
        headerText: 'text-gray-700',
        headerIcon: 'text-blue-600',
        resize: 'hover:bg-blue-100',
        resizeActive: 'bg-blue-200',
        scrollbar: 'scrollbar-thumb-gray-300 scrollbar-track-gray-100',
        emptyText: 'text-gray-400',
        emptyIcon: 'text-gray-300',
        button: 'text-gray-500 hover:text-gray-700 hover:bg-gray-100',
        buttonDanger: 'text-gray-500 hover:text-red-600 hover:bg-red-50',
        select: 'bg-white text-gray-700 border-gray-300 focus:ring-blue-500 focus:border-blue-500',
        badge: 'bg-blue-100 text-blue-700 border-blue-300',
        badgeIcon: 'text-blue-600',
        count: 'text-gray-400',
        jumpButton: 'bg-blue-100 text-blue-700 border-blue-300 hover:bg-blue-200',
        // Row colors
        row: {
            default: 'border-transparent hover:bg-gray-100 hover:border-gray-300',
            running: 'border-blue-400 bg-blue-50',
            success: 'border-green-400/50 hover:border-green-500',
            error: 'border-red-400 bg-red-50',
        },
        timestamp: 'text-gray-400',
        status: {
            pending: 'text-gray-500',
            running: 'text-blue-600',
            success: 'text-green-600',
            error: 'text-red-600',
        },
        operation: {
            CONNECT: 'text-blue-600',
            DISCONNECT: 'text-orange-500',
            UPLOAD: 'text-green-600',
            DOWNLOAD: 'text-sky-600',
            DELETE: 'text-red-600',
            RENAME: 'text-amber-600',
            MOVE: 'text-teal-600',
            MKDIR: 'text-purple-600',
            NAVIGATE: 'text-sky-600',
            UPDATE: 'text-indigo-600',
            ERROR: 'text-red-600',
            INFO: 'text-gray-600',
            SUCCESS: 'text-green-600',
        },
        operationGlow: {
            CONNECT: '', DISCONNECT: '', UPLOAD: '', DOWNLOAD: '',
            DELETE: '', RENAME: '', MOVE: '', MKDIR: '', NAVIGATE: '', UPDATE: '',
            ERROR: '', INFO: '', SUCCESS: '',
        },
        liveIndicator: 'bg-blue-500',
        liveText: 'text-blue-600',
        cursor: 'bg-blue-500',
    },
    dark: {
        // Tokio Night / Antigravity inspired (renamed from professional)
        panel: 'bg-[#1a1b26] border-t border-[#292e42]',
        header: 'bg-[#16161e] border-b border-[#292e42]',
        headerText: 'text-[#a9b1d6]',
        headerIcon: 'text-[#7aa2f7]',
        resize: 'hover:bg-[#7aa2f7]/30',
        resizeActive: 'bg-[#7aa2f7]/50',
        scrollbar: 'scrollbar-thumb-[#414868] scrollbar-track-[#1a1b26]',
        emptyText: 'text-[#565f89]',
        emptyIcon: 'text-[#414868]',
        button: 'text-[#565f89] hover:text-[#c0caf5] hover:bg-[#292e42]',
        buttonDanger: 'text-[#565f89] hover:text-[#f7768e] hover:bg-[#f7768e]/10',
        select: 'bg-[#16161e] text-[#a9b1d6] border-[#292e42] focus:ring-[#7aa2f7] focus:border-[#7aa2f7]',
        badge: 'bg-[#7aa2f7]/20 text-[#7aa2f7] border-[#7aa2f7]/40',
        badgeIcon: 'text-[#7aa2f7]',
        count: 'text-[#565f89]',
        jumpButton: 'bg-[#7aa2f7]/20 text-[#7aa2f7] border-[#7aa2f7]/40 hover:bg-[#7aa2f7]/30',
        // Row colors
        row: {
            default: 'border-transparent hover:bg-[#1f2335] hover:border-[#414868]/50',
            running: 'border-[#7aa2f7]/50 bg-[#1f2335]',
            success: 'border-[#9ece6a]/30 hover:border-[#9ece6a]/50',
            error: 'border-[#f7768e]/50 bg-[#f7768e]/5',
        },
        timestamp: 'text-[#565f89]',
        status: {
            pending: 'text-[#565f89]',
            running: 'text-[#7aa2f7]',
            success: 'text-[#9ece6a]',
            error: 'text-[#f7768e]',
        },
        operation: {
            CONNECT: 'text-[#7aa2f7]',
            DISCONNECT: 'text-[#ff9e64]',
            UPLOAD: 'text-[#9ece6a]',
            DOWNLOAD: 'text-[#7dcfff]',
            DELETE: 'text-[#f7768e]',
            RENAME: 'text-[#e0af68]',
            MOVE: 'text-[#73daca]',
            MKDIR: 'text-[#bb9af7]',
            NAVIGATE: 'text-[#7dcfff]',
            UPDATE: 'text-[#9d7cd8]',
            ERROR: 'text-[#f7768e]',
            INFO: 'text-[#a9b1d6]',
            SUCCESS: 'text-[#9ece6a]',
        },
        // No glow effect for dark theme
        operationGlow: {
            CONNECT: '', DISCONNECT: '', UPLOAD: '', DOWNLOAD: '',
            DELETE: '', RENAME: '', MOVE: '', MKDIR: '', NAVIGATE: '', UPDATE: '',
            ERROR: '', INFO: '', SUCCESS: '',
        },
        liveIndicator: 'bg-[#7aa2f7]',
        liveText: 'text-[#7aa2f7]',
        cursor: 'bg-[#7aa2f7]',
    },
    cyber: {
        // Neon cyber theme
        panel: 'bg-gradient-to-b from-gray-950 via-gray-900 to-gray-950 border-t border-cyan-900/50 shadow-[0_-4px_20px_rgba(0,0,0,0.5)]',
        header: 'bg-gradient-to-r from-gray-900 via-gray-800/80 to-gray-900 border-b border-cyan-900/30',
        headerText: 'text-cyan-300',
        headerIcon: 'text-cyan-500',
        resize: 'hover:bg-gradient-to-r hover:from-transparent hover:via-cyan-500/50 hover:to-transparent',
        resizeActive: 'bg-gradient-to-r from-transparent via-cyan-400 to-transparent shadow-[0_0_10px_rgba(34,211,238,0.5)]',
        scrollbar: 'scrollbar-thumb-cyan-800/50 scrollbar-track-gray-900/50 hover:scrollbar-thumb-cyan-700/70',
        emptyText: 'text-gray-500',
        emptyIcon: 'text-gray-700',
        button: 'text-gray-500 hover:text-cyan-300 hover:bg-cyan-500/10',
        buttonDanger: 'text-gray-500 hover:text-rose-400 hover:bg-rose-500/10 hover:drop-shadow-[0_0_4px_rgba(244,63,94,0.5)]',
        select: 'bg-gray-900 text-cyan-300 border-cyan-800/50 focus:ring-cyan-500 focus:border-cyan-500 hover:border-cyan-600',
        badge: 'bg-cyan-500/20 text-cyan-300 border-cyan-500/40 shadow-[0_0_8px_rgba(34,211,238,0.3)]',
        badgeIcon: 'text-cyan-400',
        count: 'text-gray-500',
        jumpButton: 'bg-cyan-500/20 text-cyan-300 border-cyan-500/40 shadow-[0_0_10px_rgba(34,211,238,0.3)] hover:bg-cyan-500/30 hover:shadow-[0_0_15px_rgba(34,211,238,0.4)]',
        // Row colors
        row: {
            default: 'border-transparent hover:bg-cyan-950/30 hover:border-cyan-500/30',
            running: 'border-cyan-400/50 bg-cyan-950/20',
            success: 'border-emerald-500/30 hover:border-emerald-400/50',
            error: 'border-rose-500/50 bg-rose-950/10',
        },
        timestamp: 'text-cyan-600/60',
        status: {
            pending: 'text-gray-400',
            running: 'text-cyan-400',
            success: 'text-emerald-400',
            error: 'text-rose-500',
        },
        operation: {
            CONNECT: 'text-cyan-400',
            DISCONNECT: 'text-orange-400',
            UPLOAD: 'text-emerald-400',
            DOWNLOAD: 'text-blue-400',
            DELETE: 'text-rose-500',
            RENAME: 'text-amber-400',
            MOVE: 'text-teal-400',
            MKDIR: 'text-violet-400',
            NAVIGATE: 'text-sky-400',
            UPDATE: 'text-indigo-400',
            ERROR: 'text-rose-500',
            INFO: 'text-gray-300',
            SUCCESS: 'text-emerald-400',
        },
        operationGlow: {
            CONNECT: 'drop-shadow-[0_0_6px_rgba(34,211,238,0.8)]',
            DISCONNECT: 'drop-shadow-[0_0_6px_rgba(251,146,60,0.8)]',
            UPLOAD: 'drop-shadow-[0_0_6px_rgba(52,211,153,0.8)]',
            DOWNLOAD: 'drop-shadow-[0_0_6px_rgba(96,165,250,0.8)]',
            DELETE: 'drop-shadow-[0_0_6px_rgba(244,63,94,0.8)]',
            RENAME: 'drop-shadow-[0_0_6px_rgba(251,191,36,0.8)]',
            MOVE: 'drop-shadow-[0_0_6px_rgba(45,212,191,0.8)]',
            MKDIR: 'drop-shadow-[0_0_6px_rgba(167,139,250,0.8)]',
            NAVIGATE: 'drop-shadow-[0_0_6px_rgba(56,189,248,0.8)]',
            UPDATE: 'drop-shadow-[0_0_6px_rgba(129,140,248,0.8)]',
            ERROR: 'drop-shadow-[0_0_6px_rgba(244,63,94,1)]',
            INFO: '',
            SUCCESS: 'drop-shadow-[0_0_6px_rgba(52,211,153,0.8)]',
        },
        liveIndicator: 'bg-cyan-400',
        liveText: 'text-cyan-400',
        cursor: 'bg-cyan-400',
    },
};

// ============================================================================
// Typewriter Hook - Fast & Fluid
// ============================================================================

function useTypewriter(text: string, speed: number = 8, enabled: boolean = true): { displayText: string; isTyping: boolean } {
    const [displayText, setDisplayText] = useState('');
    const [isTyping, setIsTyping] = useState(false);
    const prevTextRef = useRef('');

    useEffect(() => {
        if (!enabled || !text) {
            setDisplayText(text);
            setIsTyping(false);
            return;
        }

        // If text is the same, don't animate
        if (text === prevTextRef.current) {
            return;
        }
        prevTextRef.current = text;

        // Start fresh for new text
        setDisplayText('');
        setIsTyping(true);

        let index = 0;
        const timer = setInterval(() => {
            if (index < text.length) {
                // Type 2-3 characters at a time for faster fluid effect
                const charsToAdd = Math.min(2 + Math.floor(Math.random() * 2), text.length - index);
                setDisplayText(text.slice(0, index + charsToAdd));
                index += charsToAdd;
            } else {
                setIsTyping(false);
                clearInterval(timer);
            }
        }, speed);

        return () => clearInterval(timer);
    }, [text, speed, enabled]);

    return { displayText: displayText || text, isTyping };
}

// ============================================================================
// Types
// ============================================================================

interface ActivityLogPanelProps {
    /** Whether the panel is visible */
    isVisible: boolean;
    /** Callback to toggle visibility */
    onToggle: () => void;
    /** Initial panel height in pixels */
    initialHeight?: number;
    /** Minimum height */
    minHeight?: number;
    /** Maximum height */
    maxHeight?: number;
    /** Theme: 'light', 'dark' (default), or 'cyber' */
    theme?: LogTheme;
}

// ============================================================================
// Sub-components
// ============================================================================

interface LogEntryRowProps {
    entry: LogEntry;
    themeConfig: typeof THEMES.dark;
    isLatest: boolean;
}

const LogEntryRow: React.FC<LogEntryRowProps> = React.memo(({ entry, themeConfig, isLatest }) => {
    const icon = getOperationIcon(entry.operation);
    const statusClass = themeConfig.status[entry.status] || themeConfig.status.pending;
    const opClass = themeConfig.operation[entry.operation] || themeConfig.operation.INFO;
    const opGlow = themeConfig.operationGlow?.[entry.operation] || '';

    // Typewriter effect only for the latest running entry
    const shouldAnimate = isLatest && entry.status === 'running';
    const { displayText, isTyping } = useTypewriter(entry.message, 8, shouldAnimate);

    const rowClass = entry.status === 'error' ? themeConfig.row.error :
        entry.status === 'running' ? themeConfig.row.running :
            entry.status === 'success' ? themeConfig.row.success :
                themeConfig.row.default;

    const copyEntry = useCallback(() => {
        const text = `[${formatTimestamp(entry.timestamp)}] ${entry.operation} - ${entry.message}${entry.details ? ` (${entry.details})` : ''}`;
        navigator.clipboard.writeText(text);
    }, [entry]);

    return (
        <div className={`group flex items-start gap-2 py-0.5 px-3 font-mono text-xs border-l-2 transition-colors ${rowClass}`}>
            {/* Timestamp */}
            <span className={`shrink-0 w-16 tabular-nums ${themeConfig.timestamp}`}>
                {formatTimestamp(entry.timestamp)}
            </span>

            {/* Icon - Lucide component, no glow */}
            <span className={`shrink-0 w-5 flex items-center justify-center ${opClass}`} aria-hidden="true">
                <OperationIcon iconName={icon} className={opClass} />
            </span>

            {/* Operation type badge - WITH glow for cyber theme */}
            <span className={`shrink-0 w-20 text-[10px] font-semibold uppercase tracking-wider ${opClass} ${opGlow}`}>
                {entry.operation}
            </span>

            {/* Message with typewriter effect + inline copy button */}
            <span className={`flex-1 break-all ${statusClass}`}>
                {shouldAnimate ? displayText : entry.message}
                {/* Blinking cursor during typing */}
                {isTyping && (
                    <span className={`inline-block w-[2px] h-3.5 ml-0.5 animate-[blink_0.5s_infinite] align-middle ${themeConfig.cursor}`}
                        style={{ animationTimingFunction: 'steps(1)' }} />
                )}
                {entry.details && (
                    <span className="text-[#565f89] ml-2 italic">({entry.details})</span>
                )}
                <button
                    onClick={copyEntry}
                    className={`inline-flex ml-2 opacity-0 group-hover:opacity-100 p-0.5 rounded transition-all align-middle ${themeConfig.button}`}
                    title="Copy"
                >
                    <Copy size={10} />
                </button>
            </span>

            {/* Status indicator */}
            {entry.status === 'running' && (
                <span className="shrink-0 flex items-center gap-1 self-center">
                    <span className={`w-1.5 h-1.5 rounded-full animate-ping ${themeConfig.liveIndicator}`} />
                    <span className={`text-[9px] uppercase tracking-widest ${themeConfig.liveText}`}>LIVE</span>
                </span>
            )}
            {/* Show SYNCED for completed sync operations */}
            {entry.status === 'success' && entry.message.toLowerCase().includes('sync') && (
                <span className="shrink-0 flex items-center gap-1 self-center">
                    <span className="w-1.5 h-1.5 rounded-full bg-[#9ece6a]" />
                    <span className="text-[9px] uppercase tracking-widest text-[#9ece6a]">SYNCED</span>
                </span>
            )}
        </div>
    );
});

LogEntryRow.displayName = 'LogEntryRow';

// ============================================================================
// Main Component
// ============================================================================

export const ActivityLogPanel: React.FC<ActivityLogPanelProps> = ({
    isVisible,
    onToggle,
    initialHeight = 150,
    minHeight = 80,
    maxHeight = 400,
    theme: themeProp = 'dark',
}) => {
    const t = useTranslation();
    const { entries, clear, runningCount } = useActivityLog();
    const scrollRef = useRef<HTMLDivElement>(null);
    const panelRef = useRef<HTMLDivElement>(null);
    const [autoScroll, setAutoScroll] = useState(true);
    const [filterType, setFilterType] = useState<OperationType | 'ALL'>('ALL');
    const [showCloudSync, setShowCloudSync] = useState(true);  // Toggle to show/hide AeroCloud sync messages
    const [height, setHeight] = useState(initialHeight);
    const [isResizing, setIsResizing] = useState(false);

    // Use theme from props (controlled by app-level theme)
    const themeConfig = THEMES[themeProp];

    // Auto-scroll to bottom when new entries arrive
    useEffect(() => {
        if (autoScroll && scrollRef.current) {
            scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
        }
    }, [entries, autoScroll]);

    // Handle scroll to detect if user scrolled up manually
    const handleScroll = useCallback(() => {
        if (!scrollRef.current) return;

        const { scrollTop, scrollHeight, clientHeight } = scrollRef.current;
        const isAtBottom = scrollHeight - scrollTop - clientHeight < 20;
        setAutoScroll(isAtBottom);
    }, []);

    // Resize handlers
    const handleResizeStart = useCallback((e: React.MouseEvent) => {
        e.preventDefault();
        setIsResizing(true);

        const startY = e.clientY;
        const startHeight = height;

        const handleMouseMove = (moveEvent: MouseEvent) => {
            const deltaY = startY - moveEvent.clientY;
            const newHeight = Math.min(maxHeight, Math.max(minHeight, startHeight + deltaY));
            setHeight(newHeight);
        };

        const handleMouseUp = () => {
            setIsResizing(false);
            document.removeEventListener('mousemove', handleMouseMove);
            document.removeEventListener('mouseup', handleMouseUp);
        };

        document.addEventListener('mousemove', handleMouseMove);
        document.addEventListener('mouseup', handleMouseUp);
    }, [height, minHeight, maxHeight]);

    // Filter entries
    const filteredEntries = useMemo(() => {
        let result = entries;

        // Filter by operation type
        if (filterType !== 'ALL') {
            result = result.filter(e => e.operation === filterType);
        }

        // Filter out AeroCloud sync messages if disabled
        if (!showCloudSync) {
            result = result.filter(e => !e.message.toLowerCase().includes('aerocloud'));
        }

        return result;
    }, [entries, filterType, showCloudSync]);

    // Find the latest entry for typewriter effect
    const latestEntryId = entries.length > 0 ? entries[entries.length - 1].id : null;

    // Don't render if not visible
    if (!isVisible) return null;

    return (
        <div
            ref={panelRef}
            className={`${themeConfig.panel} flex flex-col shrink-0 relative`}
            style={{ height }}
        >
            {/* Cyber grid background (only for cyber theme) */}
            {themeProp === 'cyber' && (
                <>
                    {/* Scanlines effect - more visible */}
                    <div
                        className="absolute inset-0 pointer-events-none z-0"
                        style={{
                            background: 'repeating-linear-gradient(0deg, transparent, transparent 1px, rgba(0,255,255,0.03) 1px, rgba(0,0,0,0.25) 2px)',
                            backgroundSize: '100% 3px',
                        }}
                    />
                    {/* CRT flicker */}
                    <div
                        className="absolute inset-0 pointer-events-none z-0 animate-pulse opacity-[0.02]"
                        style={{ background: 'radial-gradient(ellipse at center, rgba(34,211,238,0.1) 0%, transparent 70%)' }}
                    />
                    {/* Grid pattern */}
                    <div className="absolute inset-0 opacity-[0.03] pointer-events-none z-0"
                        style={{
                            backgroundImage: 'linear-gradient(0deg, transparent 24%, rgba(34,211,238,0.3) 25%, rgba(34,211,238,0.3) 26%, transparent 27%, transparent 74%, rgba(34,211,238,0.3) 75%, rgba(34,211,238,0.3) 76%, transparent 77%), linear-gradient(90deg, transparent 24%, rgba(34,211,238,0.3) 25%, rgba(34,211,238,0.3) 26%, transparent 27%, transparent 74%, rgba(34,211,238,0.3) 75%, rgba(34,211,238,0.3) 76%, transparent 77%)',
                            backgroundSize: '50px 50px'
                        }}
                    />
                </>
            )}

            {/* Resize handle at top */}
            <div
                onMouseDown={handleResizeStart}
                className={`absolute top-0 left-0 right-0 h-1.5 cursor-ns-resize flex items-center justify-center z-10 group transition-all
                    ${isResizing ? themeConfig.resizeActive : themeConfig.resize}`}
            >
                <GripHorizontal
                    size={12}
                    className={`text-gray-600 opacity-0 group-hover:opacity-100 transition-opacity ${isResizing ? 'opacity-100' : ''}`}
                />
            </div>

            {/* Header */}
            <div className={`flex items-center justify-between px-3 py-1.5 ${themeConfig.header} mt-1.5`}>
                <div className="flex items-center gap-3">
                    {/* Title with terminal icon */}
                    <button
                        onClick={onToggle}
                        className={`flex items-center gap-2 text-xs font-medium ${themeConfig.headerText} hover:opacity-80 transition-colors group`}
                    >
                        <Terminal size={14} className={themeConfig.headerIcon} />
                        <ChevronDown size={14} className="opacity-60" />
                        <span className="tracking-wide text-[11px]">{t('transfer.activityLog') || 'Activity Log'}</span>
                    </button>

                    {/* Running count badge */}
                    {runningCount > 0 && (
                        <span className={`px-2 py-0.5 text-[10px] font-bold rounded-sm border animate-pulse flex items-center gap-1.5 ${themeConfig.badge}`}>
                            <Zap size={10} className={themeConfig.badgeIcon} />
                            {runningCount} ACTIVE
                        </span>
                    )}

                    {/* Entry count */}
                    <span className={`text-[10px] font-mono ${themeConfig.count}`}>
                        [{filteredEntries.length}]
                    </span>
                </div>

                {/* Actions */}
                <div className="flex items-center gap-2">
                    {/* AeroCloud sync filter toggle */}
                    <button
                        onClick={() => setShowCloudSync(!showCloudSync)}
                        className={`p-1.5 rounded transition-all ${showCloudSync ? themeConfig.button : 'opacity-40 ' + themeConfig.button}`}
                        title={showCloudSync ? 'Hide AeroCloud sync messages' : 'Show AeroCloud sync messages'}
                    >
                        <Cloud size={12} className={showCloudSync ? 'text-[#7dcfff]' : ''} />
                    </button>

                    {/* Filter dropdown */}
                    <select
                        value={filterType}
                        onChange={(e) => setFilterType(e.target.value as OperationType | 'ALL')}
                        className={`text-[10px] border rounded-sm px-2 py-0.5 focus:outline-none focus:ring-1 uppercase tracking-wider font-mono cursor-pointer transition-colors ${themeConfig.select}`}
                    >
                        <option value="ALL">All</option>
                        <option value="CONNECT">Connect</option>
                        <option value="DISCONNECT">Disconnect</option>
                        <option value="UPLOAD">Upload</option>
                        <option value="DOWNLOAD">Download</option>
                        <option value="DELETE">Delete</option>
                        <option value="NAVIGATE">Navigate</option>
                        <option value="ERROR">Errors</option>
                    </select>

                    {/* Copy All button */}
                    <button
                        onClick={() => {
                            const logText = filteredEntries.map(e =>
                                `[${formatTimestamp(e.timestamp)}] ${e.operation} - ${e.message}`
                            ).join('\n');
                            navigator.clipboard.writeText(logText);
                        }}
                        className={`p-1.5 rounded transition-all ${themeConfig.button}`}
                        title="Copy all logs"
                    >
                        <Copy size={12} />
                    </button>

                    {/* Clear button */}
                    <button
                        onClick={clear}
                        className={`p-1.5 rounded transition-all ${themeConfig.buttonDanger}`}
                        title="Clear log"
                    >
                        <Trash2 size={12} />
                    </button>

                    {/* Close button */}
                    <button
                        onClick={onToggle}
                        className={`p-1.5 rounded transition-all ${themeConfig.button}`}
                        title="Close"
                    >
                        <X size={14} />
                    </button>
                </div>
            </div>

            {/* Log entries */}
            <div
                ref={scrollRef}
                onScroll={handleScroll}
                className={`flex-1 overflow-y-auto overflow-x-hidden scrollbar-thin ${themeConfig.scrollbar}`}
            >
                {filteredEntries.length === 0 ? (
                    <div className={`flex flex-col items-center justify-center h-full text-xs gap-2 ${themeConfig.emptyText}`}>
                        <Terminal size={24} className={themeConfig.emptyIcon} />
                        <span className="uppercase tracking-widest text-[10px]">{t('transfer.noActivity') || 'Awaiting commands...'}</span>
                    </div>
                ) : (
                    filteredEntries.map(entry => (
                        <LogEntryRow
                            key={entry.id}
                            entry={entry}
                            themeConfig={themeConfig}
                            isLatest={entry.id === latestEntryId}
                        />
                    ))
                )}
            </div>

            {/* Auto-scroll indicator */}
            {!autoScroll && entries.length > 0 && (
                <button
                    onClick={() => {
                        setAutoScroll(true);
                        if (scrollRef.current) {
                            scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
                        }
                    }}
                    className={`absolute bottom-3 right-4 px-3 py-1.5 text-[10px] rounded-sm border transition-all font-mono uppercase tracking-wider flex items-center gap-1.5 ${themeConfig.jumpButton}`}
                >
                    <ChevronDown size={12} />
                    LATEST
                </button>
            )}

            {/* CSS for blinking cursor animation */}
            <style>{`
                @keyframes blink {
                    0%, 49% { opacity: 1; }
                    50%, 100% { opacity: 0; }
                }
            `}</style>
        </div>
    );
};

export default ActivityLogPanel;
