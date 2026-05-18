// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Whitelist of `localStorage` keys included in a full keystore export.
 *
 * Rationale. Not every key in `window.localStorage` is worth persisting
 * across machines: framework/Vite scratch, transient UI animation
 * flags, debug toggles, and migration markers should reset on import,
 * not be carried forward. This list pins the keys whose value is part
 * of the user's identity on AeroFTP -- preferences they chose, layouts
 * they configured, tabs they had open -- and nothing else.
 *
 * Adding a key here is an explicit decision: backups become slightly
 * larger and any breaking schema change to the value format has to
 * support upgrade-on-import. Removing a key here means previously
 * exported backups still carry it but `applyLocalStorage` will drop
 * it on the import side.
 */
// NOTE: every entry below is the exact key the app actually writes.
// The earlier list carried several wrong names (underscore vs hyphen,
// `aeroftp_` vs `aerofile_` prefix), so collectLocalStorage() never
// matched them and the corresponding state (custom icon library, app
// background, theme) was silently absent from every full keystore,
// resetting on import (issue #214 pt.4b). Keep these byte-identical to
// the constants in the owning components.
export const KEYSTORE_LS_WHITELIST: string[] = [
    // AeroAgent agent mode (Safe / Auto / Extreme) -- security-relevant
    'aeroftp_ai_agent_mode',

    // AeroFile multi-tab state (both dual-panel sides)
    'aerofile_local_tabs',
    'aerofile_local_tabs_2',
    'aerofile_active_tab',
    'aerofile_active_tab_2',
    'aerofile_recent_paths',
    'aerofile_show_sidebar',
    'aerofile_sidebar_mode',
    'aerofile_custom_locations',

    // Server card UX
    'aeroftp-favorite-servers',
    'aeroftp_myservers_filter',
    'aeroftp_hide_server_username',
    'aeroftp-my-servers-density',

    // IntroHub
    'aeroftp-intro-active-tab',
    'aeroftp-intro-view-mode',
    'aeroftp-discover-category',

    // Appearance: theme + background + lock screen
    'aeroftp-theme',
    'aeroftp-icon-theme',
    'aeroftp_app_background_pattern',
    'aeroftp_lock_pattern',

    // Custom assets the user authored (icon library + its ordering)
    'aeroftp-custom-icons',
    'aeroftp-custom-icons-sort',

    // Terminal preferences
    'aeroftp-terminal-settings',

    // GitHub commit dialog co-authors
    'github-co-authors',

    // Activity log filters
    'aeroftp_activitylog_filters',
    'aeroftp_activitylog_filter',
    'aeroftp_activitylog_show_cloudsync',
];

/**
 * Collect the whitelisted keys from `window.localStorage` into a plain
 * map suitable for serialisation. Missing keys are simply omitted (the
 * backup format treats absence as "user never set this").
 */
export const collectLocalStorage = (): Record<string, string> => {
    const out: Record<string, string> = {};
    for (const key of KEYSTORE_LS_WHITELIST) {
        try {
            const value = localStorage.getItem(key);
            if (value !== null) out[key] = value;
        } catch {
            // Storage quota or disabled storage: skip silently.
        }
    }
    return out;
};

/**
 * Restore a previously exported localStorage map into the running
 * window. Only keys still on the whitelist are applied; entries from
 * an older backup whose key has since been retired are dropped (the
 * backup remains valid, the user just won't see ghosts of removed
 * features).
 *
 * Returns the number of keys actually written.
 */
export const applyLocalStorage = (map: Record<string, string> | undefined | null): number => {
    if (!map) return 0;
    let applied = 0;
    const allowed = new Set(KEYSTORE_LS_WHITELIST);
    for (const [key, value] of Object.entries(map)) {
        if (!allowed.has(key)) continue;
        try {
            localStorage.setItem(key, value);
            applied++;
        } catch {
            // Quota: stop trying further keys to avoid partial-state churn.
            break;
        }
    }
    return applied;
};
