// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// The one trash table, shared by every provider's '🗑 View Trash' dialog.
//
// What it fixes, all reported in discussion #347:
//   - nothing was sortable, in any of the seventeen copies;
//   - files and folders were told apart only by an icon, with no way to group
//     them, so a Type column now sorts them;
//   - the Name header sat at the left edge of a cell whose content starts after
//     the type icon, so it read as hanging over the checkbox column. The icon
//     has its own column now and the header lines up with the names;
//   - the deleted-at column was narrow enough to wrap each timestamp onto two
//     lines, halving how many rows fit on screen. It is wider, and does not wrap;
//   - selection was one click per row. Shift extends a range, Ctrl adds without
//     losing what is already selected, and a rubber-band drag selects an area.

import * as React from 'react';
import { CheckSquare, File, FileQuestion, Folder, Square, ChevronUp, ChevronDown, Settings2 } from 'lucide-react';
import { formatSize, formatDate } from '../../utils/formatters';
import { useMarqueeSelection } from '../../hooks/useMarqueeSelection';
import { useTranslation } from '../../i18n';
import { useTableColumns, type TableColumnDef, type TableColAlign } from '../../hooks/useTableColumns';
import { TableColumnsManager } from '../ui/TableColumnsManager';
import { TableColResizer } from '../ui/TableColResizer';
import {
    nextTrashSort,
    selectionAfterRowClick,
    sortTrashRows,
    type TrashRow,
    type TrashSort,
    type TrashSortKey,
} from './trashRows';

export type { TrashRow } from './trashRows';

/** A provider-specific column (Nextcloud's original path, S3's per-row action). */
export interface TrashExtraColumn {
    key: string;
    header: React.ReactNode;
    /** Width/alignment classes for both the header and the cells. */
    className?: string;
    render: (row: TrashRow) => React.ReactNode;
}

interface TrashTableProps {
    rows: TrashRow[];
    /** Omit both selection props for a read-only trash (Internxt, which has no
     *  restore or delete API of its own): the checkbox column and the rubber
     *  band disappear, the sorting stays. */
    selected?: Set<string>;
    setSelected?: React.Dispatch<React.SetStateAction<Set<string>>>;
    /** Tailwind tint for a selected row, e.g. 'bg-cyan-500/10'. */
    rowTintClass?: string;
    /** Tailwind colour for the checked box, e.g. 'text-cyan-500'. */
    accentClass?: string;
    extraColumns?: TrashExtraColumn[];
    /** Drop the Type column where every row is the same kind (the S3 view lists
     *  object versions, never folders, and says what each one is in its own
     *  Kind column). */
    showTypeColumn?: boolean;
}

const TRASH_COLUMNS: TableColumnDef<TrashSortKey>[] = [
    // settings.columnType is the file-list column header of the same name,
    // already translated everywhere; no 47th copy of the word "Type".
    { id: 'type', labelKey: 'settings.columnType', sortable: true, defaultVisible: true, defaultWidth: 64, minWidth: 48, defaultAlign: 'center' },
    { id: 'name', labelKey: 'common.name', sortable: true, defaultVisible: true, defaultWidth: 300, minWidth: 120, defaultAlign: 'left' },
    { id: 'size', labelKey: 'common.size', sortable: true, defaultVisible: true, defaultWidth: 100, minWidth: 72, defaultAlign: 'right' },
    { id: 'deletedAt', labelKey: 'contextMenu.trashDeletedDate', sortable: true, defaultVisible: true, defaultWidth: 210, minWidth: 160, defaultAlign: 'left' },
];

const alignClass = (align: TableColAlign): string => align === 'right'
    ? 'text-right'
    : align === 'center' ? 'text-center' : 'text-left';

const justifyClass = (align: TableColAlign): string => align === 'right'
    ? 'justify-end'
    : align === 'center' ? 'justify-center' : 'justify-start';

const NO_SELECTION: Set<string> = new Set();

export const TrashTable: React.FC<TrashTableProps> = ({
    rows,
    selected,
    setSelected,
    rowTintClass = 'bg-blue-500/10',
    accentClass = 'text-blue-500',
    extraColumns = [],
    showTypeColumn = true,
}) => {
    const t = useTranslation();
    const selectable = !!setSelected;
    const selection = selected ?? NO_SELECTION;
    const anchorRef = React.useRef<number | null>(null);
    const containerRef = React.useRef<HTMLDivElement | null>(null);
    const managerRef = React.useRef<HTMLDivElement | null>(null);
    const [showManager, setShowManager] = React.useState(false);
    const columns = useTableColumns({ columns: TRASH_COLUMNS, storageKey: 'trash_table' });
    const visibleColumns = React.useMemo(
        () => columns.orderedVisibleColumns.filter((column) => showTypeColumn || column.id !== 'type'),
        [columns.orderedVisibleColumns, showTypeColumn],
    );
    const [liveWidths, setLiveWidths] = React.useState(columns.config.widths);
    React.useEffect(() => setLiveWidths(columns.config.widths), [columns.config.widths]);
    const sort: TrashSort | null = columns.config.sort
        ? { key: columns.config.sort.colId, direction: columns.config.sort.dir }
        : null;

    const ordered = React.useMemo(() => sortTrashRows(rows, sort), [rows, sort]);

    React.useEffect(() => {
        if (!showManager) return;
        const closeOutside = (event: MouseEvent) => {
            if (!managerRef.current?.contains(event.target as Node)) setShowManager(false);
        };
        const closeOnEscape = (event: KeyboardEvent) => {
            if (event.key === 'Escape') setShowManager(false);
        };
        document.addEventListener('mousedown', closeOutside);
        document.addEventListener('keydown', closeOnEscape);
        return () => {
            document.removeEventListener('mousedown', closeOutside);
            document.removeEventListener('keydown', closeOnEscape);
        };
    }, [showManager]);

    // Rubber band. The hook keys items by `data-file-name`, which here carries
    // the row id, so the set it produces is the same set the checkboxes use.
    //
    // This wrapper is the scroller as well as the marquee coordinate space.
    // Provider dialogs give it a bounded `h-full`; owning scrollTop here lets
    // the shared hook auto-scroll while a rubber band crosses an edge.
    const noopSetSelected = React.useCallback(() => {}, []);
    const marquee = useMarqueeSelection({
        containerRef,
        itemSelector: 'tr[data-file-row]',
        selected: selection,
        setSelected: setSelected ?? noopSetSelected,
        setLastIndex: (i) => { anchorRef.current = i; },
        disabled: !selectable,
    });

    const onRowClick = (index: number, e: React.MouseEvent) => {
        if (!setSelected) return;
        const result = selectionAfterRowClick(ordered, index, selection, anchorRef.current, {
            shift: e.shiftKey,
        });
        anchorRef.current = result.anchor;
        setSelected(result.selected);
    };

    const sortIndicator = (key: TrashSortKey) => {
        if (sort?.key !== key) return null;
        return sort.direction === 'asc'
            ? <ChevronUp size={11} className="inline-block ml-0.5 -mt-px" />
            : <ChevronDown size={11} className="inline-block ml-0.5 -mt-px" />;
    };

    const renderCoreCell = (row: TrashRow, key: TrashSortKey) => {
        switch (key) {
            case 'type':
                if (row.typeUnknown) return <FileQuestion size={13} className="inline-block text-gray-400" />;
                return row.isDir
                    ? <Folder size={13} className="inline-block text-yellow-500" />
                    : <File size={13} className="inline-block text-gray-500 dark:text-gray-500" />;
            case 'name':
                return <span className="block truncate text-gray-900 dark:text-gray-100">{row.name}</span>;
            case 'size':
                return <span className="tabular-nums text-gray-600 dark:text-gray-400">{row.isDir || row.size == null ? '-' : formatSize(row.size)}</span>;
            case 'deletedAt': {
                const label = row.deletedAtLabel ?? (row.deletedAt ? formatDate(row.deletedAt) : '-');
                return <span className="whitespace-nowrap tabular-nums text-gray-500 dark:text-gray-500" title={label === '-' ? undefined : label}>{label}</span>;
            }
            default:
                return null;
        }
    };

    const tableMinWidth = (selectable ? 32 : 0)
        + visibleColumns.reduce((sum, column) => sum + liveWidths[column.id], 0)
        + extraColumns.length * 120
        + 40;

    return (
        <div ref={containerRef} className="relative h-full min-h-0 overflow-auto" onMouseDown={marquee.onMouseDown}>
            <table className="table-fixed text-xs select-none" style={{ width: `${tableMinWidth}px`, minWidth: `${tableMinWidth}px` }}>
                <colgroup>
                    {selectable && <col style={{ width: '32px' }} />}
                    {visibleColumns.map((column) => <col key={column.id} style={{ width: `${liveWidths[column.id]}px` }} />)}
                    {extraColumns.map((column) => <col key={column.key} />)}
                    <col style={{ width: '40px' }} />
                </colgroup>
                <thead className="sticky top-0 z-10 bg-gray-50 dark:bg-gray-800 border-b border-gray-200 dark:border-gray-700">
                    <tr className="text-left text-gray-500 dark:text-gray-500">
                        {selectable && <th className="w-8 px-2 py-1.5" />}
                        {visibleColumns.map((column) => {
                            const key = column.id;
                            const alignment = columns.resolveAlign(key);
                            return (
                            <th key={key} className={`relative px-2 py-1.5 whitespace-nowrap ${alignClass(alignment)}`}>
                                <button
                                    type="button"
                                    onClick={() => {
                                        // The anchor is an index in the current
                                        // order; a sort gives that index to a
                                        // different row, so start a new range.
                                        anchorRef.current = null;
                                        const next = nextTrashSort(sort, key);
                                        columns.setSort(next ? { colId: next.key, dir: next.direction } : null);
                                    }}
                                    className={`flex w-full items-center hover:text-gray-700 dark:hover:text-gray-300 ${justifyClass(alignment)}`}
                                    aria-label={t(column.labelKey)}
                                >
                                    {t(column.labelKey)}
                                    {sortIndicator(key)}
                                </button>
                                <TableColResizer
                                    currentWidth={liveWidths[key]}
                                    minWidth={column.minWidth}
                                    onResize={(width) => setLiveWidths((current) => ({ ...current, [key]: width }))}
                                    onResizeEnd={(width) => columns.setWidth(key, width)}
                                    title={t('table.dragToResize')}
                                />
                            </th>
                            );
                        })}
                        {extraColumns.map((col) => (
                            <th key={col.key} className={`px-2 py-1.5 ${col.className ?? ''}`}>
                                {col.header}
                            </th>
                        ))}
                        <th className="px-1 py-1.5 text-right">
                            <div ref={managerRef} className="relative inline-block">
                                <button
                                    type="button"
                                    onClick={(event) => { event.stopPropagation(); setShowManager((value) => !value); }}
                                    className="rounded p-1 text-gray-400 hover:bg-gray-200 hover:text-gray-700 dark:hover:bg-gray-700 dark:hover:text-gray-200"
                                    title={t('table.manageColumns')}
                                    aria-label={t('table.manageColumns')}
                                >
                                    <Settings2 size={13} />
                                </button>
                                {showManager && (
                                    <TableColumnsManager
                                        columns={TRASH_COLUMNS}
                                        visibility={columns.config.visibility}
                                        orderedAllColumns={columns.orderedAllColumns}
                                        onSetVisible={columns.setVisible}
                                        onSetOrder={columns.setOrder}
                                        onReset={() => { columns.reset(); setShowManager(false); }}
                                        onClose={() => setShowManager(false)}
                                        resolveAlign={columns.resolveAlign}
                                        onSetAlign={columns.setAlign}
                                    />
                                )}
                            </div>
                        </th>
                    </tr>
                </thead>
                <tbody>
                    {ordered.map((row, index) => {
                        const isSelected = selection.has(row.id);
                        return (
                            <tr
                                key={row.id}
                                data-file-row
                                data-file-name={row.id}
                                data-file-index={index}
                                className={`hover:bg-gray-100 dark:hover:bg-gray-700 border-b border-gray-200 dark:border-gray-700/30 ${
                                    selectable ? 'cursor-pointer' : ''
                                } ${isSelected ? rowTintClass : ''}`}
                                onClick={(e) => onRowClick(index, e)}
                            >
                                {selectable && (
                                    <td className="px-2 py-1.5 text-center">
                                        {isSelected ? (
                                            <CheckSquare size={13} className={accentClass} />
                                        ) : (
                                            <Square size={13} className="text-gray-500 dark:text-gray-500" />
                                        )}
                                    </td>
                                )}
                                {visibleColumns.map((column) => (
                                    <td key={column.id} className={`px-2 py-1.5 ${alignClass(columns.resolveAlign(column.id))}`}>
                                        {renderCoreCell(row, column.id)}
                                    </td>
                                ))}
                                {extraColumns.map((col) => (
                                    <td key={col.key} className={`px-2 py-1.5 ${col.className ?? ''}`}>
                                        {col.render(row)}
                                    </td>
                                ))}
                                <td />
                            </tr>
                        );
                    })}
                </tbody>
            </table>
            {marquee.box && (
                <div
                    className="pointer-events-none absolute border border-blue-400 bg-blue-400/15"
                    style={{
                        left: marquee.box.left,
                        top: marquee.box.top,
                        width: marquee.box.width,
                        height: marquee.box.height,
                    }}
                />
            )}
        </div>
    );
};

export default TrashTable;
