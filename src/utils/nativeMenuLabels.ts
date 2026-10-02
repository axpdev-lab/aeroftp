// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Labels for the native menu, in the app's language.
 *
 * The native menu is built in Rust at startup in English. Until the VS
 * Code-style titlebar (6cefedff3) the frontend sent translated labels through
 * `rebuild_menu` on every language change; that commit removed the call with
 * the native menu bar it replaced, but the native menu still exists: macOS
 * always shows it in the system menu bar, and Linux and Windows show it when
 * Settings turns it on. Since then it stayed in English whatever the
 * language. These are the keys `rebuild_menu_on_main` reads (lib.rs), each
 * from the `menu.*` strings every locale already has.
 */
export const NATIVE_MENU_KEYS = [
    'quit',
    'about',
    'settings',
    'refresh',
    'shortcuts',
    'support',
    'file',
    'newFolder',
    'debugMode',
    'dependencies',
    'edit',
    'rename',
    'delete',
    'devtools',
    'toggleDevtools',
    'toggleEditor',
    'toggleTerminal',
    'toggleAgent',
    'view',
    'toggleTheme',
    'checkForUpdates',
    'help',
] as const;

export function nativeMenuLabels(t: (key: string) => string): Record<string, string> {
    const labels: Record<string, string> = {};
    for (const key of NATIVE_MENU_KEYS) labels[key] = t(`menu.${key}`);
    return labels;
}
