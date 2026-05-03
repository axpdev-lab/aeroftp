// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useMemo, useState, useCallback } from 'react';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';

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
export type MyServersSort = { colId: MyServersSortableColId; dir: MyServersSortDir };
export type MyServersColumnVisibility = Record<MyServersTableColId, boolean>;

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

const COLUMN_IDS = MY_SERVERS_TABLE_COLUMNS.map(col => col.id);
const SORTABLE_IDS = new Set<MyServersTableColId>(
    MY_SERVERS_TABLE_COLUMNS.filter(col => col.sortable).map(col => col.id),
);

const VAULT_ACCOUNT = 'app_settings';
const VAULT_KEY = 'aeroftp_settings';
const SETTINGS_GROUP = 'ui_settings';
const SETTINGS_FIELD = 'my_servers_table';
const EVENT_NAME = 'aeroftp-settings-changed';

const defaultVisibility = (cardLayout: 'compact' | 'detailed'): MyServersColumnVisibility => ({
    index: true,
    icon: true,
    name: true,
    badges: true,
    subtitle: true,
    used: true,
    total: true,
    pct: true,
    paths: false,
    time: true,
    health: cardLayout === 'detailed',
    actions: true,
    favorite: true,
});

const sanitizeSort = (raw: unknown): MyServersSort | null => {
    if (!raw || typeof raw !== 'object') return null;
    const obj = raw as Record<string, unknown>;
    const colId = obj.colId;
    const dir = obj.dir;
    if (typeof colId !== 'string' || !SORTABLE_IDS.has(colId as MyServersTableColId)) return null;
    if (dir !== 'asc' && dir !== 'desc') return null;
    return { colId: colId as MyServersSortableColId, dir };
};

const sanitizeSettings = (
    raw: unknown,
    cardLayout: 'compact' | 'detailed',
): { visibility: MyServersColumnVisibility; sort: MyServersSort | null } => {
    const fallback = defaultVisibility(cardLayout);
    const obj = raw && typeof raw === 'object' ? raw as Record<string, unknown> : {};
    const rawVisibility = obj.visibility && typeof obj.visibility === 'object'
        ? obj.visibility as Record<string, unknown>
        : {};
    const visibility = { ...fallback };
    for (const id of COLUMN_IDS) {
        if (typeof rawVisibility[id] === 'boolean') {
            visibility[id] = rawVisibility[id];
        }
    }
    return { visibility, sort: sanitizeSort(obj.sort) };
};

const readSettingsBlob = (blob: Record<string, unknown> | null | undefined): unknown => {
    const uiSettings = blob?.[SETTINGS_GROUP];
    if (uiSettings && typeof uiSettings === 'object') {
        const nested = (uiSettings as Record<string, unknown>)[SETTINGS_FIELD];
        if (nested !== undefined) return nested;
    }
    return blob?.[SETTINGS_FIELD];
};

export const useMyServersColumns = (
    cardLayout: 'compact' | 'detailed',
): {
    visibility: MyServersColumnVisibility;
    sort: MyServersSort | null;
    setColVisible: (colId: MyServersTableColId, visible: boolean) => void;
    setSort: (sort: MyServersSort | null) => void;
} => {
    const [settings, setLocal] = useState(() => sanitizeSettings(null, cardLayout));
    const [hasPersistedSettings, setHasPersistedSettings] = useState(false);

    useEffect(() => {
        if (!hasPersistedSettings) {
            setLocal(prev => ({
                ...prev,
                visibility: { ...prev.visibility, health: cardLayout === 'detailed' },
            }));
        }
    }, [cardLayout, hasPersistedSettings]);

    useEffect(() => {
        let cancelled = false;
        (async () => {
            try {
                const blob = await secureGetWithFallback<Record<string, unknown>>(
                    VAULT_ACCOUNT,
                    VAULT_KEY,
                );
                const rawSettings = readSettingsBlob(blob);
                if (!cancelled && rawSettings !== undefined) {
                    setHasPersistedSettings(true);
                    setLocal(sanitizeSettings(rawSettings, cardLayout));
                }
            } catch {
                /* defaults already applied */
            }
        })();
        const onChanged = (e: Event) => {
            const detail = (e as CustomEvent<Record<string, unknown> | null>).detail;
            const rawSettings = readSettingsBlob(detail);
            if (rawSettings !== undefined) {
                setHasPersistedSettings(true);
                setLocal(sanitizeSettings(rawSettings, cardLayout));
            }
        };
        window.addEventListener(EVENT_NAME, onChanged);
        return () => {
            cancelled = true;
            window.removeEventListener(EVENT_NAME, onChanged);
        };
    }, [cardLayout]);

    const persist = useCallback((next: { visibility: MyServersColumnVisibility; sort: MyServersSort | null }) => {
        setLocal(next);
        setHasPersistedSettings(true);
        (async () => {
            try {
                const existing = await secureGetWithFallback<Record<string, unknown>>(
                    VAULT_ACCOUNT,
                    VAULT_KEY,
                );
                const base = { ...(existing || {}) };
                delete base[SETTINGS_FIELD];
                const uiSettings = base[SETTINGS_GROUP] && typeof base[SETTINGS_GROUP] === 'object'
                    ? base[SETTINGS_GROUP] as Record<string, unknown>
                    : {};
                const updated = {
                    ...base,
                    [SETTINGS_GROUP]: { ...uiSettings, [SETTINGS_FIELD]: next },
                };
                await secureStoreAndClean(VAULT_ACCOUNT, VAULT_KEY, updated);
                window.dispatchEvent(new CustomEvent(EVENT_NAME, { detail: updated }));
            } catch {
                /* best-effort */
            }
        })();
    }, []);

    const setColVisible = useCallback((colId: MyServersTableColId, visible: boolean) => {
        persist({
            ...settings,
            visibility: { ...settings.visibility, [colId]: visible },
        });
    }, [persist, settings]);

    const setSort = useCallback((sort: MyServersSort | null) => {
        persist({ ...settings, sort });
    }, [persist, settings]);

    return useMemo(() => ({
        visibility: settings.visibility,
        sort: settings.sort,
        setColVisible,
        setSort,
    }), [settings.visibility, settings.sort, setColVisible, setSort]);
};
