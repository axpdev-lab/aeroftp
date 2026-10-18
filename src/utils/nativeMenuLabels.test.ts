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

    it('relabels the installed menu in place, with appmenu-gtk-module kept out', () => {
        // appmenu-gtk-module (in Ubuntu's default GTK_MODULES) recursed on the
        // app menu bar until the main thread overflowed its stack whenever a
        // realized window held it: a rebuild that installed the menu again
        // after startup aborted the app, so did turning the bar on.
        const fnBody = (name: string) => {
            const start = libRs.indexOf(`fn ${name}(`);
            return libRs.slice(start, libRs.indexOf('\n}\n', start));
        };
        const rebuild = fnBody('rebuild_menu_on_main');
        expect(rebuild).toMatch(/\} else \{[^}]*relabel_installed_menu\(&app, &menu\)/);
        expect(rebuild).not.toMatch(/set_menu\(|remove_menu\(/);
        // Forced before GTK starts, not defaulted: a session value of "1"
        // would load the module again.
        const run = fnBody('run');
        const proxy = run.indexOf('std::env::set_var("UBUNTU_MENUPROXY", "0");');
        expect(proxy).toBeGreaterThan(-1);
        expect(run.slice(Math.max(0, proxy - 120), proxy)).not.toMatch(/var_os\("UBUNTU_MENUPROXY"\)/);
        expect(proxy).toBeLessThan(run.indexOf('tauri::Builder::default()'));
        // Settings may ask for the bar before the menu exists; the startup
        // sequence applies the stored choice instead of always removing it.
        expect(fnBody('toggle_menu_bar')).toMatch(/MAIN_MENU_BAR_VISIBLE\.store\(visible,/);
        expect(fnBody('finish_startup')).toMatch(
            /if !MAIN_MENU_BAR_VISIBLE\.load\([^)]*\) \{\s*let _ = main_window\.remove_menu\(\);/,
        );
    });
});
