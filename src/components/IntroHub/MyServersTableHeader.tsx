import * as React from 'react';
import { ArrowDown, ArrowUp, Settings2 } from 'lucide-react';
import { useTranslation } from '../../i18n';
import {
    MY_SERVERS_TABLE_COLUMNS,
    type MyServersColumnVisibility,
    type MyServersSort,
    type MyServersSortableColId,
    type MyServersTableColId,
    type MyServersTableColumn,
} from '../../hooks/useMyServersColumns';

interface MyServersTableHeaderProps {
    visibility: MyServersColumnVisibility;
    sort: MyServersSort | null;
    onSort: (sort: MyServersSort | null) => void;
    onVisibleChange: (colId: MyServersTableColId, visible: boolean) => void;
}

const nextSortFor = (column: MyServersTableColumn, sort: MyServersSort | null): MyServersSort | null => {
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

export function MyServersTableHeader({
    visibility,
    sort,
    onSort,
    onVisibleChange,
}: MyServersTableHeaderProps) {
    const t = useTranslation();
    const visibleColumns = MY_SERVERS_TABLE_COLUMNS.filter(col => visibility[col.id]);
    const lastVisibleId = visibleColumns[visibleColumns.length - 1]?.id;
    const sortLabel = sort ? t(MY_SERVERS_TABLE_COLUMNS.find(col => col.id === sort.colId)?.labelKey || '') : '';

    return (
        <thead className="sticky top-0 z-20 bg-gray-50 dark:bg-gray-800 border-b border-gray-200 dark:border-gray-700 shadow-sm">
            <tr>
                {visibleColumns.map((column) => {
                    const isSorted = sort?.colId === column.id;
                    const label = t(column.labelKey);
                    const title = column.id === 'index'
                        ? sort === null
                            ? t('introHub.table.manualOrderActive')
                            : t('introHub.table.clickToReturnManual', { column: sortLabel })
                        : column.sortable
                            ? t('introHub.table.clickToSortBy', { column: label })
                            : label;
                    const ariaSort = isSorted
                        ? sort.dir === 'asc' ? 'ascending' : 'descending'
                        : undefined;
                    const content = (
                        <span className={`flex items-center gap-1 ${column.className.includes('text-right') ? 'justify-end' : column.className.includes('text-center') ? 'justify-center' : ''}`}>
                            <span>{column.id === 'index' ? '#' : label}</span>
                            {isSorted && (sort.dir === 'asc' ? <ArrowUp size={11} /> : <ArrowDown size={11} />)}
                        </span>
                    );

                    return (
                        <th
                            key={column.id}
                            scope="col"
                            aria-sort={ariaSort as React.AriaAttributes['aria-sort']}
                            className={`${column.className} relative px-3 py-2 text-[11px] font-semibold uppercase text-gray-500 dark:text-gray-400 tracking-wide whitespace-nowrap ${column.headerClassName || ''}`}
                        >
                            {column.sortable ? (
                                <button
                                    type="button"
                                    onClick={() => onSort(nextSortFor(column, sort))}
                                    className="w-full cursor-pointer hover:text-gray-800 dark:hover:text-gray-100 transition-colors"
                                    title={title}
                                >
                                    {content}
                                </button>
                            ) : (
                                <span title={title}>{content}</span>
                            )}
                            {column.id === lastVisibleId && (
                                <details className="absolute right-1 top-1/2 -translate-y-1/2">
                                    <summary
                                        className="list-none p-1 rounded-md cursor-pointer text-gray-400 hover:text-gray-700 hover:bg-gray-200 dark:hover:text-gray-200 dark:hover:bg-gray-700"
                                        title={t('introHub.table.columnSettings')}
                                        onClick={(e) => e.stopPropagation()}
                                    >
                                        <Settings2 size={13} />
                                    </summary>
                                    <div
                                        className="absolute right-0 mt-2 w-48 rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 shadow-xl p-2 normal-case tracking-normal text-left"
                                        onClick={(e) => e.stopPropagation()}
                                    >
                                        {MY_SERVERS_TABLE_COLUMNS.map((item) => (
                                            <label
                                                key={item.id}
                                                className="flex items-center gap-2 px-2 py-1.5 rounded-md hover:bg-gray-100 dark:hover:bg-gray-700 text-xs text-gray-700 dark:text-gray-200 cursor-pointer"
                                            >
                                                <input
                                                    type="checkbox"
                                                    checked={visibility[item.id]}
                                                    onChange={(e) => onVisibleChange(item.id, e.target.checked)}
                                                    className="rounded border-gray-300 dark:border-gray-600 text-blue-600 focus:ring-blue-500"
                                                />
                                                <span className="truncate">{item.id === 'index' ? '#' : t(item.labelKey)}</span>
                                            </label>
                                        ))}
                                    </div>
                                </details>
                            )}
                        </th>
                    );
                })}
            </tr>
        </thead>
    );
}
