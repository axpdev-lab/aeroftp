// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useState, useCallback } from 'react';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';

export type MyServersDensity = 'compact' | 'comfortable';

export const DEFAULT_DENSITY: MyServersDensity = 'compact';

const VAULT_ACCOUNT = 'app_settings';
const VAULT_KEY = 'aeroftp_settings';
const SETTINGS_FIELD = 'my_servers_density';
const EVENT_NAME = 'aeroftp-settings-changed';
const LS_KEY = 'aeroftp-my-servers-density';

const sanitize = (raw: unknown): MyServersDensity =>
    raw === 'comfortable' ? 'comfortable' : 'compact';

/**
 * Density toggle for the My Servers list view. Mirrors the storage-thresholds
 * hook: vault for cross-device persistence, localStorage for synchronous first
 * paint, `aeroftp-settings-changed` for cross-component sync.
 */
export const useMyServersDensity = (): {
    density: MyServersDensity;
    setDensity: (next: MyServersDensity) => void;
} => {
    const [density, setLocal] = useState<MyServersDensity>(() => {
        try {
            const ls = localStorage.getItem(LS_KEY);
            if (ls) return sanitize(ls);
        } catch { /* fall through */ }
        return DEFAULT_DENSITY;
    });

    useEffect(() => {
        let cancelled = false;
        (async () => {
            try {
                const blob = await secureGetWithFallback<Record<string, unknown>>(
                    VAULT_ACCOUNT,
                    VAULT_KEY,
                );
                if (!cancelled && blob && blob[SETTINGS_FIELD] !== undefined) {
                    setLocal(sanitize(blob[SETTINGS_FIELD]));
                }
            } catch { /* default already applied */ }
        })();
        const onChanged = (e: Event) => {
            const detail = (e as CustomEvent<Record<string, unknown> | null>).detail;
            if (detail && detail[SETTINGS_FIELD] !== undefined) {
                setLocal(sanitize(detail[SETTINGS_FIELD]));
            }
        };
        window.addEventListener(EVENT_NAME, onChanged);
        return () => {
            cancelled = true;
            window.removeEventListener(EVENT_NAME, onChanged);
        };
    }, []);

    const setDensity = useCallback((next: MyServersDensity) => {
        const sanitized = sanitize(next);
        setLocal(sanitized);
        try { localStorage.setItem(LS_KEY, sanitized); } catch { /* best-effort */ }
        (async () => {
            try {
                const existing = await secureGetWithFallback<Record<string, unknown>>(
                    VAULT_ACCOUNT,
                    VAULT_KEY,
                );
                const updated = { ...(existing || {}), [SETTINGS_FIELD]: sanitized };
                await secureStoreAndClean(VAULT_ACCOUNT, VAULT_KEY, updated);
                window.dispatchEvent(new CustomEvent(EVENT_NAME, { detail: updated }));
            } catch { /* best-effort */ }
        })();
    }, []);

    return { density, setDensity };
};
