// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useState, useCallback } from 'react';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';

export interface StorageThresholds {
    /** Percent of total ≥ this value renders amber. Default 80. */
    warn: number;
    /** Percent of total ≥ this value renders red. Default 95. */
    critical: number;
}

export const DEFAULT_THRESHOLDS: StorageThresholds = { warn: 80, critical: 95 };

const VAULT_ACCOUNT = 'app_settings';
const VAULT_KEY = 'aeroftp_settings';
const SETTINGS_FIELD = 'storage_thresholds';
const EVENT_NAME = 'aeroftp-settings-changed';

const clamp = (n: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, n));

export const sanitizeThresholds = (raw: unknown): StorageThresholds => {
    const obj = (raw && typeof raw === 'object') ? raw as Record<string, unknown> : {};
    const warn = clamp(Math.round(Number(obj.warn ?? DEFAULT_THRESHOLDS.warn)), 5, 99);
    const critRaw = clamp(Math.round(Number(obj.critical ?? DEFAULT_THRESHOLDS.critical)), 5, 100);
    // Critical must be strictly greater than warn so the two zones don't collapse.
    const critical = critRaw > warn ? critRaw : Math.min(100, warn + 1);
    return { warn, critical };
};

export type StorageTone = 'unknown' | 'low' | 'ok' | 'warn' | 'critical';

export const getStorageTone = (
    used: number | undefined,
    total: number | undefined,
    thresholds: StorageThresholds,
): { tone: StorageTone; pct: number | null } => {
    if (!total || total <= 0 || typeof used !== 'number') {
        return { tone: 'unknown', pct: null };
    }
    const pct = (used / total) * 100;
    if (pct > 100) return { tone: 'critical', pct };
    if (pct < 5) return { tone: 'low', pct };
    if (pct >= thresholds.critical) return { tone: 'critical', pct };
    if (pct >= thresholds.warn) return { tone: 'warn', pct };
    return { tone: 'ok', pct };
};

export const TONE_BG_CLASS: Record<StorageTone, string> = {
    unknown: 'bg-gray-300 dark:bg-gray-600',
    low: 'bg-gray-400 dark:bg-gray-500',
    ok: 'bg-emerald-500',
    warn: 'bg-amber-500',
    critical: 'bg-red-500',
};

export const TONE_TEXT_CLASS: Record<StorageTone, string> = {
    unknown: 'text-gray-400 dark:text-gray-500',
    low: 'text-gray-500 dark:text-gray-400',
    ok: 'text-emerald-600 dark:text-emerald-400',
    warn: 'text-amber-600 dark:text-amber-400',
    critical: 'text-red-600 dark:text-red-400',
};

/**
 * Live storage thresholds, persisted to vault under `storage_thresholds` inside
 * the global `aeroftp_settings` blob. Mirrors the pattern used by the existing
 * font-size shortcuts hook so the same `aeroftp-settings-changed` event drives
 * cross-component sync.
 */
export const useStorageThresholds = (): {
    thresholds: StorageThresholds;
    setThresholds: (next: StorageThresholds) => void;
} => {
    const [thresholds, setLocal] = useState<StorageThresholds>(DEFAULT_THRESHOLDS);

    useEffect(() => {
        let cancelled = false;
        (async () => {
            try {
                const blob = await secureGetWithFallback<Record<string, unknown>>(
                    VAULT_ACCOUNT,
                    VAULT_KEY,
                );
                if (!cancelled && blob && blob[SETTINGS_FIELD]) {
                    setLocal(sanitizeThresholds(blob[SETTINGS_FIELD]));
                }
            } catch {
                /* default already applied */
            }
        })();
        const onChanged = (e: Event) => {
            const detail = (e as CustomEvent<Record<string, unknown> | null>).detail;
            if (detail && detail[SETTINGS_FIELD]) {
                setLocal(sanitizeThresholds(detail[SETTINGS_FIELD]));
            }
        };
        window.addEventListener(EVENT_NAME, onChanged);
        return () => {
            cancelled = true;
            window.removeEventListener(EVENT_NAME, onChanged);
        };
    }, []);

    const setThresholds = useCallback((next: StorageThresholds) => {
        const sanitized = sanitizeThresholds(next);
        setLocal(sanitized);
        (async () => {
            try {
                const existing = await secureGetWithFallback<Record<string, unknown>>(
                    VAULT_ACCOUNT,
                    VAULT_KEY,
                );
                const updated = { ...(existing || {}), [SETTINGS_FIELD]: sanitized };
                await secureStoreAndClean(VAULT_ACCOUNT, VAULT_KEY, updated);
                window.dispatchEvent(new CustomEvent(EVENT_NAME, { detail: updated }));
            } catch {
                /* best-effort */
            }
        })();
    }, []);

    return { thresholds, setThresholds };
};
