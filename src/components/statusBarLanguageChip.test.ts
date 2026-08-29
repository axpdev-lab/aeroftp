// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// The status bar language chip exists because AeroFTP ships 47 interface
// languages, starts in English by design, and hides the selector in
// Settings > Appearance > Interface. A SourceForge reviewer on 2026-08-28
// listed "I want more languages" as a con of a build that already had his.
//
// The chip is only useful if the whole chain holds: it renders, it opens
// Settings, and Settings lands on the pane with the language list. That last
// link is the fragile one, because the Settings panel stays mounted for the
// life of the app and its Appearance sub-tab remembers the last visit: open it
// on 'ui' alone and a user who was last in Theme gets Theme again, with no
// language list in sight and no error anywhere.
//
// Assertions read the source rather than the rendered DOM, following
// customTitlebarDragRegions.test.ts: vitest runs in `node` here, with no DOM.

import { describe, expect, it } from 'vitest';
import STATUS_BAR from './StatusBar.tsx?raw';
import SETTINGS_PANEL from './SettingsPanel.tsx?raw';
import APP from '../App.tsx?raw';

/** The chip's JSX block, from its guard to the end of the button. */
const chipBlock = (src: string): string => {
    const start = src.indexOf('{onOpenLanguageSettings && (');
    expect(start).toBeGreaterThan(-1);
    return src.slice(start, src.indexOf('</button>', start));
};

describe('status bar language chip', () => {
    it('shows the language code and nothing that needs translating', () => {
        const chip = chipBlock(STATUS_BAR);
        // The label is the language's own code, the tooltip is the nativeName
        // that AVAILABLE_LANGUAGES already carries. A `t('...')` in here would
        // mean a new key in all 47 locale files for a chip whose entire content
        // is language data.
        expect(chip).toContain('{language}');
        expect(chip).toContain('title={languageNativeName}');
        expect(chip).not.toContain("t('");
    });

    it('carries no flag', () => {
        // A flag names a country, not a language: Spanish is not Spain, English
        // is not the United Kingdom. The owner asked for the code alone.
        expect(chipBlock(STATUS_BAR)).not.toMatch(/[\u{1F1E6}-\u{1F1FF}]/u);
        expect(chipBlock(STATUS_BAR)).not.toContain('flag');
    });

    it('is optional, so a caller that has nowhere to send the user gets no chip', () => {
        expect(STATUS_BAR).toContain('onOpenLanguageSettings?: () => void;');
    });
});

describe('the chip lands on the language list, not merely on Appearance', () => {
    it('App asks for the Appearance tab AND the Interface sub-tab', () => {
        const start = APP.indexOf('onOpenLanguageSettings={()');
        expect(start).toBeGreaterThan(-1);
        // The handler body, up to the closing of its arrow function.
        const handler = APP.slice(start, APP.indexOf('}}', start));
        expect(handler).toContain("setSettingsInitialTab('ui')");
        expect(handler).toContain("setSettingsInitialAppearanceSubTab('interface')");
    });

    it('App passes the sub-tab down and clears it on close', () => {
        expect(APP).toContain('initialAppearanceSubTab={settingsInitialAppearanceSubTab}');
        expect(APP).toContain('setSettingsInitialAppearanceSubTab(undefined)');
    });

    it('SettingsPanel actually applies the requested sub-tab when it opens', () => {
        const openEffect = SETTINGS_PANEL.slice(
            SETTINGS_PANEL.indexOf('if (isOpen && initialTab) {'),
            SETTINGS_PANEL.indexOf('}, [isOpen, initialTab'),
        );
        expect(openEffect).toContain('setAppearanceSubTab(initialAppearanceSubTab)');
    });

    it('re-applies the sub-tab when the request changes, not only on the first open', () => {
        // Omitting it from the dependency list makes the second visit from the
        // chip a no-op if the user has since wandered to another sub-tab.
        expect(SETTINGS_PANEL).toContain('}, [isOpen, initialTab, initialAppearanceSubTab]);');
    });
});
