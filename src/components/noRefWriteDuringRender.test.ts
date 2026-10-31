// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import appRaw from '../App.tsx?raw';

/**
 * No ref is written while App renders.
 *
 * React can render and then discard the result, and a ref written during that
 * render keeps a value from a tree that never committed: the drop handler, the
 * transfer functions or the sorted rows of a render that was thrown away.
 * App's latest-value refs are written in a layout effect, once the render
 * commits and before an event can read them.
 */
describe('App writes no ref during render', () => {
    it('writes each latest-value ref after the commit', () => {
        const start = appRaw.indexOf('\nconst App: React.FC = () => {');
        expect(start).toBeGreaterThan(-1);
        const body = appRaw.slice(start, appRaw.indexOf('\nexport default App;'));
        // A statement of the component's own body, not of a function inside it.
        expect(body.match(/^ {2}[A-Za-z0-9_]+\.current = .*$/gm) ?? []).toEqual([]);
        for (const ref of ['sortedLocalFilesRef', 'sortedRemoteFilesRef', 'handleToggleAeroFileRef', 'transferFnsRef', 'crossPanelDropRef', 'lockControllerInterrupt']) {
            expect(body, ref).toMatch(new RegExp(`useLayoutEffect\\(\\(\\) => \\{[^}]*?\\b${ref}\\.current = `));
        }
    });
});
