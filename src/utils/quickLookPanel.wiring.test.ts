// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Regression wiring: with the second local panel focused, Space must preview
// the file selected there. The overlay used to read panel 1's list at the same
// index, so it showed another file (or nothing) and arrow keys moved panel 1's
// selection. Both halves are run from the production App.tsx source: the Space
// branch that opens Quick Look, and the derivation the overlay renders from.

import { transformWithOxc } from 'vite';
import { describe, expect, it, vi } from 'vitest';
import app from '../App.tsx?raw';
import { quickLookStartIndex, quickLookView } from './quickLookPanel';

async function run<T>(source: string, context: Record<string, unknown>, result: string): Promise<T> {
    const { code } = await transformWithOxc(source, 'quick-look-wiring.ts');
    return new Function(...Object.keys(context), `${code}\nreturn ${result};`)(...Object.values(context));
}

function slice(start: string, end: string, from = 0): string {
    const at = app.indexOf(start, from);
    const stop = app.indexOf(end, at + start.length);
    if (at < 0 || stop < 0) throw new Error(`App.tsx block not found: ${start}`);
    return app.slice(at, stop);
}

// The local branch of the Space shortcut, up to the remote branch.
const spaceBranch = () => slice('const panel = getActiveLocalState();', '} else {', app.indexOf("'Space': () => {"));
// The render-time value the overlay reads its file, list, folder and selection from.
const overlaySource = () => slice('const quickLookTarget =', '\n\n');

const entry = (name: string, is_dir = false) => ({ name, path: '', size: 1, modified: null, is_dir });
const left = [entry('a.txt'), entry('b.txt'), entry('c.txt')];
const right = [entry('docs', true), entry('notes.md'), entry('photo.png')];

function dualPanels() {
    return {
        sortedLocalFiles: left, currentLocalPath: '/left', setSelectedLocalFiles: vi.fn(),
        sortedLocalFiles2: right, currentLocalPath2: '/right', setSelectedLocalFiles2: vi.fn(),
    };
}

async function pressSpace(id: 'local' | 'local2', selected: string) {
    const panels = dualPanels();
    const opened = { panelId: undefined as unknown, index: undefined as unknown, open: false };
    await run(spaceBranch(), {
        getActiveLocalState: () => id === 'local2'
            ? { id, sortedFiles: right, selection: new Set([selected]) }
            : { id, sortedFiles: left, selection: new Set([selected]) },
        quickLookStartIndex,
        setQuickLookPanelId: (value: unknown) => { opened.panelId = value; },
        setQuickLookIndex: (value: unknown) => { opened.index = value; },
        setQuickLookOpen: (value: boolean) => { opened.open = value; },
    }, 'undefined');
    expect(opened.open).toBe(true);
    const target = await run<ReturnType<typeof quickLookView>>(overlaySource(), {
        ...panels, quickLookView, quickLookOpen: opened.open, quickLookPanelId: opened.panelId, quickLookIndex: opened.index,
    }, 'quickLookTarget');
    return { target, panels };
}

describe('Quick Look follows the local panel that opened it', () => {
    it('previews and navigates the second panel when it has focus', async () => {
        const { target, panels } = await pressSpace('local2', 'photo.png');
        expect(target?.file.name, 'overlay must show the file selected in local2, not the row at that index in panel 1').toBe('photo.png');
        expect(target?.allFiles).toBe(right);
        expect(target?.currentPath).toBe('/right');
        target!.select(1);
        expect(panels.setSelectedLocalFiles2).toHaveBeenCalledWith(new Set(['notes.md']));
        expect(panels.setSelectedLocalFiles, 'arrow navigation must not move panel 1 selection').not.toHaveBeenCalled();
    });

    it('keeps panel 1 behavior unchanged', async () => {
        const { target, panels } = await pressSpace('local', 'b.txt');
        expect(target?.file.name).toBe('b.txt');
        expect(target?.allFiles).toBe(left);
        expect(target?.currentPath).toBe('/left');
        target!.select(2);
        expect(panels.setSelectedLocalFiles).toHaveBeenCalledWith(new Set(['c.txt']));
        expect(panels.setSelectedLocalFiles2).not.toHaveBeenCalled();
    });

    it('renders the overlay only from that derivation', () => {
        const overlay = slice('<QuickLookOverlay', '/>');
        expect(app.slice(app.indexOf('<QuickLookOverlay') - 40, app.indexOf('<QuickLookOverlay'))).toContain('{quickLookTarget && (');
        for (const panel1Only of ['sortedLocalFiles', 'currentLocalPath', 'setSelectedLocalFiles']) {
            expect(overlay, `QuickLookOverlay props read ${panel1Only} directly, which is always panel 1`).not.toContain(panel1Only);
        }
    });
});
