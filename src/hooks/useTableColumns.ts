// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useMemo, useState } from 'react';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';

export interface TableColumnDef<TColId extends string> {
    id: TColId;
    labelKey: string;
    sortable: boolean;
    defaultVisible: boolean;
    defaultWidth: number;
    minWidth?: number;
    pinnedStart?: boolean;
    pinnedEnd?: boolean;
    /** Optional className applied to the rendered <th>. */
    className?: string;
    /** Optional className applied to body <td>. */
    bodyClassName?: string;
    /** Hidden until the table is wide enough for a "detailed" layout. */
    detailedOnly?: boolean;
}

export type TableSort<TColId extends string> = {
    colId: TColId;
    dir: 'asc' | 'desc';
};

export interface TableColumnsConfig<TColId extends string> {
    visibility: Record<TColId, boolean>;
    order: TColId[];
    widths: Record<TColId, number>;
    sort: TableSort<TColId> | null;
}

export interface UseTableColumnsResult<TColId extends string> {
    config: TableColumnsConfig<TColId>;
    /** Visible columns rendered in the user's effective order (pinned at the ends). */
    orderedVisibleColumns: TableColumnDef<TColId>[];
    /** All columns in effective order (regardless of visibility) for the manager popover. */
    orderedAllColumns: TableColumnDef<TColId>[];
    setVisible: (id: TColId, visible: boolean) => void;
    setOrder: (order: TColId[]) => void;
    setWidth: (id: TColId, width: number) => void;
    setSort: (sort: TableSort<TColId> | null) => void;
    reset: () => void;
    /** True when the user has saved at least one explicit value to the vault. */
    hasPersisted: boolean;
}

const VAULT_ACCOUNT = 'app_settings';
const VAULT_KEY = 'aeroftp_settings';
const SETTINGS_GROUP = 'ui_settings';
const EVENT_NAME = 'aeroftp-settings-changed';

const DEFAULT_MIN_WIDTH = 60;

export interface UseTableColumnsOpts<TColId extends string> {
    columns: TableColumnDef<TColId>[];
    /** Vault sub-key, e.g. 'my_servers_table'. */
    storageKey: string;
    /** Override defaultVisible per-column at runtime (e.g. compact vs detailed). */
    overrideDefaultVisibility?: (id: TColId, def: TableColumnDef<TColId>) => boolean;
    /** Restrict which sort col ids are accepted from the persisted blob. */
    sortableColIds?: TColId[];
}

const buildDefaults = <TColId extends string>(
    columns: TableColumnDef<TColId>[],
    overrideVisible: ((id: TColId, def: TableColumnDef<TColId>) => boolean) | undefined,
): TableColumnsConfig<TColId> => {
    const visibility = {} as Record<TColId, boolean>;
    const widths = {} as Record<TColId, number>;
    const order: TColId[] = [];
    for (const col of columns) {
        visibility[col.id] = overrideVisible ? overrideVisible(col.id, col) : col.defaultVisible;
        widths[col.id] = col.defaultWidth;
        order.push(col.id);
    }
    return { visibility, order, widths, sort: null };
};

const sanitizeOrder = <TColId extends string>(
    raw: unknown,
    knownIds: TColId[],
): TColId[] => {
    const known = new Set<string>(knownIds as readonly string[]);
    const result: TColId[] = [];
    if (Array.isArray(raw)) {
        for (const value of raw) {
            if (typeof value === 'string' && known.has(value) && !result.includes(value as TColId)) {
                result.push(value as TColId);
            }
        }
    }
    for (const id of knownIds) {
        if (!result.includes(id)) result.push(id);
    }
    return result;
};

const sanitizeVisibility = <TColId extends string>(
    raw: unknown,
    fallback: Record<TColId, boolean>,
): Record<TColId, boolean> => {
    const result = { ...fallback };
    if (raw && typeof raw === 'object') {
        const obj = raw as Record<string, unknown>;
        for (const id of Object.keys(fallback) as TColId[]) {
            if (typeof obj[id] === 'boolean') result[id] = obj[id] as boolean;
        }
    }
    return result;
};

const sanitizeWidths = <TColId extends string>(
    raw: unknown,
    columns: TableColumnDef<TColId>[],
    fallback: Record<TColId, number>,
): Record<TColId, number> => {
    const result = { ...fallback };
    if (raw && typeof raw === 'object') {
        const obj = raw as Record<string, unknown>;
        for (const col of columns) {
            const value = obj[col.id];
            if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
                const min = col.minWidth ?? DEFAULT_MIN_WIDTH;
                result[col.id] = Math.max(min, Math.floor(value));
            }
        }
    }
    return result;
};

const sanitizeSort = <TColId extends string>(
    raw: unknown,
    sortable: Set<TColId>,
): TableSort<TColId> | null => {
    if (!raw || typeof raw !== 'object') return null;
    const obj = raw as Record<string, unknown>;
    const colId = obj.colId;
    const dir = obj.dir;
    if (typeof colId !== 'string' || !sortable.has(colId as TColId)) return null;
    if (dir !== 'asc' && dir !== 'desc') return null;
    return { colId: colId as TColId, dir };
};

const sanitizeConfig = <TColId extends string>(
    raw: unknown,
    columns: TableColumnDef<TColId>[],
    overrideVisible: ((id: TColId, def: TableColumnDef<TColId>) => boolean) | undefined,
    sortableColIds: TColId[],
): TableColumnsConfig<TColId> => {
    const fallback = buildDefaults(columns, overrideVisible);
    const obj = raw && typeof raw === 'object' ? (raw as Record<string, unknown>) : {};
    const knownIds = columns.map(col => col.id);
    return {
        visibility: sanitizeVisibility(obj.visibility, fallback.visibility),
        order: sanitizeOrder(obj.order, knownIds),
        widths: sanitizeWidths(obj.widths, columns, fallback.widths),
        sort: sanitizeSort(obj.sort, new Set(sortableColIds)),
    };
};

const readSettingsBlob = (
    blob: Record<string, unknown> | null | undefined,
    storageKey: string,
): unknown => {
    if (!blob) return undefined;
    const uiSettings = blob[SETTINGS_GROUP];
    if (uiSettings && typeof uiSettings === 'object') {
        const nested = (uiSettings as Record<string, unknown>)[storageKey];
        if (nested !== undefined) return nested;
    }
    return blob[storageKey];
};

/**
 * Resolve the effective render order for visible/all columns:
 * pinnedStart first (registry order), pinnedEnd last (registry order),
 * the remaining ids follow `config.order`.
 */
const computeOrderedColumns = <TColId extends string>(
    columns: TableColumnDef<TColId>[],
    order: TColId[],
    visibility: Record<TColId, boolean> | null,
): TableColumnDef<TColId>[] => {
    const byId = new Map(columns.map(col => [col.id, col] as const));
    const pinnedStart = columns.filter(col => col.pinnedStart);
    const pinnedEnd = columns.filter(col => col.pinnedEnd);
    const middle: TableColumnDef<TColId>[] = [];
    const seen = new Set<TColId>();
    for (const id of order) {
        const col = byId.get(id);
        if (!col || col.pinnedStart || col.pinnedEnd) continue;
        if (seen.has(id)) continue;
        seen.add(id);
        middle.push(col);
    }
    for (const col of columns) {
        if (col.pinnedStart || col.pinnedEnd) continue;
        if (!seen.has(col.id)) {
            seen.add(col.id);
            middle.push(col);
        }
    }
    const ordered = [...pinnedStart, ...middle, ...pinnedEnd];
    if (!visibility) return ordered;
    return ordered.filter(col => visibility[col.id]);
};

export function useTableColumns<TColId extends string>(
    opts: UseTableColumnsOpts<TColId>,
): UseTableColumnsResult<TColId> {
    const { columns, storageKey, overrideDefaultVisibility, sortableColIds } = opts;
    const sortableIds = useMemo<TColId[]>(
        () => sortableColIds ?? columns.filter(col => col.sortable).map(col => col.id),
        [columns, sortableColIds],
    );

    const initialConfig = useMemo(
        () => buildDefaults(columns, overrideDefaultVisibility),
        // eslint-disable-next-line react-hooks/exhaustive-deps
        [],
    );
    const [config, setConfig] = useState<TableColumnsConfig<TColId>>(initialConfig);
    const [hasPersisted, setHasPersisted] = useState(false);

    // Re-apply override when the runtime hint changes (compact ↔ detailed)
    // but only as long as the user has not persisted their own value.
    useEffect(() => {
        if (hasPersisted || !overrideDefaultVisibility) return;
        setConfig(prev => {
            const nextVisibility = { ...prev.visibility };
            let mutated = false;
            for (const col of columns) {
                const want = overrideDefaultVisibility(col.id, col);
                if (nextVisibility[col.id] !== want) {
                    nextVisibility[col.id] = want;
                    mutated = true;
                }
            }
            return mutated ? { ...prev, visibility: nextVisibility } : prev;
        });
    }, [hasPersisted, overrideDefaultVisibility, columns]);

    useEffect(() => {
        let cancelled = false;
        (async () => {
            try {
                const blob = await secureGetWithFallback<Record<string, unknown>>(VAULT_ACCOUNT, VAULT_KEY);
                const rawSettings = readSettingsBlob(blob, storageKey);
                if (!cancelled && rawSettings !== undefined) {
                    setHasPersisted(true);
                    setConfig(sanitizeConfig(rawSettings, columns, overrideDefaultVisibility, sortableIds));
                }
            } catch {
                /* defaults already applied */
            }
        })();

        const onChanged = (e: Event) => {
            const detail = (e as CustomEvent<Record<string, unknown> | null>).detail;
            const rawSettings = readSettingsBlob(detail, storageKey);
            if (rawSettings !== undefined) {
                setHasPersisted(true);
                setConfig(sanitizeConfig(rawSettings, columns, overrideDefaultVisibility, sortableIds));
            }
        };
        window.addEventListener(EVENT_NAME, onChanged);
        return () => {
            cancelled = true;
            window.removeEventListener(EVENT_NAME, onChanged);
        };
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [storageKey]);

    const persist = useCallback((next: TableColumnsConfig<TColId>) => {
        setConfig(next);
        setHasPersisted(true);
        (async () => {
            try {
                const existing = await secureGetWithFallback<Record<string, unknown>>(VAULT_ACCOUNT, VAULT_KEY);
                const base = { ...(existing || {}) };
                // Drop legacy top-level placement (Phase 2 Step 8 back-compat)
                delete base[storageKey];
                const uiSettings = base[SETTINGS_GROUP] && typeof base[SETTINGS_GROUP] === 'object'
                    ? (base[SETTINGS_GROUP] as Record<string, unknown>)
                    : {};
                const updated = {
                    ...base,
                    [SETTINGS_GROUP]: { ...uiSettings, [storageKey]: next },
                };
                await secureStoreAndClean(VAULT_ACCOUNT, VAULT_KEY, updated);
                window.dispatchEvent(new CustomEvent(EVENT_NAME, { detail: updated }));
            } catch {
                /* best-effort */
            }
        })();
    }, [storageKey]);

    const setVisible = useCallback((id: TColId, visible: boolean) => {
        setConfig(prev => {
            const next = { ...prev, visibility: { ...prev.visibility, [id]: visible } };
            void persist(next);
            return next;
        });
    }, [persist]);

    const setOrder = useCallback((order: TColId[]) => {
        const knownIds = columns.map(col => col.id);
        const sanitized = sanitizeOrder<TColId>(order, knownIds);
        setConfig(prev => {
            const next = { ...prev, order: sanitized };
            void persist(next);
            return next;
        });
    }, [columns, persist]);

    const setWidth = useCallback((id: TColId, width: number) => {
        const col = columns.find(c => c.id === id);
        const min = col?.minWidth ?? DEFAULT_MIN_WIDTH;
        const clamped = Math.max(min, Math.floor(width));
        setConfig(prev => {
            if (prev.widths[id] === clamped) return prev;
            const next = { ...prev, widths: { ...prev.widths, [id]: clamped } };
            void persist(next);
            return next;
        });
    }, [columns, persist]);

    const setSort = useCallback((sort: TableSort<TColId> | null) => {
        const sortable = new Set(sortableIds);
        const sanitized = sort && sortable.has(sort.colId) ? sort : null;
        setConfig(prev => {
            const next = { ...prev, sort: sanitized };
            void persist(next);
            return next;
        });
    }, [persist, sortableIds]);

    const reset = useCallback(() => {
        const defaults = buildDefaults(columns, overrideDefaultVisibility);
        setConfig(defaults);
        setHasPersisted(true);
        void persist(defaults);
    }, [columns, overrideDefaultVisibility, persist]);

    const orderedVisibleColumns = useMemo(
        () => computeOrderedColumns(columns, config.order, config.visibility),
        [columns, config.order, config.visibility],
    );
    const orderedAllColumns = useMemo(
        () => computeOrderedColumns(columns, config.order, null),
        [columns, config.order],
    );

    return useMemo(() => ({
        config,
        orderedVisibleColumns,
        orderedAllColumns,
        setVisible,
        setOrder,
        setWidth,
        setSort,
        reset,
        hasPersisted,
    }), [
        config,
        orderedVisibleColumns,
        orderedAllColumns,
        setVisible,
        setOrder,
        setWidth,
        setSort,
        reset,
        hasPersisted,
    ]);
}

export const __TEST_ONLY__ = {
    sanitizeConfig,
    sanitizeOrder,
    sanitizeWidths,
    sanitizeVisibility,
    sanitizeSort,
    computeOrderedColumns,
    buildDefaults,
};
