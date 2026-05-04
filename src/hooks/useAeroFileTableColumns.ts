// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { useMemo } from 'react';
import {
    type TableColumnDef,
    type TableSort,
    type UseTableColumnsResult,
    useTableColumns,
} from './useTableColumns';

export type AeroFileLocalColId = 'name' | 'size' | 'type' | 'modified';
export type AeroFileRemoteColId = AeroFileLocalColId | 'permissions';
export type AeroFileSortableColId = 'name' | 'size' | 'type' | 'modified';

export type AeroFileSort = TableSort<AeroFileSortableColId>;

const LOCAL_COLUMNS: TableColumnDef<AeroFileLocalColId>[] = [
    { id: 'name', labelKey: 'browser.name', sortable: true, defaultVisible: true, defaultWidth: 320, minWidth: 140, pinnedStart: true },
    { id: 'size', labelKey: 'browser.size', sortable: true, defaultVisible: true, defaultWidth: 96, minWidth: 70 },
    { id: 'type', labelKey: 'browser.type', sortable: true, defaultVisible: false, defaultWidth: 96, minWidth: 70 },
    { id: 'modified', labelKey: 'browser.modified', sortable: true, defaultVisible: true, defaultWidth: 160, minWidth: 110 },
];

const REMOTE_COLUMNS: TableColumnDef<AeroFileRemoteColId>[] = [
    { id: 'name', labelKey: 'browser.name', sortable: true, defaultVisible: true, defaultWidth: 320, minWidth: 140, pinnedStart: true },
    { id: 'size', labelKey: 'browser.size', sortable: true, defaultVisible: true, defaultWidth: 96, minWidth: 70 },
    { id: 'type', labelKey: 'browser.type', sortable: true, defaultVisible: false, defaultWidth: 96, minWidth: 70 },
    { id: 'permissions', labelKey: 'browser.permsHeader', sortable: false, defaultVisible: false, defaultWidth: 110, minWidth: 80 },
    { id: 'modified', labelKey: 'browser.modified', sortable: true, defaultVisible: true, defaultWidth: 160, minWidth: 110 },
];

const SORTABLE_LOCAL: AeroFileSortableColId[] = ['name', 'size', 'type', 'modified'];
const SORTABLE_REMOTE: AeroFileSortableColId[] = ['name', 'size', 'type', 'modified'];

export const AERO_FILE_LOCAL_COLUMNS = LOCAL_COLUMNS;
export const AERO_FILE_REMOTE_COLUMNS = REMOTE_COLUMNS;

export type AeroFileLocalTableColumns = UseTableColumnsResult<AeroFileLocalColId>;
export type AeroFileRemoteTableColumns = UseTableColumnsResult<AeroFileRemoteColId>;

export function useAeroFileLocalColumns(): AeroFileLocalTableColumns {
    return useTableColumns<AeroFileLocalColId>(useMemo(() => ({
        columns: LOCAL_COLUMNS,
        storageKey: 'aero_file_local_table',
        sortableColIds: SORTABLE_LOCAL as AeroFileLocalColId[],
    }), []));
}

export function useAeroFileRemoteColumns(): AeroFileRemoteTableColumns {
    return useTableColumns<AeroFileRemoteColId>(useMemo(() => ({
        columns: REMOTE_COLUMNS,
        storageKey: 'aero_file_remote_table',
        sortableColIds: SORTABLE_REMOTE as AeroFileRemoteColId[],
    }), []));
}
