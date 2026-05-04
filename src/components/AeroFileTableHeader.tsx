// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { ArrowDown, ArrowUp, Settings2 } from 'lucide-react';
import { useTranslation } from '../i18n';
import type { TableColumnDef, UseTableColumnsResult } from '../hooks/useTableColumns';
import { TableColResizer } from './ui/TableColResizer';
import { TableColumnsManager } from './ui/TableColumnsManager';

interface AeroFileTableHeaderProps<TColId extends string> {
    columns: UseTableColumnsResult<TColId>;
    onLiveResize: (id: TColId, widthPx: number) => void;
    /** Optional className override applied per-th (e.g. tablet-hide for "type"). */
    columnHeaderClassName?: (id: TColId) => string | undefined;
}

export function AeroFileTableHeader<TColId extends string>({
    columns,
    onLiveResize,
    columnHeaderClassName,
}: AeroFileTableHeaderProps<TColId>) {
    const t = useTranslation();
    const { config, orderedVisibleColumns, orderedAllColumns, setSort, setVisible, setOrder, setWidth, reset } = columns;
    const [showManager, setShowManager] = React.useState(false);
    const [dragId, setDragId] = React.useState<TColId | null>(null);
    const [overId, setOverId] = React.useState<TColId | null>(null);
    const wrapRef = React.useRef<HTMLDivElement | null>(null);

    React.useEffect(() => {
        if (!showManager) return;
        const onClickOutside = (e: MouseEvent) => {
            if (!wrapRef.current) return;
            if (!wrapRef.current.contains(e.target as Node)) setShowManager(false);
        };
        const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') setShowManager(false); };
        document.addEventListener('mousedown', onClickOutside);
        document.addEventListener('keydown', onKey);
        return () => {
            document.removeEventListener('mousedown', onClickOutside);
            document.removeEventListener('keydown', onKey);
        };
    }, [showManager]);

    const moveAfter = React.useCallback((sourceId: TColId, targetId: TColId) => {
        const middle = orderedAllColumns
            .filter(c => !c.pinnedStart && !c.pinnedEnd)
            .map(c => c.id);
        if (!middle.includes(sourceId) || !middle.includes(targetId)) return;
        const next = middle.filter(id => id !== sourceId);
        const idx = next.indexOf(targetId);
        if (idx < 0) return;
        next.splice(idx, 0, sourceId);
        const startIds = orderedAllColumns.filter(c => c.pinnedStart).map(c => c.id);
        const endIds = orderedAllColumns.filter(c => c.pinnedEnd).map(c => c.id);
        setOrder([...startIds, ...next, ...endIds]);
    }, [orderedAllColumns, setOrder]);

    const sort = config.sort;
    const lastVisibleId = orderedVisibleColumns[orderedVisibleColumns.length - 1]?.id;

    return (
        <thead className="bg-gray-50 dark:bg-gray-700 sticky top-0" role="rowgroup">
            <tr role="row">
                {orderedVisibleColumns.map((col: TableColumnDef<TColId>) => {
                    const label = t(col.labelKey);
                    const isSorted = sort?.colId === col.id;
                    const isPinned = !!(col.pinnedStart || col.pinnedEnd);
                    const canDrag = !isPinned;
                    const isDragging = dragId === col.id;
                    const isDragTarget = overId === col.id && dragId !== null && dragId !== col.id;
                    const extraCls = columnHeaderClassName?.(col.id) || '';
                    const sortable = col.sortable;
                    const onSortClick = () => {
                        if (!sortable) return;
                        if (!sort || sort.colId !== col.id) {
                            setSort({ colId: col.id, dir: 'asc' });
                        } else if (sort.dir === 'asc') {
                            setSort({ colId: col.id, dir: 'desc' });
                        } else {
                            setSort(null);
                        }
                    };
                    return (
                        <th
                            key={String(col.id)}
                            scope="col"
                            aria-sort={isSorted ? (sort.dir === 'asc' ? 'ascending' : 'descending') : undefined}
                            draggable={canDrag}
                            onDragStart={canDrag ? (e) => {
                                setDragId(col.id);
                                e.dataTransfer.effectAllowed = 'move';
                                try { e.dataTransfer.setData('text/plain', String(col.id)); } catch { /* ignore */ }
                            } : undefined}
                            onDragEnter={canDrag ? (e) => {
                                e.preventDefault();
                                if (dragId && dragId !== col.id) setOverId(col.id);
                            } : undefined}
                            onDragOver={canDrag ? (e) => { e.preventDefault(); e.dataTransfer.dropEffect = 'move'; } : undefined}
                            onDrop={canDrag ? (e) => {
                                e.preventDefault();
                                if (dragId && dragId !== col.id) moveAfter(dragId, col.id);
                                setDragId(null);
                                setOverId(null);
                            } : undefined}
                            onDragEnd={canDrag ? () => { setDragId(null); setOverId(null); } : undefined}
                            className={`relative px-3 py-2 text-left text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wider whitespace-nowrap select-none
                                ${canDrag ? 'cursor-grab active:cursor-grabbing' : ''}
                                ${isDragging ? 'opacity-40' : ''}
                                ${isDragTarget ? 'bg-blue-100 dark:bg-blue-900/30' : ''}
                                ${extraCls}`}
                        >
                            <div className="flex items-center gap-1">
                                <button
                                    type="button"
                                    onClick={onSortClick}
                                    disabled={!sortable}
                                    className={`flex items-center gap-1 min-w-0 flex-1 ${sortable ? 'cursor-pointer hover:text-gray-700 dark:hover:text-gray-200' : 'cursor-default'} text-left`}
                                    title={sortable ? t('introHub.table.clickToSortBy', { column: label }) : label}
                                >
                                    <span className="truncate">{label}</span>
                                    {isSorted && (sort.dir === 'asc'
                                        ? <ArrowUp size={12} className="text-blue-500 shrink-0" />
                                        : <ArrowDown size={12} className="text-blue-500 shrink-0" />)}
                                </button>
                                {col.id === lastVisibleId && (
                                    <div ref={wrapRef} className="relative shrink-0">
                                        <button
                                            type="button"
                                            onMouseDown={(e) => e.stopPropagation()}
                                            onClick={(e) => { e.stopPropagation(); setShowManager(s => !s); }}
                                            className="p-1 rounded-md text-gray-400 hover:text-gray-700 hover:bg-gray-200 dark:hover:text-gray-200 dark:hover:bg-gray-600"
                                            title={t('table.manageColumns')}
                                        >
                                            <Settings2 size={13} />
                                        </button>
                                        {showManager && (
                                            <TableColumnsManager<TColId>
                                                columns={orderedAllColumns}
                                                visibility={config.visibility}
                                                orderedAllColumns={orderedAllColumns}
                                                onSetVisible={setVisible}
                                                onSetOrder={setOrder}
                                                onReset={() => { reset(); setShowManager(false); }}
                                                onClose={() => setShowManager(false)}
                                            />
                                        )}
                                    </div>
                                )}
                            </div>
                            {!col.pinnedEnd && (
                                <TableColResizer
                                    currentWidth={config.widths[col.id]}
                                    minWidth={col.minWidth}
                                    onResize={(w) => onLiveResize(col.id, w)}
                                    onResizeEnd={(w) => setWidth(col.id, w)}
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
