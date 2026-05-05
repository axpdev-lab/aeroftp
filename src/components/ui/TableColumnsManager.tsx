// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { AlignLeft, AlignCenter, AlignRight, GripVertical, Lock, RotateCcw, X } from 'lucide-react';
import { useTranslation } from '../../i18n';
import type { TableColAlign, TableColumnDef } from '../../hooks/useTableColumns';

interface TableColumnsManagerProps<TColId extends string> {
    columns: TableColumnDef<TColId>[];
    visibility: Record<TColId, boolean>;
    /** Effective rendered order from useTableColumns (pinned at the ends). */
    orderedAllColumns: TableColumnDef<TColId>[];
    onSetVisible: (id: TColId, visible: boolean) => void;
    onSetOrder: (order: TColId[]) => void;
    onReset: () => void;
    onClose: () => void;
    /** Resolve effective alignment for a column (user override or default). */
    resolveAlign?: (id: TColId) => TableColAlign;
    /** Set or clear (null) the alignment override for a column. */
    onSetAlign?: (id: TColId, align: TableColAlign | null) => void;
}

interface ColumnRowProps<TColId extends string> {
    column: TableColumnDef<TColId>;
    visible: boolean;
    pinnedLabel: string;
    isDragging: boolean;
    isDropTarget: boolean;
    canDrag: boolean;
    onToggle: (visible: boolean) => void;
    onDragStart?: (e: React.DragEvent) => void;
    onDragEnter?: (e: React.DragEvent) => void;
    onDragOver?: (e: React.DragEvent) => void;
    onDrop?: (e: React.DragEvent) => void;
    onDragEnd?: () => void;
    align?: TableColAlign;
    onSetAlign?: (align: TableColAlign) => void;
}

function ColumnRow<TColId extends string>({
    column,
    visible,
    pinnedLabel,
    isDragging,
    isDropTarget,
    canDrag,
    onToggle,
    onDragStart,
    onDragEnter,
    onDragOver,
    onDrop,
    onDragEnd,
    align,
    onSetAlign,
}: ColumnRowProps<TColId>) {
    const t = useTranslation();
    const label = t(column.labelKey);
    const isPinned = !!(column.pinnedStart || column.pinnedEnd);
    const alignBtnCls = (active: boolean) => `p-1 rounded transition-colors ${active
        ? 'bg-blue-100 dark:bg-blue-900/40 text-blue-600 dark:text-blue-400'
        : 'text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:bg-gray-200 dark:hover:bg-gray-600'}`;
    return (
        <div
            data-testid={`table-cols-row-${column.id}`}
            draggable={canDrag}
            onDragStart={onDragStart}
            onDragEnter={onDragEnter}
            onDragOver={onDragOver}
            onDrop={onDrop}
            onDragEnd={onDragEnd}
            className={`flex items-center gap-2 px-2 py-1.5 rounded-md text-xs text-gray-700 dark:text-gray-200
                ${isDragging ? 'opacity-40' : ''}
                ${isDropTarget ? 'bg-blue-100 dark:bg-blue-900/30' : 'hover:bg-gray-100 dark:hover:bg-gray-700'}`}
        >
            <span
                className={`shrink-0 ${canDrag ? 'cursor-grab active:cursor-grabbing text-gray-400 hover:text-gray-700 dark:hover:text-gray-200' : 'text-gray-300 dark:text-gray-600'}`}
                title={canDrag ? t('table.dragToReorder') : pinnedLabel}
                aria-hidden="true"
            >
                {isPinned ? <Lock size={12} /> : <GripVertical size={14} />}
            </span>
            <label className="flex flex-1 items-center gap-2 cursor-pointer min-w-0">
                <input
                    type="checkbox"
                    checked={visible}
                    onChange={(e) => onToggle(e.target.checked)}
                    className="rounded border-gray-300 dark:border-gray-600 text-blue-600 focus:ring-blue-500"
                />
                <span className="truncate">{label || column.id}</span>
            </label>
            {onSetAlign && align && (
                <div className="flex items-center gap-0.5 shrink-0" role="group" aria-label={t('table.alignmentGroup')}>
                    <button
                        type="button"
                        onClick={() => onSetAlign('left')}
                        className={alignBtnCls(align === 'left')}
                        title={t('table.alignLeft')}
                        aria-pressed={align === 'left'}
                    >
                        <AlignLeft size={12} />
                    </button>
                    <button
                        type="button"
                        onClick={() => onSetAlign('center')}
                        className={alignBtnCls(align === 'center')}
                        title={t('table.alignCenter')}
                        aria-pressed={align === 'center'}
                    >
                        <AlignCenter size={12} />
                    </button>
                    <button
                        type="button"
                        onClick={() => onSetAlign('right')}
                        className={alignBtnCls(align === 'right')}
                        title={t('table.alignRight')}
                        aria-pressed={align === 'right'}
                    >
                        <AlignRight size={12} />
                    </button>
                </div>
            )}
            {isPinned && !onSetAlign && (
                <span className="shrink-0 text-[10px] tracking-wide text-gray-400 dark:text-gray-500">
                    {pinnedLabel}
                </span>
            )}
        </div>
    );
}

export function TableColumnsManager<TColId extends string>({
    columns,
    visibility,
    orderedAllColumns,
    onSetVisible,
    onSetOrder,
    onReset,
    onClose,
    resolveAlign,
    onSetAlign,
}: TableColumnsManagerProps<TColId>) {
    const t = useTranslation();
    const [dragId, setDragId] = React.useState<TColId | null>(null);
    const [overId, setOverId] = React.useState<TColId | null>(null);

    const middleIds = React.useMemo(
        () => orderedAllColumns
            .filter(col => !col.pinnedStart && !col.pinnedEnd)
            .map(col => col.id),
        [orderedAllColumns],
    );

    const reorderableIds = new Set(middleIds);

    const moveAfter = React.useCallback((sourceId: TColId, targetId: TColId) => {
        if (!reorderableIds.has(sourceId) || !reorderableIds.has(targetId) || sourceId === targetId) return;
        const next = middleIds.filter(id => id !== sourceId);
        const idx = next.indexOf(targetId);
        if (idx < 0) return;
        next.splice(idx, 0, sourceId);
        // Build the full order: pinnedStart (registry order) + new middle + pinnedEnd (registry order)
        const startIds = columns.filter(c => c.pinnedStart).map(c => c.id);
        const endIds = columns.filter(c => c.pinnedEnd).map(c => c.id);
        onSetOrder([...startIds, ...next, ...endIds]);
    // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [columns, middleIds.join('|'), onSetOrder]);

    return (
        <div
            data-testid="table-cols-manager"
            className="absolute right-0 top-full mt-2 z-30 w-64 rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 shadow-xl overflow-hidden"
            onClick={(e) => e.stopPropagation()}
        >
            <div className="flex items-center justify-between px-3 py-2 border-b border-gray-100 dark:border-gray-700 bg-gray-50 dark:bg-gray-900/40">
                <span className="text-xs font-semibold text-gray-700 dark:text-gray-200 tracking-wide">
                    {t('table.manageColumns')}
                </span>
                <button
                    type="button"
                    onClick={onClose}
                    className="p-1 rounded text-gray-400 hover:text-gray-700 dark:hover:text-gray-200 hover:bg-gray-200 dark:hover:bg-gray-700"
                    aria-label={t('common.close')}
                >
                    <X size={13} />
                </button>
            </div>
            <div className="p-1.5 max-h-[60vh] overflow-y-auto">
                {orderedAllColumns.map((column) => {
                    const isPinned = !!(column.pinnedStart || column.pinnedEnd);
                    const canDrag = !isPinned;
                    return (
                        <ColumnRow
                            key={column.id}
                            column={column}
                            visible={!!visibility[column.id]}
                            pinnedLabel={t('table.pinned')}
                            canDrag={canDrag}
                            isDragging={dragId === column.id}
                            isDropTarget={overId === column.id && dragId !== null && dragId !== column.id}
                            onToggle={(v) => onSetVisible(column.id, v)}
                            onDragStart={canDrag ? (e) => {
                                setDragId(column.id);
                                e.dataTransfer.effectAllowed = 'move';
                                try { e.dataTransfer.setData('text/plain', column.id); } catch { /* ignore */ }
                            } : undefined}
                            onDragEnter={canDrag ? (e) => {
                                e.preventDefault();
                                if (dragId && dragId !== column.id) setOverId(column.id);
                            } : undefined}
                            onDragOver={canDrag ? (e) => {
                                e.preventDefault();
                                e.dataTransfer.dropEffect = 'move';
                            } : undefined}
                            onDrop={canDrag ? (e) => {
                                e.preventDefault();
                                if (dragId && dragId !== column.id) moveAfter(dragId, column.id);
                                setDragId(null);
                                setOverId(null);
                            } : undefined}
                            onDragEnd={canDrag ? () => {
                                setDragId(null);
                                setOverId(null);
                            } : undefined}
                            align={resolveAlign && onSetAlign ? resolveAlign(column.id) : undefined}
                            onSetAlign={onSetAlign ? ((align) => onSetAlign(column.id, align)) : undefined}
                        />
                    );
                })}
            </div>
            <div className="border-t border-gray-100 dark:border-gray-700 px-3 py-2">
                <button
                    type="button"
                    onClick={() => {
                        onReset();
                    }}
                    className="flex items-center gap-2 text-xs text-gray-600 dark:text-gray-300 hover:text-blue-600 dark:hover:text-blue-400"
                    title={t('table.resetToDefaults')}
                >
                    <RotateCcw size={12} />
                    <span>{t('table.resetToDefaults')}</span>
                </button>
            </div>
        </div>
    );
}
