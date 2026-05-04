// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { ArrowDown, ArrowUp, Settings2, Star } from 'lucide-react';
import { useTranslation } from '../../i18n';
import {
    MY_SERVERS_TABLE_COLUMNS,
    type MyServersSort,
    type MyServersSortableColId,
    type MyServersTableColId,
    type MyServersTableColumn,
    type MyServersTableColumnsResult,
} from '../../hooks/useMyServersColumns';
import type { TableColumnDef } from '../../hooks/useTableColumns';
import { TableColResizer } from '../ui/TableColResizer';
import { TableColumnsManager } from '../ui/TableColumnsManager';

interface MyServersTableHeaderProps {
    columns: MyServersTableColumnsResult;
    onReorder: (sourceId: MyServersTableColId, targetId: MyServersTableColId) => void;
    onLiveResize: (id: MyServersTableColId, widthPx: number) => void;
}

const nextSortFor = (
    column: MyServersTableColumn,
    sort: MyServersSort | null,
): MyServersSort | null => {
    if (column.id === 'index') return null;
    if (!column.sortable) return sort;
    if (!sort || sort.colId !== column.id) {
        return { colId: column.id as MyServersSortableColId, dir: 'asc' };
    }
    if (sort.dir === 'asc') {
        return { colId: column.id as MyServersSortableColId, dir: 'desc' };
    }
    return null;
};

const findLegacyClass = (id: MyServersTableColId): string =>
    MY_SERVERS_TABLE_COLUMNS.find(col => col.id === id)?.className || '';

export function MyServersTableHeader({ columns, onReorder, onLiveResize }: MyServersTableHeaderProps) {
    const t = useTranslation();
    const { config, orderedVisibleColumns, orderedAllColumns, setSort, setVisible, setWidth, reset } = columns;
    const sort = config.sort as MyServersSort | null;
    const sortLabel = sort
        ? t(MY_SERVERS_TABLE_COLUMNS.find(col => col.id === sort.colId)?.labelKey || '')
        : '';
    const lastVisibleId = orderedVisibleColumns[orderedVisibleColumns.length - 1]?.id;

    const [showManager, setShowManager] = React.useState(false);
    const [dragId, setDragId] = React.useState<MyServersTableColId | null>(null);
    const [overId, setOverId] = React.useState<MyServersTableColId | null>(null);
    const managerWrapperRef = React.useRef<HTMLDivElement | null>(null);

    React.useEffect(() => {
        if (!showManager) return;
        const onClickOutside = (e: MouseEvent) => {
            if (!managerWrapperRef.current) return;
            if (!managerWrapperRef.current.contains(e.target as Node)) setShowManager(false);
        };
        const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') setShowManager(false); };
        document.addEventListener('mousedown', onClickOutside);
        document.addEventListener('keydown', onKey);
        return () => {
            document.removeEventListener('mousedown', onClickOutside);
            document.removeEventListener('keydown', onKey);
        };
    }, [showManager]);

    return (
        <thead className="sticky top-0 z-20 bg-gray-50 dark:bg-gray-800 border-b border-gray-200 dark:border-gray-700 shadow-sm">
            <tr>
                {orderedVisibleColumns.map((column: TableColumnDef<MyServersTableColId>) => {
                    const legacy = MY_SERVERS_TABLE_COLUMNS.find(c => c.id === column.id);
                    const isSorted = sort?.colId === column.id;
                    const label = t(column.labelKey);
                    const displayLabel = column.id === 'favorite'
                        ? <Star size={12} fill={isSorted ? 'currentColor' : 'none'} />
                        : column.id === 'index'
                            ? '#'
                            : label;
                    const isPinned = !!(column.pinnedStart || column.pinnedEnd);
                    const canDrag = !isPinned;
                    const sortable = !!legacy?.sortable;
                    const title = column.id === 'index'
                        ? sort === null
                            ? t('introHub.table.manualOrderActive')
                            : t('introHub.table.clickToReturnManual', { column: sortLabel })
                        : sortable
                            ? t('introHub.table.clickToSortBy', { column: label })
                            : label;
                    const ariaSort = isSorted
                        ? sort.dir === 'asc' ? 'ascending' : 'descending'
                        : undefined;
                    const legacyCls = findLegacyClass(column.id);
                    const alignClass = legacyCls.includes('text-right')
                        ? 'justify-end'
                        : legacyCls.includes('text-center') ? 'justify-center' : '';
                    const isDragTarget = overId === column.id && dragId !== null && dragId !== column.id;
                    const isDragging = dragId === column.id;

                    const content = (
                        <span className={`flex items-center gap-1 ${alignClass}`}>
                            <span className="inline-flex items-center">{displayLabel}</span>
                            {isSorted && (sort.dir === 'asc' ? <ArrowUp size={11} /> : <ArrowDown size={11} />)}
                        </span>
                    );
                    const columnControl = sortable ? (
                        <button
                            type="button"
                            onClick={() => setSort(nextSortFor(legacy, sort))}
                            className="w-full cursor-pointer hover:text-gray-800 dark:hover:text-gray-100 transition-colors"
                            title={title}
                        >
                            {content}
                        </button>
                    ) : (
                        <span title={title}>{content}</span>
                    );

                    return (
                        <th
                            key={column.id}
                            scope="col"
                            aria-sort={ariaSort as React.AriaAttributes['aria-sort']}
                            draggable={canDrag}
                            onDragStart={canDrag ? (e) => {
                                setDragId(column.id);
                                e.dataTransfer.effectAllowed = 'move';
                                try { e.dataTransfer.setData('text/plain', column.id); } catch { /* ignore */ }
                            } : undefined}
                            onDragEnter={canDrag ? (e) => {
                                e.preventDefault();
                                if (dragId && dragId !== column.id && !column.pinnedStart && !column.pinnedEnd) {
                                    setOverId(column.id);
                                }
                            } : undefined}
                            onDragOver={canDrag ? (e) => {
                                e.preventDefault();
                                e.dataTransfer.dropEffect = 'move';
                            } : undefined}
                            onDrop={canDrag ? (e) => {
                                e.preventDefault();
                                if (dragId && dragId !== column.id) onReorder(dragId, column.id);
                                setDragId(null);
                                setOverId(null);
                            } : undefined}
                            onDragEnd={canDrag ? () => { setDragId(null); setOverId(null); } : undefined}
                            className={`${legacyCls} relative px-3 py-2 text-[11px] font-semibold uppercase text-gray-500 dark:text-gray-400 tracking-wide whitespace-nowrap
                                ${canDrag ? 'cursor-grab active:cursor-grabbing' : ''}
                                ${isDragging ? 'opacity-40' : ''}
                                ${isDragTarget ? 'bg-blue-100 dark:bg-blue-900/30' : ''}`}
                        >
                            {column.id === lastVisibleId ? (
                                <div className={`flex items-center gap-1 ${alignClass}`}>
                                    <div className="min-w-0 flex-1">{columnControl}</div>
                                    <div ref={managerWrapperRef} className="relative shrink-0">
                                        <button
                                            type="button"
                                            onClick={(e) => { e.stopPropagation(); setShowManager(s => !s); }}
                                            onMouseDown={(e) => e.stopPropagation()}
                                            className="p-1 rounded-md cursor-pointer text-gray-400 hover:text-gray-700 hover:bg-gray-200 dark:hover:text-gray-200 dark:hover:bg-gray-700"
                                            title={t('table.manageColumns')}
                                        >
                                            <Settings2 size={13} />
                                        </button>
                                        {showManager && (
                                            <TableColumnsManager
                                                columns={orderedAllColumns}
                                                visibility={config.visibility}
                                                orderedAllColumns={orderedAllColumns}
                                                onSetVisible={setVisible}
                                                onSetOrder={(order) => columns.setOrder(order)}
                                                onReset={() => { reset(); setShowManager(false); }}
                                                onClose={() => setShowManager(false)}
                                            />
                                        )}
                                    </div>
                                </div>
                            ) : columnControl}
                            {!column.pinnedEnd && (
                                <TableColResizer
                                    currentWidth={config.widths[column.id]}
                                    minWidth={column.minWidth}
                                    onResize={(w) => onLiveResize(column.id, w)}
                                    onResizeEnd={(w) => setWidth(column.id, w)}
                                    title={t('table.dragToResize')}
                                />
                            )}
                        </th>
                    );
                })}
            </tr>
        </thead>
    );
}
