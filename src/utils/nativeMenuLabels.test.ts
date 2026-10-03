// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import libRs from '../../src-tauri/src/lib.rs?raw';
import appSource from '../App.tsx?raw';
import en from '../i18n/locales/en.json';
import { NATIVE_MENU_KEYS, nativeMenuLabels } from './nativeMenuLabels';

/** The `get("key", ...)` names `rebuild_menu_on_main` reads, from the Rust source. */
function rustMenuKeys(): string[] {
    const start = libRs.indexOf('fn rebuild_menu_on_main(');
    const end = libRs.indexOf('\n}\n', start);
    return [...libRs.slice(start, end).matchAll(/get\("([A-Za-z]+)",/g)].map((m) => m[1]);
}

/**
 * Since 6cefedff3 nothing called `rebuild_menu`, so the native menu (always
 * visible on macOS, optional elsewhere) stayed in English in every language.
 */
describe('the native menu follows the language', () => {
    it('sends a translated label for every item rebuild_menu builds', () => {
        expect([...NATIVE_MENU_KEYS].sort()).toEqual(rustMenuKeys().sort());
        const menu = (en as { translations: { menu: Record<string, string> } }).translations.menu;
        expect(NATIVE_MENU_KEYS.filter((k) => !(k in menu))).toEqual([]);
        expect(nativeMenuLabels((key) => `<${key}>`).quit).toBe('<menu.quit>');
    });

    it('is rebuilt from App on a language change', () => {
        expect(appSource).toContain("invoke('rebuild_menu', { labels: nativeMenuLabels(t) })");
    });

    it('keeps a hidden native menu bar hidden after a rebuild', () => {
        // A global set_menu reaches every window on Linux; the main window's
        // visibility is the user's choice (toggle_menu_bar).
        // Not a name match: the rebuild must read the flag and remove the menu
        // when it is off, and the toggle must write it.
        const body = libRs.slice(libRs.indexOf('fn rebuild_menu_on_main('), libRs.indexOf('\n}\n', libRs.indexOf('fn rebuild_menu_on_main(')));
        expect(body).toMatch(/if !MAIN_MENU_BAR_VISIBLE\.load\([^)]*\)\s*\{[^}]*get_webview_window\("main"\)[^}]*\.remove_menu\(\)/);
        const toggle = libRs.slice(libRs.indexOf('fn toggle_menu_bar('), libRs.indexOf('fn rebuild_menu('));
        expect(toggle).toMatch(/MAIN_MENU_BAR_VISIBLE\.store\(visible,/);
    });
});
