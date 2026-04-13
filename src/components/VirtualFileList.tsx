// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

/**
 * VirtualFileList — virtualized file list using react-window v2
 *
 * Drop-in replacement for the table-based file list in both local and remote panels.
 * Uses List for O(visible) DOM nodes instead of O(total) — critical for
 * directories with 10,000+ files.
 *
 * Layout: div-based flex rows matching the original table column alignment.
 */

import React, { useRef, useState, useEffect, CSSProperties, ReactElement } from 'react';
import { List, RowComponentProps } from 'react-window';

// ============================================================================
// Types
// ============================================================================

export interface VirtualFileItem {
    name: string;
    path: string;
    is_dir: boolean;
    size: number | null;
    modified: string | null;
    permissions?: string | null;
}

export interface VirtualFileListProps {
    /** Sorted files to display (without the "go up" row) */
    files: VirtualFileItem[];
    /** Selected file names */
    selectedFiles: Set<string>;
    /** Columns to show */
    visibleColumns: string[];
    /** Row height in pixels */
    rowHeight?: number;

    // --- Navigation ---
    currentPath: string;
    isAtRoot: boolean;
    onNavigateUp: () => void;

    // --- Rendering ---
    renderIcon: (file: VirtualFileItem) => React.ReactNode;
    renderGoUpIcon: () => React.ReactNode;
    renderName: (file: VirtualFileItem, index: number) => React.ReactNode;
    renderTags?: (file: VirtualFileItem) => React.ReactNode;
    renderSyncBadge?: (file: VirtualFileItem) => React.ReactNode;
    renderPermissions?: (file: VirtualFileItem) => React.ReactNode;
    formatBytes: (bytes: number | null) => string;
    formatDate: (date: string | Date | null) => string;
    displayName: (name: string, isDir: boolean) => string;
    folderTypeLabel: string;

    // --- Events ---
    onFileClick: (e: React.MouseEvent, file: VirtualFileItem, index: number) => void;
    onFileDoubleClick: (file: VirtualFileItem) => void;
    onContextMenu: (e: React.MouseEvent, file: VirtualFileItem) => void;

    // --- Drag & Drop ---
    onDragStart?: (e: React.DragEvent, file: VirtualFileItem) => void;
    onDragEnd?: () => void;
    onDragOver?: (e: React.DragEvent, file: VirtualFileItem) => void;
    onDragLeave?: (e: React.DragEvent) => void;
    onDrop?: (e: React.DragEvent, file: VirtualFileItem) => void;
    dropTargetPath?: string | null;
    dragSourcePaths?: string[];

    // --- Header ---
    headerContent: React.ReactNode;

    // --- Labels ---
    parentFolderLabel: string;
}

// Row height default: matches py-2 (8+8) + content (~17) = ~33px
const DEFAULT_ROW_HEIGHT = 33;

// ============================================================================
// Row props passed via rowProps to List
// ============================================================================

interface RowExtraProps {
    files: VirtualFileItem[];
    listProps: VirtualFileListProps;
}

// ============================================================================
// Virtual Row component (react-window v2 API)
// ============================================================================

function VirtualRow({ index, style, files, listProps }: RowComponentProps<RowExtraProps>): ReactElement | null {
    // Index 0 = "Go Up" row
    if (index === 0) {
        const disabled = listProps.isAtRoot;
        return (
            <div
                style={style}
                role="row"
                className={`flex items-center ${disabled ? 'opacity-50 cursor-not-allowed' : 'hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer'} border-b border-gray-100 dark:border-gray-700`}
                onDoubleClick={() => !disabled && listProps.onNavigateUp()}
            >
                <div className="flex-1 min-w-0 px-4 py-1 flex items-center gap-2 text-gray-500">
                    {listProps.renderGoUpIcon()}
                    <span className="italic">{listProps.parentFolderLabel}</span>
                </div>
                {listProps.visibleColumns.includes('size') && (
                    <div className="w-[90px] shrink-0 px-4 py-1 text-sm text-gray-400">{'\u2014'}</div>
                )}
                {listProps.visibleColumns.includes('type') && (
                    <div className="hidden xl:block w-[80px] shrink-0 px-3 py-1 text-sm text-gray-400">{'\u2014'}</div>
                )}
                {listProps.visibleColumns.includes('permissions') && (
                    <div className="hidden xl:block w-[90px] shrink-0 px-3 py-1 text-sm text-gray-400">{'\u2014'}</div>
                )}
                {listProps.visibleColumns.includes('modified') && (
                    <div className="w-[140px] shrink-0 px-4 py-1 text-sm text-gray-400">{'\u2014'}</div>
                )}
            </div>
        );
    }

    // File rows: index - 1 maps to files array
    const fileIndex = index - 1;
    const file = files[fileIndex];
    if (!file) return null;

    const isSelected = listProps.selectedFiles.has(file.name);
    const isDropTarget = listProps.dropTargetPath === file.path && file.is_dir;
    const isDragSource = listProps.dragSourcePaths?.includes(file.path);

    return (
        <div
            style={style}
            data-file-row
            role="row"
            aria-selected={isSelected}
            draggable={file.name !== '..'}
            onDragStart={(e) => listProps.onDragStart?.(e, file)}
            onDragEnd={() => listProps.onDragEnd?.()}
            onDragOver={(e) => listProps.onDragOver?.(e, file)}
            onDragLeave={(e) => listProps.onDragLeave?.(e)}
            onDrop={(e) => file.is_dir && listProps.onDrop?.(e, file)}
            onClick={(e) => listProps.onFileClick(e, file, fileIndex)}
            onDoubleClick={() => listProps.onFileDoubleClick(file)}
            onContextMenu={(e) => listProps.onContextMenu(e, file)}
            className={`flex items-center cursor-pointer transition-colors border-b border-gray-100 dark:border-gray-700 ${
                isDropTarget
                    ? 'bg-green-100 dark:bg-green-900/40 ring-2 ring-green-500'
                    : isSelected
                        ? 'bg-blue-100 dark:bg-blue-900/40'
                        : 'hover:bg-blue-50 dark:hover:bg-gray-700'
            } ${isDragSource ? 'opacity-50' : ''}`}
        >
            <div className="flex-1 min-w-0 px-4 py-1 flex items-center gap-2">
                {listProps.renderIcon(file)}
                {listProps.renderName(file, fileIndex)}
                {listProps.renderTags?.(file)}
                {listProps.renderSyncBadge?.(file)}
            </div>
            {listProps.visibleColumns.includes('size') && (
                <div className="w-[90px] shrink-0 px-4 py-1 text-sm text-gray-500">
                    {file.size !== null ? listProps.formatBytes(file.size) : '-'}
                </div>
            )}
            {listProps.visibleColumns.includes('type') && (
                <div className="hidden xl:block w-[80px] shrink-0 px-3 py-1 text-xs text-gray-500 uppercase truncate">
                    {file.is_dir ? listProps.folderTypeLabel : (file.name.includes('.') ? file.name.split('.').pop() : '\u2014')}
                </div>
            )}
            {listProps.visibleColumns.includes('permissions') && listProps.renderPermissions && (
                <div className="hidden xl:block w-[90px] shrink-0 px-3 py-1">
                    {listProps.renderPermissions(file)}
                </div>
            )}
            {listProps.visibleColumns.includes('modified') && (
                <div className="w-[140px] shrink-0 px-4 py-1 text-xs text-gray-500 whitespace-nowrap">
                    {listProps.formatDate(file.modified)}
                </div>
            )}
        </div>
    );
}

// ============================================================================
// Non-virtualized row (same rendering, no style positioning)
// ============================================================================

function PlainRow({ index, files, listProps }: { index: number; files: VirtualFileItem[]; listProps: VirtualFileListProps }) {
    return VirtualRow({ index, style: {}, files, listProps, ariaAttributes: { 'aria-posinset': index + 1, 'aria-setsize': files.length + 1, role: 'listitem' } });
}

// ============================================================================
// Main Component
// ============================================================================

export const VirtualFileList: React.FC<VirtualFileListProps> = (props) => {
    const containerRef = useRef<HTMLDivElement>(null);
    const [containerHeight, setContainerHeight] = useState(400);
    const rowHeight = props.rowHeight ?? DEFAULT_ROW_HEIGHT;

    // Total items: files + 1 for "go up" row
    const itemCount = props.files.length + 1;

    // Measure container height with ResizeObserver
    useEffect(() => {
        const el = containerRef.current;
        if (!el) return;
        const ro = new ResizeObserver((entries) => {
            for (const entry of entries) {
                setContainerHeight(entry.contentRect.height);
            }
        });
        ro.observe(el);
        setContainerHeight(el.clientHeight);
        return () => ro.disconnect();
    }, []);

    const rowProps: RowExtraProps = { files: props.files, listProps: props };

    // If very few files, skip virtualization (overhead not worth it)
    const shouldVirtualize = itemCount > 100;

    if (!shouldVirtualize) {
        return (
            <div className="w-full" role="grid">
                {props.headerContent}
                {Array.from({ length: itemCount }, (_, i) => (
                    <PlainRow
                        key={i === 0 ? '__go_up__' : (props.files[i - 1]?.path ?? `__idx_${i}`)}
                        index={i}
                        files={props.files}
                        listProps={props}
                    />
                ))}
            </div>
        );
    }

    return (
        <div className="w-full h-full flex flex-col" role="grid">
            {props.headerContent}
            <div ref={containerRef} className="flex-1 min-h-0">
                <List<RowExtraProps>
                    rowComponent={VirtualRow}
                    rowCount={itemCount}
                    rowHeight={rowHeight}
                    rowProps={rowProps}
                    overscanCount={10}
                    style={{ height: containerHeight, width: '100%' }}
                />
            </div>
        </div>
    );
};

VirtualFileList.displayName = 'VirtualFileList';
