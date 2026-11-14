// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it, vi } from 'vitest';

/**
 * Discussion #1527: the app-wide ESC shortcut closed the universal preview
 * directly, after the preview had already asked whether to drop unsaved
 * AeroImage edits, so the edits were lost unasked. The handler is run from
 * App.tsx's own source against stub state, as the shortcut runs it.
 */
const escapeHandlerSource = (): string => {
    const app = readFileSync(resolve(process.cwd(), 'src/App.tsx'), 'utf8');
    const start = app.indexOf("'Escape': () => {");
    expect(start).toBeGreaterThan(-1);
    const open = app.indexOf('{', start);
    let depth = 0;
    for (let i = open; i < app.length; i += 1) {
        if (app[i] === '{') depth += 1;
        else if (app[i] === '}') {
            depth -= 1;
            if (depth === 0) return app.slice(open + 1, i);
        }
    }
    throw new Error('unterminated Escape handler');
};

const runEscape = (state: Record<string, unknown>) => {
    const spies = new Map<string, ReturnType<typeof vi.fn>>();
    const scope = new Proxy(state, {
        has: () => true,
        get: (target, key) => {
            if (typeof key !== 'string') return undefined;
            if (key in target) return target[key];
            if (key === 'Set') return Set;
            if (!spies.has(key)) spies.set(key, vi.fn());
            return spies.get(key);
        },
    });
    // Sloppy-mode `with` resolves every free name of the handler on the stub scope.
    new Function('scope', `with (scope) { ${escapeHandlerSource()} }`)(scope);
    return (name: string) => spies.get(name);
};

describe("the app-wide ESC shortcut", () => {
    const none = new Set<string>();
    const closed = {
        quickLookOpen: false, universalPreviewOpen: false, showCyberTools: false, showShortcutsDialog: false,
        showAboutDialog: false, showSettingsPanel: true, inputDialog: null, confirmDialog: null,
        selectedRemoteFiles: none, selectedLocalFiles: none, selectedLocalFiles2: none,
    };

    it('leaves an open preview to its own guard, and closes nothing underneath it', () => {
        const spy = runEscape({ ...closed, universalPreviewOpen: true });
        expect(spy('closeUniversalPreview')).toBeUndefined();
        expect(spy('setShowSettingsPanel')).toBeUndefined();
    });

    it('still closes the next layer when no preview is open', () => {
        const spy = runEscape(closed);
        expect(spy('setShowSettingsPanel')).toHaveBeenCalledWith(false);
    });
});
