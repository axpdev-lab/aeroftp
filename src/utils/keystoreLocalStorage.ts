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
 * Synthetic key carrying OS-level "launch on system startup" state in
 * the keystore payload. The setting itself lives outside the WebView
 * (LaunchAgent on macOS, .desktop file on Linux, Registry on Windows)
 * via `tauri-plugin-autostart`, so the regular localStorage whitelist
 * cannot capture it. We piggyback on the `local_storage` map in the
 * keystore envelope: `collectLocalStorage` populates this key from the
 * plugin at export time, `applyLocalStorage` translates it back into a
 * plugin enable/disable call at import time and never writes it to
 * `window.localStorage`. Older backups without the key simply leave
 * the OS state untouched on import (issue #214 C3 pt.E1).
 */
const AUTOSTART_KEY = '_aeroftp_system_autostart';

/**
 * Event dispatched on the global `window` once `applyLocalStorage` has
 * finished applying restored values. Reactive hooks that snapshot
 * localStorage once on mount (`useTheme`, `useIconTheme`, App's
 * `appBackgroundId`) listen for this and re-read their key so the
 * imported preferences take effect without a manual refresh
 * (issue #214 C3 pt.E2).
 */
export const KEYSTORE_RESTORED_EVENT = 'aeroftp-localstorage-restored';

/**
 * Collect the whitelisted keys from `window.localStorage` into a plain
 * map suitable for serialisation. Missing keys are simply omitted (the
 * backup format treats absence as "user never set this").
 *
 * Also captures OS-level state that does not live in localStorage but
 * is part of the user's identity on this install (currently: the
 * autostart toggle).
 */
export const collectLocalStorage = async (): Promise<Record<string, string>> => {
    const out: Record<string, string> = {};
    for (const key of KEYSTORE_LS_WHITELIST) {
        try {
            const value = localStorage.getItem(key);
            if (value !== null) out[key] = value;
        } catch {
            // Storage quota or disabled storage: skip silently.
        }
    }
    // OS-level autostart state. Plugin import is dynamic so the helper
    // stays usable in a browser preview where the Tauri plugin is
    // unavailable.
    try {
        const { isEnabled } = await import('@tauri-apps/plugin-autostart');
        const enabled = await isEnabled();
        out[AUTOSTART_KEY] = enabled ? 'true' : 'false';
    } catch {
        // Plugin unavailable (web preview, dev): skip silently.
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
 * The synthetic `AUTOSTART_KEY` is intercepted and translated into a
 * `tauri-plugin-autostart` enable/disable call instead of being
 * written to `window.localStorage`.
 *
 * After the map has been applied, dispatches `KEYSTORE_RESTORED_EVENT`
 * on `window` so hooks that snapshot localStorage once on mount can
 * re-read their key without requiring the user to refresh the page.
 *
 * Returns the number of keys actually applied (including the
 * autostart toggle if present).
 */
export const applyLocalStorage = async (
    map: Record<string, string> | undefined | null,
): Promise<number> => {
    if (!map) return 0;
    let applied = 0;
    const allowed = new Set(KEYSTORE_LS_WHITELIST);
    for (const [key, value] of Object.entries(map)) {
        if (key === AUTOSTART_KEY) {
            try {
                const { enable, disable } = await import('@tauri-apps/plugin-autostart');
                if (value === 'true') {
                    await enable();
                } else {
                    await disable();
                }
                applied++;
            } catch {
                // Plugin unavailable: skip silently rather than fail
                // the whole import.
            }
            continue;
        }
        if (!allowed.has(key)) continue;
        try {
            localStorage.setItem(key, value);
            applied++;
        } catch {
            // Quota: stop trying further keys to avoid partial-state churn.
            break;
        }
    }
    if (applied > 0) {
        try {
            window.dispatchEvent(new CustomEvent(KEYSTORE_RESTORED_EVENT));
            // The custom-icons manager already listens for its own
            // pre-existing event; re-emit it so the library refreshes
            // alongside theme + background.
            window.dispatchEvent(new CustomEvent('aeroftp-custom-icons-changed'));
        } catch {
            // CustomEvent missing in some test environments: ignore.
        }
    }
    return applied;
};
