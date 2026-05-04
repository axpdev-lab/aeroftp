// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { useMemo } from 'react';
import {
    type TableColumnDef,
    type TableSort,
    useTableColumns,
    type UseTableColumnsResult,
} from './useTableColumns';

export type MyServersTableColId =
    | 'index'
    | 'icon'
    | 'name'
    | 'badges'
    | 'subtitle'
    | 'used'
    | 'total'
    | 'pct'
    | 'paths'
    | 'time'
    | 'health'
    | 'actions'
    | 'favorite';

export type MyServersSortableColId = 'index' | 'name' | 'badges' | 'used' | 'total' | 'pct' | 'time' | 'favorite';
export type MyServersSortDir = 'asc' | 'desc';
export type MyServersSort = TableSort<MyServersSortableColId>;
export type MyServersColumnVisibility = Record<MyServersTableColId, boolean>;

/** Legacy registry shape used by MyServersTable JSX (className helpers). */
export interface MyServersTableColumn {
    id: MyServersTableColId;
    labelKey: string;
    sortable: boolean;
    className: string;
    headerClassName?: string;
}

export const MY_SERVERS_TABLE_COLUMNS: MyServersTableColumn[] = [
    { id: 'index', labelKey: 'introHub.table.columns.index', sortable: true, className: 'w-16 text-right' },
    { id: 'icon', labelKey: 'introHub.table.columns.icon', sortable: false, className: 'w-14 text-center' },
    { id: 'name', labelKey: 'introHub.table.columns.name', sortable: true, className: 'min-w-[160px]' },
    { id: 'badges', labelKey: 'introHub.table.columns.badges', sortable: true, className: 'min-w-[120px]' },
    { id: 'subtitle', labelKey: 'introHub.table.columns.subtitle', sortable: false, className: 'min-w-[220px]' },
    { id: 'used', labelKey: 'introHub.table.columns.used', sortable: true, className: 'w-24 text-right tabular-nums' },
    { id: 'total', labelKey: 'introHub.table.columns.total', sortable: true, className: 'w-24 text-right tabular-nums' },
    { id: 'pct', labelKey: 'introHub.table.columns.pct', sortable: true, className: 'w-20 text-right tabular-nums' },
    { id: 'paths', labelKey: 'introHub.table.columns.paths', sortable: false, className: 'min-w-[220px] text-right' },
    { id: 'time', labelKey: 'introHub.table.columns.time', sortable: true, className: 'w-24 text-right tabular-nums' },
    { id: 'health', labelKey: 'introHub.table.columns.health', sortable: false, className: 'w-20 text-center' },
    { id: 'actions', labelKey: 'introHub.table.columns.actions', sortable: false, className: 'w-28 text-right' },
    { id: 'favorite', labelKey: 'introHub.table.columns.favorite', sortable: true, className: 'w-14 text-center' },
];

const SORTABLE_IDS: MyServersSortableColId[] = MY_SERVERS_TABLE_COLUMNS
    .filter(col => col.sortable)
    .map(col => col.id as MyServersSortableColId);

/**
 * Generic-shaped registry consumed by useTableColumns. Mirrors
 * MY_SERVERS_TABLE_COLUMNS but adds defaultVisible/defaultWidth/min/pinned.
 */
const TABLE_COLUMN_DEFS: TableColumnDef<MyServersTableColId>[] = [
    { id: 'index', labelKey: 'introHub.table.columns.index', sortable: true, defaultVisible: true, defaultWidth: 56, minWidth: 40, pinnedStart: true, className: 'w-16 text-right' },
    { id: 'icon', labelKey: 'introHub.table.columns.icon', sortable: false, defaultVisible: true, defaultWidth: 56, minWidth: 48, pinnedStart: true, className: 'w-14 text-center' },
    { id: 'name', labelKey: 'introHub.table.columns.name', sortable: true, defaultVisible: true, defaultWidth: 200, minWidth: 120 },
    { id: 'badges', labelKey: 'introHub.table.columns.badges', sortable: true, defaultVisible: true, defaultWidth: 140, minWidth: 100 },
    { id: 'subtitle', labelKey: 'introHub.table.columns.subtitle', sortable: false, defaultVisible: true, defaultWidth: 240, minWidth: 140 },
    { id: 'used', labelKey: 'introHub.table.columns.used', sortable: true, defaultVisible: true, defaultWidth: 96, minWidth: 80 },
    { id: 'total', labelKey: 'introHub.table.columns.total', sortable: true, defaultVisible: true, defaultWidth: 96, minWidth: 80 },
    { id: 'pct', labelKey: 'introHub.table.columns.pct', sortable: true, defaultVisible: true, defaultWidth: 80, minWidth: 60 },
    { id: 'paths', labelKey: 'introHub.table.columns.paths', sortable: false, defaultVisible: false, defaultWidth: 240, minWidth: 140 },
    { id: 'time', labelKey: 'introHub.table.columns.time', sortable: true, defaultVisible: true, defaultWidth: 96, minWidth: 80 },
    { id: 'health', labelKey: 'introHub.table.columns.health', sortable: false, defaultVisible: false, defaultWidth: 80, minWidth: 60, detailedOnly: true },
    { id: 'actions', labelKey: 'introHub.table.columns.actions', sortable: false, defaultVisible: true, defaultWidth: 112, minWidth: 96, pinnedEnd: true },
    { id: 'favorite', labelKey: 'introHub.table.columns.favorite', sortable: true, defaultVisible: true, defaultWidth: 56, minWidth: 48, pinnedEnd: true },
];

export const MY_SERVERS_TABLE_DEFS = TABLE_COLUMN_DEFS;

export type MyServersTableColumnsResult = UseTableColumnsResult<MyServersTableColId> & {
    /** Convenience accessors kept for back-compat with existing consumers. */
    visibility: MyServersColumnVisibility;
    sort: MyServersSort | null;
    setColVisible: (id: MyServersTableColId, visible: boolean) => void;
};

export function useMyServersColumns(cardLayout: 'compact' | 'detailed'): MyServersTableColumnsResult {
    const overrideDefaultVisibility = useMemo(
        () => (id: MyServersTableColId, def: TableColumnDef<MyServersTableColId>) => {
            if (def.detailedOnly) return cardLayout === 'detailed' && def.defaultVisible;
            return def.defaultVisible;
        },
        [cardLayout],
    );

    const result = useTableColumns<MyServersTableColId>({
        columns: TABLE_COLUMN_DEFS,
        storageKey: 'my_servers_table',
        overrideDefaultVisibility,
        sortableColIds: SORTABLE_IDS,
    });

    return useMemo<MyServersTableColumnsResult>(() => ({
        ...result,
        visibility: result.config.visibility,
        sort: result.config.sort as MyServersSort | null,
        setColVisible: result.setVisible,
    }), [result]);
}
