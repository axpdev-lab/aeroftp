// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import dialogRaw from './DuplicateFinderDialog.tsx?raw';

/**
 * The Find Duplicates results list had no scrollbar, on any platform.
 *
 * Not an oversight in the dialog: `html.modal-open` deliberately hides every
 * `::-webkit-scrollbar` on the page while a modal is open, because WebKitGTK
 * paints native scrollbars *above* CSS z-index overlays and the page behind
 * would otherwise show its bars through the dialog. The selector matched every
 * scrollable element, the modal's own content included — so the list the user
 * was scrolling had its scrollbar hidden by a rule aimed at the page behind it.
 * Reported on #347, asking for one at least 14px wide.
 *
 * `.modal-scroll` opts a modal's own scroll container back in and gives it a
 * grabbable width. The rule itself lives in `styles.css`; a `?raw` import of a
 * stylesheet comes back empty under Vite, so what is checkable from here is that
 * the list carries the class — which is the part a future edit would drop.
 */
describe('the results list keeps its scrollbar (#347)', () => {
    it('marks the scroll container so the modal-open rule skips it', () => {
        const list = dialogRaw.slice(dialogRaw.indexOf('{/* Groups list (scrollable) */}'));
        expect(list.slice(0, 300)).toMatch(/className="modal-scroll [^"]*overflow-y-auto/);
    });

    it('still adds modal-open, so the page behind stays hidden', () => {
        // The workaround is not removed, only narrowed: dropping it would bring
        // back the WebKitGTK bars showing through the overlay.
        expect(dialogRaw).toMatch(/classList\.add\('modal-open'\)/);
    });
});
