// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useState } from 'react';
import { onVaultReady, secureGetWithFallback } from '../utils/secureStorage';
import { APP_SETTINGS_EVENT, updateAppSettings } from '../utils/appSettings';

const ACCOUNT = 'app_settings';
const CACHE_KEY = 'aeroftp_settings';
const EVENT = APP_SETTINGS_EVENT;

type Settings = Record<string, unknown>;

const readBreakdown = (blob: Settings | null): boolean => {
    const ui = blob?.ui_settings as Settings | undefined;
    return ui?.my_servers_breakdown === true;
};

/** The CLI stores this same boolean in config_app_settings.ui_settings. */
export function useMyServersBreakdown(refreshKey?: number) {
    const [breakdown, setLocal] = useState(false);

    useEffect(() => {
        let cancelled = false;
        let loadVersion = 0;
        const load = async () => {
            const version = ++loadVersion;
            const blob = await secureGetWithFallback<Settings>(ACCOUNT, CACHE_KEY);
            if (!cancelled && version === loadVersion) setLocal(readBreakdown(blob));
        };
        void load();
        const onFocus = () => { void load(); };
        const onChanged = (event: Event) => {
            ++loadVersion;
            setLocal(readBreakdown((event as CustomEvent<Settings | null>).detail));
        };
        window.addEventListener('focus', onFocus);
        window.addEventListener(EVENT, onChanged);
        const offVaultReady = refreshKey === undefined ? onVaultReady(onFocus) : undefined;
        return () => {
            cancelled = true;
            window.removeEventListener('focus', onFocus);
            window.removeEventListener(EVENT, onChanged);
            offVaultReady?.();
        };
    }, [refreshKey]);

    const setBreakdown = useCallback(async (next: boolean) => {
        try {
            await updateAppSettings(existing => {
                const ui = existing?.ui_settings;
                return {
                    ...(existing || {}),
                    ui_settings: {
                        ...(ui && typeof ui === 'object' ? ui : {}),
                        my_servers_breakdown: next,
                    },
                };
            });
        } catch {
            // Keep the last confirmed choice when the vault write fails.
        }
    }, []);

    return { breakdown, setBreakdown };
}
