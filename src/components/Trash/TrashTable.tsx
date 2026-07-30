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
import { CheckSquare, File, Folder, Square, ChevronUp, ChevronDown } from 'lucide-react';
import { formatSize, formatDate } from '../../utils/formatters';
import { useMarqueeSelection } from '../../hooks/useMarqueeSelection';
import { useTranslation } from '../../i18n';
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

const HEADERS: { key: TrashSortKey; labelKey: string; fallback: string; className: string }[] = [
    // settings.columnType is the file-list column header of the same name,
    // already translated everywhere; no 47th copy of the word "Type".
    { key: 'type', labelKey: 'settings.columnType', fallback: 'Type', className: 'w-10' },
    { key: 'name', labelKey: 'common.name', fallback: 'Name', className: '' },
    { key: 'size', labelKey: 'common.size', fallback: 'Size', className: 'w-20 text-right' },
    {
        key: 'deletedAt',
        labelKey: 'contextMenu.trashDeletedDate',
        fallback: 'Deleted',
        // Wide enough for a full date and time on one line, and never wrapped:
        // the Name column gives up the width, which it has to spare.
        className: 'w-48 whitespace-nowrap',
    },
];

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
    const [sort, setSort] = React.useState<TrashSort | null>(null);
    const anchorRef = React.useRef<number | null>(null);
    const containerRef = React.useRef<HTMLDivElement | null>(null);

    const ordered = React.useMemo(() => sortTrashRows(rows, sort), [rows, sort]);

    // Rubber band. The hook keys items by `data-file-name`, which here carries
    // the row id, so the set it produces is the same set the checkboxes use.
    //
    // The element below is the full-height table wrapper, not the scroller: the
    // scrolling ancestor belongs to each provider's dialog. Hit testing is
    // unaffected, because an unclipped wrapper's bounding rect moves with the
    // scroll and pointer-to-content coordinates stay consistent. What does not
    // work is the hook's edge auto-scroll, which reads `scrollTop` from this
    // element and always sees 0: dragging past the bottom edge selects what is
    // visible and stops there instead of scrolling on. Selecting more means
    // scrolling first, or holding Ctrl and dragging again to add.
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

    return (
        <div ref={containerRef} className="relative h-full" onMouseDown={marquee.onMouseDown}>
            <table className="w-full text-xs select-none">
                <thead className="sticky top-0 z-10 bg-gray-50 dark:bg-gray-800 border-b border-gray-200 dark:border-gray-700">
                    <tr className="text-left text-gray-500 dark:text-gray-500">
                        {selectable && <th className="w-8 px-2 py-1.5" />}
                        {HEADERS.filter((h) => showTypeColumn || h.key !== 'type').map(({ key, labelKey, fallback, className }) => (
                            <th key={key} className={`px-2 py-1.5 ${className}`}>
                                <button
                                    type="button"
                                    onClick={() => setSort((cur) => nextTrashSort(cur, key))}
                                    className="inline-flex items-center hover:text-gray-700 dark:hover:text-gray-300"
                                    aria-label={t(labelKey) || fallback}
                                >
                                    {t(labelKey) || fallback}
                                    {sortIndicator(key)}
                                </button>
                            </th>
                        ))}
                        {extraColumns.map((col) => (
                            <th key={col.key} className={`px-2 py-1.5 ${col.className ?? ''}`}>
                                {col.header}
                            </th>
                        ))}
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
                                {showTypeColumn && (
                                    <td className="px-2 py-1.5 text-center">
                                        {row.isDir ? (
                                            <Folder size={13} className="inline-block text-yellow-500" />
                                        ) : (
                                            <File size={13} className="inline-block text-gray-500 dark:text-gray-500" />
                                        )}
                                    </td>
                                )}
                                <td className="px-2 py-1.5">
                                    <span className="block truncate text-gray-900 dark:text-gray-100">{row.name}</span>
                                </td>
                                <td className="px-2 py-1.5 text-right text-gray-600 dark:text-gray-400 tabular-nums">
                                    {/* Null is not zero: a folder, an S3 delete marker and a
                                        FileLu row have no size to show, and sortTrashRows
                                        already treats them that way. */}
                                    {row.isDir || row.size == null ? '-' : formatSize(row.size)}
                                </td>
                                <td className="px-2 py-1.5 whitespace-nowrap text-gray-500 dark:text-gray-500 tabular-nums">
                                    {row.deletedAtLabel ?? (row.deletedAt ? formatDate(row.deletedAt) : '-')}
                                </td>
                                {extraColumns.map((col) => (
                                    <td key={col.key} className={`px-2 py-1.5 ${col.className ?? ''}`}>
                                        {col.render(row)}
                                    </td>
                                ))}
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
