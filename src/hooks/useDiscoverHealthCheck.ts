// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useRef, useState } from 'react';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';

const SETTINGS_KEY = 'aeroftp_settings';
const SETTINGS_VAULT_KEY = 'app_settings';

/**
 * Reader + writer for the "health check on Add Services" preference, the same
 * `discoverHealthCheck` field SettingsPanel persists. Mirrors useIntroHubIconSize:
 * it reads the vault on mount and re-reads on `aeroftp-settings-changed`, so the
 * Settings checkbox and the double-click toggle on the Discover Check button stay
 * in sync. The setter writes the merged settings blob back and dispatches the
 * event so every other listener (Settings panel, App-level useSettings) updates.
 *
 * Default ON (true): absent/legacy settings keep the original eager-probe behaviour.
 */
export function useDiscoverHealthCheck(): [boolean, (value: boolean) => void] {
    const [enabled, setEnabledState] = useState(true);
    const mountedRef = useRef(true);

    useEffect(() => {
        mountedRef.current = true;
        let cancelled = false;
        (async () => {
            try {
                const parsed = await secureGetWithFallback<Record<string, unknown>>(SETTINGS_VAULT_KEY, SETTINGS_KEY);
                if (!cancelled && parsed && typeof parsed.discoverHealthCheck === 'boolean') {
                    setEnabledState(parsed.discoverHealthCheck);
                }
            } catch {
                /* keep default ON */
            }
        })();

        const onChange = (e: Event) => {
            const detail = (e as CustomEvent)?.detail as Record<string, unknown> | undefined;
            if (detail && typeof detail.discoverHealthCheck === 'boolean') {
                setEnabledState(detail.discoverHealthCheck);
            }
        };
        window.addEventListener('aeroftp-settings-changed', onChange);
        return () => {
            cancelled = true;
            mountedRef.current = false;
            window.removeEventListener('aeroftp-settings-changed', onChange);
        };
    }, []);

    const setEnabled = useCallback((value: boolean) => {
        setEnabledState(value);
        (async () => {
            try {
                const existing = (await secureGetWithFallback<Record<string, unknown>>(SETTINGS_VAULT_KEY, SETTINGS_KEY)) || {};
                const updated = { ...existing, discoverHealthCheck: value };
                await secureStoreAndClean(SETTINGS_VAULT_KEY, SETTINGS_KEY, updated);
                window.dispatchEvent(new CustomEvent('aeroftp-settings-changed', { detail: updated }));
            } catch {
                /* best-effort: local state already reflects the choice */
            }
        })();
    }, []);

    return [enabled, setEnabled];
}

export default useDiscoverHealthCheck;
