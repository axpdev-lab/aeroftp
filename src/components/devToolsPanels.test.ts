// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Pins the second half of Ehud's #347 repro (2026-08-06): with the Editor
// column toggled off, a file handed to AeroTools became a tab nobody could
// see, and the app read as having ignored the gesture. Receiving a file has to
// bring the surface that received it on screen.

import { describe, it, expect } from 'vitest';
import { panelsWithIncomingFile, type PanelVisibility } from './DevTools/types';

const closed: PanelVisibility = { editor: false, terminal: true, chat: false };

describe('panelsWithIncomingFile', () => {
    it('opens the editor column when a file arrives while it is hidden', () => {
        // Exactly the repro: Editor toggled off, then double-click a .sh.
        expect(panelsWithIncomingFile(closed, true).editor).toBe(true);
    });

    it('leaves the other columns as the user arranged them', () => {
        const next = panelsWithIncomingFile(closed, true);
        expect(next.terminal).toBe(true);
        expect(next.chat).toBe(false);
    });

    it('does not reopen the editor when no file arrived', () => {
        // Clearing the file (or a re-render with none) must not fight the user
        // closing the column.
        expect(panelsWithIncomingFile(closed, false)).toBe(closed);
    });

    it('returns the same object when the editor is already open', () => {
        // Identity, not just equality: the caller passes this straight to
        // setState, and a fresh object there would re-render on every render.
        const open: PanelVisibility = { editor: true, terminal: false, chat: false };
        expect(panelsWithIncomingFile(open, true)).toBe(open);
    });
});
