import * as React from 'react';
import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { HardDrive, X, ChevronLeft, Loader2, FolderOpen, File, AlertCircle } from 'lucide-react';
import { useTranslation } from '../i18n';
import { formatBytes } from '../utils/formatters';
import { DiskUsageNode } from '../types/aerofile';

// ─── Types ──────────────────────────────────────────────────────────────────

interface DiskUsageTreemapProps {
    isOpen: boolean;
    scanPath: string;
    onClose: () => void;
}

interface TreemapRect {
    x: number;
    y: number;
    w: number;
    h: number;
    node: DiskUsageNode;
}

// ─── Color palette ──────────────────────────────────────────────────────────

const COLOR_HEX = [
    '#3b82f6', '#a855f7', '#14b8a6', '#6366f1',
    '#06b6d4', '#8b5cf6', '#0ea5e9', '#d946ef',
    '#10b981', '#f43f5e',
];

const COLOR_HEX_HOVER = [
    '#60a5fa', '#c084fc', '#2dd4bf', '#818cf8',
    '#22d3ee', '#a78bfa', '#38bdf8', '#e879f9',
    '#34d399', '#fb7185',
];

const FILE_COLOR = '#4b5563';
const FILE_COLOR_HOVER = '#6b7280';

// ─── Squarified Treemap Layout ──────────────────────────────────────────────

function layoutTreemap(
    nodes: DiskUsageNode[],
    x: number,
    y: number,
    w: number,
    h: number
): TreemapRect[] {
    if (nodes.length === 0 || w <= 0 || h <= 0) return [];

    const totalSize = nodes.reduce((sum, n) => sum + n.size, 0);
    if (totalSize === 0) return [];

    const sorted = [...nodes].sort((a, b) => b.size - a.size);

    const results: TreemapRect[] = [];
    let remaining = [...sorted];
    let cx = x, cy = y, cw = w, ch = h;

    while (remaining.length > 0) {
        const isWide = cw >= ch;
        const side = isWide ? ch : cw;
        const remainingTotal = remaining.reduce((s, n) => s + n.size, 0);

        if (remainingTotal === 0) break;

        let row: DiskUsageNode[] = [];
        let rowSize = 0;
        let bestRatio = Infinity;

        for (let i = 0; i < remaining.length; i++) {
            const candidate = [...row, remaining[i]];
            const candidateSize = rowSize + remaining[i].size;
            const stripLen = (candidateSize / remainingTotal) * (isWide ? cw : ch);

            if (stripLen === 0 || side === 0) break;

            let worstRatio = 0;
            for (const item of candidate) {
                const itemLen = (item.size / candidateSize) * side;
                if (itemLen === 0) continue;
                const ratio = Math.max(stripLen / itemLen, itemLen / stripLen);
                worstRatio = Math.max(worstRatio, ratio);
            }

            if (worstRatio <= bestRatio) {
                bestRatio = worstRatio;
                row = candidate;
                rowSize = candidateSize;
            } else {
                break;
            }
        }

        if (row.length === 0) break;

        const stripSize = (rowSize / remainingTotal) * (isWide ? cw : ch);
        let pos = isWide ? cy : cx;

        for (const node of row) {
            const nodeLen = rowSize > 0 ? (node.size / rowSize) * side : 0;
            if (isWide) {
                results.push({ x: cx, y: pos, w: stripSize, h: nodeLen, node });
                pos += nodeLen;
            } else {
                results.push({ x: pos, y: cy, w: nodeLen, h: stripSize, node });
                pos += nodeLen;
            }
        }

        if (isWide) {
            cx += stripSize;
            cw -= stripSize;
        } else {
            cy += stripSize;
            ch -= stripSize;
        }

        remaining = remaining.slice(row.length);
    }

    return results;
}

// ─── Helpers ────────────────────────────────────────────────────────────────

function countItems(node: DiskUsageNode): number {
    if (!node.children || node.children.length === 0) return 1;
    return 1 + node.children.reduce((sum, c) => sum + countItems(c), 0);
}

function getPercentage(size: number, total: number): string {
    if (total === 0) return '0';
    return ((size / total) * 100).toFixed(1);
}

// ─── Component ──────────────────────────────────────────────────────────────

const DiskUsageTreemap: React.FC<DiskUsageTreemapProps> = ({ isOpen, scanPath, onClose }) => {
    const t = useTranslation();

    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [rootNode, setRootNode] = useState<DiskUsageNode | null>(null);
    const [navStack, setNavStack] = useState<string[]>([]);
    const [hoveredNode, setHoveredNode] = useState<DiskUsageNode | null>(null);

    const containerRef = useRef<HTMLDivElement>(null);
    const scanIdRef = useRef(0);
    const [containerSize, setContainerSize] = useState({ width: 0, height: 0 });

    const currentPath = navStack.length > 0 ? navStack[navStack.length - 1] : scanPath;

    // Measure the treemap container
    useEffect(() => {
        if (!isOpen) return;

        const measure = () => {
            if (containerRef.current) {
                const rect = containerRef.current.getBoundingClientRect();
                setContainerSize({ width: rect.width, height: rect.height });
            }
        };

        measure();
        const observer = new ResizeObserver(measure);
        if (containerRef.current) {
            observer.observe(containerRef.current);
        }

        return () => observer.disconnect();
    }, [isOpen, rootNode]);

    // Scan disk usage with stale-result prevention
    const scanDirectory = useCallback(async (dirPath: string) => {
        const currentScanId = ++scanIdRef.current;
        setLoading(true);
        setError(null);
        setHoveredNode(null);
        try {
            const result = await invoke<DiskUsageNode>('scan_disk_usage', {
                path: dirPath,
                maxDepth: 4,
            });
            if (scanIdRef.current !== currentScanId) return; // Stale result
            setRootNode(result);
        } catch (err) {
            if (scanIdRef.current !== currentScanId) return;
            const message = err instanceof Error ? err.message : String(err);
            setError(message || 'Unknown error while scanning disk usage');
            setRootNode(null);
        } finally {
            if (scanIdRef.current === currentScanId) {
                setLoading(false);
            }
        }
    }, []);

    // Scan on open or path change
    useEffect(() => {
        if (isOpen) {
            setNavStack([]);
            scanDirectory(scanPath);
        } else {
            setRootNode(null);
            setError(null);
            setNavStack([]);
            setHoveredNode(null);
        }
    }, [isOpen, scanPath, scanDirectory]);

    // Drill down into a directory
    const drillDown = useCallback((node: DiskUsageNode) => {
        if (!node.is_dir) return;
        setNavStack(prev => [...prev, node.path]);
        scanDirectory(node.path);
    }, [scanDirectory]);

    // Go back to parent
    const goBack = useCallback(() => {
        if (navStack.length === 0) return;
        const newStack = navStack.slice(0, -1);
        setNavStack(newStack);
        const targetPath = newStack.length > 0 ? newStack[newStack.length - 1] : scanPath;
        scanDirectory(targetPath);
    }, [navStack, scanPath, scanDirectory]);

    // Keyboard shortcuts
    useEffect(() => {
        if (!isOpen) return;

        const handler = (e: KeyboardEvent) => {
            if (e.key === 'Escape') {
                e.preventDefault();
                onClose();
            } else if (e.key === 'Backspace') {
                e.preventDefault();
                goBack();
            }
        };

        document.addEventListener('keydown', handler);
        return () => document.removeEventListener('keydown', handler);
    }, [isOpen, onClose, goBack]);

    // Compute treemap rectangles
    const treemapRects = useMemo(() => {
        if (!rootNode || !rootNode.children || rootNode.children.length === 0) return [];
        if (containerSize.width === 0 || containerSize.height === 0) return [];

        return layoutTreemap(
            rootNode.children,
            0, 0,
            containerSize.width,
            containerSize.height
        );
    }, [rootNode, containerSize]);

    // Total items count
    const totalItems = useMemo(() => {
        if (!rootNode) return 0;
        return countItems(rootNode);
    }, [rootNode]);

    // Breadcrumb segments from scanPath to currentPath
    const breadcrumbs = useMemo(() => {
        const segments: { label: string; path: string }[] = [];

        // Always include the initial scan path
        const baseName = scanPath.split('/').filter(Boolean).pop() || scanPath;
        segments.push({ label: baseName, path: scanPath });

        // Add each nav stack entry
        for (const navPath of navStack) {
            const name = navPath.split('/').filter(Boolean).pop() || navPath;
            segments.push({ label: name, path: navPath });
        }

        return segments;
    }, [scanPath, navStack]);

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
            <div
                className="bg-white dark:bg-gray-800 rounded-lg shadow-xl w-[900px] max-h-[85vh] flex flex-col border border-gray-700 animate-scale-in"
                role="dialog"
                aria-label={t('diskUsage.title')}
                aria-modal="true"
            >
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-700">
                    <div className="flex items-center gap-2 min-w-0">
                        <HardDrive size={18} className="text-blue-400 shrink-0" />
                        <span className="font-medium text-sm truncate">
                            {t('diskUsage.title')}
                        </span>
                        <span className="text-xs text-gray-400 truncate ml-2 hidden sm:inline">
                            {currentPath}
                        </span>
                    </div>
                    <button
                        onClick={onClose}
                        className="p-1 hover:bg-gray-700 rounded transition-colors shrink-0"
                        title={t('common.close')}
                    >
                        <X size={18} />
                    </button>
                </div>

                {/* Navigation bar */}
                <div className="flex items-center gap-2 px-4 py-2 border-b border-gray-700/50">
                    <button
                        onClick={goBack}
                        disabled={navStack.length === 0}
                        className="flex items-center gap-1 text-xs text-gray-400 hover:text-white disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
                    >
                        <ChevronLeft size={14} />
                        <span>{t('diskUsage.back')}</span>
                    </button>
                    <div className="flex items-center gap-1 text-xs text-gray-400 overflow-hidden">
                        {breadcrumbs.map((seg, i) => (
                            <React.Fragment key={seg.path + i}>
                                {i > 0 && <span className="text-gray-600">/</span>}
                                <button
                                    onClick={() => {
                                        if (i === 0) {
                                            setNavStack([]);
                                            scanDirectory(scanPath);
                                        } else {
                                            const newStack = navStack.slice(0, i);
                                            setNavStack(newStack);
                                            scanDirectory(newStack[newStack.length - 1]);
                                        }
                                    }}
                                    className={`hover:text-white transition-colors truncate max-w-[120px] ${
                                        i === breadcrumbs.length - 1 ? 'text-gray-200 font-medium' : ''
                                    }`}
                                    title={seg.path}
                                >
                                    {seg.label}
                                </button>
                            </React.Fragment>
                        ))}
                    </div>
                </div>

                {/* Summary bar */}
                {rootNode && !loading && (
                    <div className="px-4 py-2 border-b border-gray-700/50 text-xs text-gray-400">
                        {t('diskUsage.total')}: {formatBytes(rootNode.size)}{' '}
                        {t('diskUsage.across')} {totalItems.toLocaleString()} {t('diskUsage.items')}
                    </div>
                )}

                {/* Main content area */}
                <div className="flex-1 min-h-0 p-4">
                    {loading && (
                        <div className="flex flex-col items-center justify-center h-[400px] gap-3">
                            <Loader2 size={32} className="text-blue-400 animate-spin" />
                            <span className="text-sm text-gray-400">
                                {t('diskUsage.scanning')}
                            </span>
                        </div>
                    )}

                    {error && !loading && (
                        <div className="flex flex-col items-center justify-center h-[400px] gap-3">
                            <AlertCircle size={32} className="text-red-400" />
                            <span className="text-sm text-red-400">{error}</span>
                            <button
                                onClick={() => scanDirectory(currentPath)}
                                className="text-xs text-blue-400 hover:text-blue-300 transition-colors"
                            >
                                {t('diskUsage.retry')}
                            </button>
                        </div>
                    )}

                    {!loading && !error && rootNode && (
                        <>
                            {rootNode.children && rootNode.children.length > 0 ? (
                                <div
                                    ref={containerRef}
                                    className="relative w-full rounded-lg overflow-hidden"
                                    style={{ height: '400px' }}
                                    onMouseLeave={() => setHoveredNode(null)}
                                >
                                    {treemapRects.map((rect, index) => {
                                        const isDir = rect.node.is_dir;
                                        const colorIndex = index % COLOR_HEX.length;
                                        const bgColor = isDir ? COLOR_HEX[colorIndex] : FILE_COLOR;
                                        const bgColorHover = isDir ? COLOR_HEX_HOVER[colorIndex] : FILE_COLOR_HOVER;
                                        const isHovered = hoveredNode?.path === rect.node.path;

                                        const showLabel = rect.w > 50 && rect.h > 30;
                                        const showSize = rect.w > 60 && rect.h > 48;
                                        const showIcon = rect.w > 36 && rect.h > 36;
                                        const isLargeCell = rect.w > 120 && rect.h > 60;

                                        const percentage = rootNode.size > 0
                                            ? getPercentage(rect.node.size, rootNode.size)
                                            : '0';

                                        return (
                                            <div
                                                key={rect.node.path}
                                                className={`absolute transition-all duration-150 ease-out ${
                                                    isDir ? 'cursor-pointer' : 'cursor-default'
                                                }`}
                                                style={{
                                                    left: `${rect.x + 1}px`,
                                                    top: `${rect.y + 1}px`,
                                                    width: `${Math.max(rect.w - 2, 0)}px`,
                                                    height: `${Math.max(rect.h - 2, 0)}px`,
                                                    backgroundColor: isHovered ? bgColorHover : bgColor,
                                                    borderRadius: '4px',
                                                    opacity: isHovered ? 1 : 0.85,
                                                    transform: isHovered ? 'scale(1.01)' : 'scale(1)',
                                                    zIndex: isHovered ? 10 : 1,
                                                    boxShadow: isHovered
                                                        ? '0 4px 12px rgba(0,0,0,0.4)'
                                                        : '0 1px 2px rgba(0,0,0,0.2)',
                                                }}
                                                onClick={() => {
                                                    if (isDir) drillDown(rect.node);
                                                }}
                                                onMouseEnter={() => setHoveredNode(rect.node)}
                                                title={`${rect.node.name} - ${formatBytes(rect.node.size)} (${percentage}%)`}
                                            >
                                                <div className="w-full h-full flex flex-col items-start justify-center px-2 py-1 overflow-hidden">
                                                    {showIcon && !showLabel && (
                                                        <div className="text-white/70">
                                                            {isDir
                                                                ? <FolderOpen size={14} />
                                                                : <File size={14} />
                                                            }
                                                        </div>
                                                    )}
                                                    {showLabel && (
                                                        <div className="flex items-center gap-1 min-w-0 w-full">
                                                            {isLargeCell && (
                                                                <span className="shrink-0 text-white/80">
                                                                    {isDir
                                                                        ? <FolderOpen size={12} />
                                                                        : <File size={12} />
                                                                    }
                                                                </span>
                                                            )}
                                                            <span
                                                                className="text-white font-medium truncate"
                                                                style={{
                                                                    fontSize: isLargeCell ? '12px' : '10px',
                                                                    lineHeight: '1.3',
                                                                    textShadow: '0 1px 2px rgba(0,0,0,0.5)',
                                                                }}
                                                            >
                                                                {rect.node.name}{isDir ? '/' : ''}
                                                            </span>
                                                        </div>
                                                    )}
                                                    {showSize && (
                                                        <span
                                                            className="text-white/80 truncate w-full"
                                                            style={{
                                                                fontSize: isLargeCell ? '11px' : '9px',
                                                                lineHeight: '1.3',
                                                                textShadow: '0 1px 2px rgba(0,0,0,0.5)',
                                                            }}
                                                        >
                                                            {formatBytes(rect.node.size)}
                                                        </span>
                                                    )}
                                                    {isLargeCell && (
                                                        <span
                                                            className="text-white/60 truncate w-full"
                                                            style={{
                                                                fontSize: '9px',
                                                                lineHeight: '1.3',
                                                                textShadow: '0 1px 2px rgba(0,0,0,0.3)',
                                                            }}
                                                        >
                                                            {percentage}%
                                                        </span>
                                                    )}
                                                </div>
                                            </div>
                                        );
                                    })}
                                </div>
                            ) : (
                                <div className="flex flex-col items-center justify-center h-[400px] gap-3 text-gray-500">
                                    <FolderOpen size={32} />
                                    <span className="text-sm">
                                        {t('diskUsage.empty')}
                                    </span>
                                </div>
                            )}
                        </>
                    )}
                </div>

                {/* Hover info bar */}
                <div className="px-4 py-2 border-t border-gray-700 text-xs text-gray-400 h-[36px] flex items-center">
                    {hoveredNode ? (
                        <div className="flex items-center gap-2 min-w-0">
                            {hoveredNode.is_dir
                                ? <FolderOpen size={12} className="text-blue-400 shrink-0" />
                                : <File size={12} className="text-gray-500 shrink-0" />
                            }
                            <span className="truncate">
                                <span className="text-gray-200 font-medium">
                                    {hoveredNode.name}{hoveredNode.is_dir ? '/' : ''}
                                </span>
                                <span className="mx-2 text-gray-600">&mdash;</span>
                                <span>{formatBytes(hoveredNode.size)}</span>
                                {rootNode && rootNode.size > 0 && (
                                    <>
                                        <span className="mx-1 text-gray-600">
                                            ({getPercentage(hoveredNode.size, rootNode.size)}%)
                                        </span>
                                    </>
                                )}
                                {hoveredNode.is_dir && hoveredNode.children && (
                                    <>
                                        <span className="mx-2 text-gray-600">&mdash;</span>
                                        <span>
                                            {countItems(hoveredNode).toLocaleString()} {t('diskUsage.items')}
                                        </span>
                                    </>
                                )}
                            </span>
                        </div>
                    ) : (
                        <span className="text-gray-600 italic">
                            {t('diskUsage.hoverHint')}
                        </span>
                    )}
                </div>
            </div>
        </div>
    );
};

export default DiskUsageTreemap;
