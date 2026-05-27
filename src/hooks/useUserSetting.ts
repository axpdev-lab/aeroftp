// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// MU-4 generic hook: read/write a per-user setting scope through the
// user_partitions IPC. Each call to `setValue` writes to the active user's
// partition; switching user via UserDropdown fires a PROFILES_CHANGED_EVENT
// and the hook re-reads to keep the panel in sync.

import * as React from 'react';
import {
    deleteActiveUserSetting,
    getActiveUserSetting,
    setActiveUserSetting,
} from '../utils/userPartitions';
import { PROFILES_CHANGED_EVENT } from '../utils/serverProfileStore';

interface UseUserSettingResult<T> {
    value: T | null;
    loading: boolean;
    error: string | null;
    setValue: (next: T) => Promise<void>;
    clearValue: () => Promise<void>;
    refresh: () => Promise<void>;
}

export function useUserSetting<T = unknown>(
    scope: string,
    defaultValue?: T | null,
): UseUserSettingResult<T> {
    const [value, setValueState] = React.useState<T | null>(defaultValue ?? null);
    const [loading, setLoading] = React.useState(true);
    const [error, setError] = React.useState<string | null>(null);
    const mountedRef = React.useRef(true);

    const refresh = React.useCallback(async () => {
        setLoading(true);
        setError(null);
        try {
            const next = await getActiveUserSetting<T>(scope);
            if (mountedRef.current) setValueState(next ?? defaultValue ?? null);
        } catch (err) {
            if (mountedRef.current) setError(String(err));
        } finally {
            if (mountedRef.current) setLoading(false);
        }
    }, [scope, defaultValue]);

    React.useEffect(() => {
        mountedRef.current = true;
        void refresh();
        const handler = () => { void refresh(); };
        window.addEventListener(PROFILES_CHANGED_EVENT, handler);
        return () => {
            mountedRef.current = false;
            window.removeEventListener(PROFILES_CHANGED_EVENT, handler);
        };
    }, [refresh]);

    const setValue = React.useCallback(async (next: T) => {
        setError(null);
        try {
            await setActiveUserSetting<T>(scope, next);
            if (mountedRef.current) setValueState(next);
        } catch (err) {
            if (mountedRef.current) setError(String(err));
            throw err;
        }
    }, [scope]);

    const clearValue = React.useCallback(async () => {
        setError(null);
        try {
            await deleteActiveUserSetting(scope);
            if (mountedRef.current) setValueState(defaultValue ?? null);
        } catch (err) {
            if (mountedRef.current) setError(String(err));
            throw err;
        }
    }, [scope, defaultValue]);

    return { value, loading, error, setValue, clearValue, refresh };
}
