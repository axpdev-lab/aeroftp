// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';

/**
 * No source file may call `window.confirm`, `window.alert` or `window.prompt`.
 *
 * This is not a style rule. WebKitGTK does not implement the script dialogs, so
 * on Linux `window.confirm` returns without ever drawing anything and the caller
 * takes the branch as though the user had agreed. The v2.6.4 audit closed that
 * as H18 across six components and wrote the verification down as "`grep
 * 'window.confirm' src/components/` returns zero active calls".
 *
 * That grep had stopped being true. Six call sites had drifted back in, and the
 * most expensive of them was the account delete in Manage Users: on Linux an
 * account and its encrypted payloads went with no question asked, which is what
 * a user reported as "please show an 'Are you sure?' popup". A sentence in an
 * audit document cannot hold an invariant. This test can, so it is the test that
 * states it now.
 *
 * Comments are blanked before the scan: this very file names all three calls,
 * and so does the docstring of `ConfirmOverlay`, which is their replacement. A
 * scan that counted those would fail on the fix.
 */

const sources = import.meta.glob('../**/*.{ts,tsx}', {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

/** Blank comment bodies while keeping every offset, so line numbers survive. */
function withoutComments(source: string): string {
    return source
        .replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, ' '))
        .replace(/(^|[^:])\/\/[^\n]*/g, (m, lead) => lead + ' '.repeat(m.length - lead.length));
}

const NATIVE_DIALOG = /\bwindow\s*\.\s*(confirm|alert|prompt)\s*\(/g;

interface Call {
    file: string;
    line: number;
    fn: string;
}

const calls: Call[] = [];
for (const [file, source] of Object.entries(sources)) {
    if (file.endsWith('.test.ts') || file.endsWith('.test.tsx')) continue;
    const code = withoutComments(source);
    for (const match of code.matchAll(NATIVE_DIALOG)) {
        calls.push({
            file,
            line: code.slice(0, match.index ?? 0).split('\n').length,
            fn: match[1],
        });
    }
}

describe('native browser dialogs', () => {
    it('scans a source set large enough to be meaningful', () => {
        // A glob that silently matched nothing would make every assertion below
        // pass while checking not one line of the app.
        expect(Object.keys(sources).length).toBeGreaterThan(300);
    });

    it('is never used: WebKitGTK does not draw them and the caller proceeds anyway', () => {
        const listed = calls.map((c) => `${c.file}:${c.line} window.${c.fn}()`);
        expect(listed).toEqual([]);
    });

    it('the replacement exists and carries the layer it is rendered at', () => {
        const overlay = sources['../components/common/ConfirmOverlay.tsx'];
        expect(overlay, 'ConfirmOverlay.tsx must exist').toBeTruthy();
        // The class has to reach the caller as a literal from MODAL_Z: Tailwind
        // scans the source for class names, so a computed z class never lands in
        // the stylesheet and the overlay falls back to `z-index: auto`.
        expect(overlay).toMatch(/zClass: string/);
        expect(overlay).toMatch(/\$\{zClass\}/);
    });

    it('every component that raises one passes a MODAL_Z tier', () => {
        // `(?![A-Za-z])` so the component's own `React.FC<ConfirmOverlayProps>`
        // is not read as a call site: without it the match runs from that type
        // parameter to the first `/>` of the component's own JSX.
        const usage = /<ConfirmOverlay(?![A-Za-z])[\s\S]*?\/>/g;
        const callers = Object.entries(sources).filter(
            ([file, src]) => !file.endsWith('.test.ts') && !file.endsWith('.test.tsx')
                && file !== '../components/common/ConfirmOverlay.tsx'
                && new RegExp(usage.source).test(withoutComments(src)),
        );
        expect(callers.length).toBeGreaterThanOrEqual(5);
        for (const [file, src] of callers) {
            const code = withoutComments(src);
            for (const match of code.matchAll(usage)) {
                expect(match[0], `${file}: zClass must come from MODAL_Z`).toMatch(
                    /zClass=\{MODAL_Z\.\w+\}/,
                );
            }
        }
    });
});
