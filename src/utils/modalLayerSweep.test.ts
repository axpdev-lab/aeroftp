// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { MODAL_LAYER, MODAL_Z, modalZIndexOf } from './modalLayers';

/**
 * Sweeps every full-screen overlay in the source and checks it against the
 * scale in `modalLayers.ts`.
 *
 * A per-component test cannot catch this class of bug: what broke in #537 was
 * not one component but the *relation* between two of them, and the app has 144
 * `fixed inset-0` overlays across 6 tiers. The question "can anything cover the
 * confirm the user is being asked to answer?" is only answerable over the whole
 * set, so it is asked here, over the whole set.
 *
 * On the code this replaced the sweep fails twice: `ConfirmDialog` sat at z-50,
 * under all 67 overlays of the z-9998..10001 tiers, and the two lock screens sat
 * at z-100 and z-200, under those same 67.
 */

const overlayModules = import.meta.glob('../**/*.tsx', {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

interface Overlay {
    file: string;
    line: number;
    z: number;
    /** Which key of the scale it came through, when it uses `MODAL_Z`. */
    layer: keyof typeof MODAL_LAYER | null;
    /** Set when the overlay declares no z-index at all. */
    unlayered?: true;
}

/**
 * The text of the `className` value that contains `index`, or null.
 *
 * Reading a fixed window of lines around `fixed inset-0` is not good enough: the
 * window reaches into the neighbouring element, so a `z-` class belonging to a
 * sibling can be recorded as this overlay's. The attribute value is the only
 * span that certainly belongs to the same element, so that is what is read,
 * whether it is a quoted string or a `{...}` expression with a template literal
 * inside it.
 */
const classNameValueAt = (source: string, index: number): string | null => {
    let from = source.lastIndexOf('className=', index);
    while (from !== -1) {
        let cursor = from + 'className='.length;
        let end: number;
        if (source[cursor] === '"' || source[cursor] === "'") {
            end = source.indexOf(source[cursor], cursor + 1);
            if (end === -1) return null;
        } else if (source[cursor] === '{') {
            let depth = 0;
            end = cursor;
            for (; end < source.length; end++) {
                if (source[end] === '{') depth++;
                else if (source[end] === '}' && --depth === 0) break;
            }
        } else {
            return null;
        }
        if (index <= end) return source.slice(cursor, end + 1);
        // The match sits after this attribute closes: it belongs to a later one.
        from = source.indexOf('className=', end);
        if (from === -1 || from > index) return null;
    }
    return null;
};

/**
 * Comments blanked out, newlines kept so line numbers still line up.
 *
 * A comment that *mentions* `fixed inset-0`, and `SaveAllMenu` has one
 * explaining why its confirm is portalled, is not an overlay. Counting it as one
 * makes the sweep report a missing z-index for an element that does not exist.
 */
const withoutComments = (source: string): string =>
    source
        .replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, ' '))
        .replace(/(^|[^:])\/\/[^\n]*/g, (m, lead) => lead + ' '.repeat(m.length - lead.length));

/** Every `fixed inset-0` in the tree, with the z-index it ends up at. */
const collectOverlays = (): Overlay[] => {
    const found: Overlay[] = [];
    for (const [file, raw] of Object.entries(overlayModules)) {
        const source = withoutComments(raw);
        let at = source.indexOf('fixed inset-0');
        while (at !== -1) {
            const line = source.slice(0, at).split('\n').length;
            const value = classNameValueAt(source, at) ?? '';

            const viaScale = value.match(/MODAL_Z\.([A-Za-z]+)/);
            const literal = value.match(/\bz-(?:\[\d+\]|\d+)(?![\w-])/);
            if (viaScale && (MODAL_LAYER as Record<string, number>)[viaScale[1]] !== undefined) {
                const layer = viaScale[1] as keyof typeof MODAL_LAYER;
                found.push({ file, line, z: MODAL_LAYER[layer], layer });
            } else if (literal) {
                found.push({ file, line, z: modalZIndexOf(literal[0]) as number, layer: null });
            } else {
                found.push({ file, line, z: -1, layer: null, unlayered: true });
            }
            at = source.indexOf('fixed inset-0', at + 1);
        }
    }
    return found;
};

let cached: Overlay[] | null = null;
/** Collected on first use, inside a test, so a parse problem fails a named test
 *  rather than aborting the whole file during evaluation. */
const overlays = (): Overlay[] => (cached ??= collectOverlays());

const at = (o: Overlay) => `${o.file.replace('../', 'src/')}:${o.line} (z=${o.z})`;

describe('overlay stacking sweep (#537)', () => {
    it('finds the overlays to check', () => {
        // A guard on the sweep itself: if the glob or the pattern ever stops
        // matching, the assertions below would pass over an empty set.
        expect(overlays().length).toBeGreaterThan(100);
    });

    it('finds a z-index on every one of them', () => {
        // An overlay with no z at all falls back to `z-index: auto` and is
        // ordered purely by where it happens to sit in the DOM, which is the
        // same trap by another route.
        const unlayered = overlays().filter((o) => o.unlayered).map(at);
        expect(unlayered, 'full-screen overlays with no z-index').toEqual([]);
    });

    it('lets nothing but the quit guard and the lock screens cover the app-wide confirm', () => {
        const covering = overlays().filter(
            (o) => o.z >= MODAL_LAYER.globalConfirm && o.layer !== 'globalConfirm'
                && o.layer !== 'guardedClose' && o.layer !== 'lock',
        );
        expect(covering.map(at), 'overlays at or above the app-wide confirm').toEqual([]);
    });

    it('renders the app-wide confirm above every dialog that can be waiting on it', () => {
        const confirms = overlays().filter((o) => o.layer === 'globalConfirm');
        expect(confirms.length, 'ConfirmDialog must use MODAL_Z.globalConfirm').toBe(1);
        const dialogs = overlays().filter((o) => o.layer !== 'globalConfirm' && o.layer !== 'guardedClose' && o.layer !== 'lock');
        const highest = Math.max(...dialogs.map((o) => o.z));
        expect(MODAL_LAYER.globalConfirm).toBeGreaterThan(highest);
    });

    it('lets nothing cover the lock screens', () => {
        const locks = overlays().filter((o) => o.layer === 'lock');
        // Both LockScreen and AccountLockScreen.
        expect(locks.length, 'lock screens on MODAL_Z.lock').toBe(2);
        const others = overlays().filter((o) => o.layer !== 'lock');
        const highest = Math.max(...others.map((o) => o.z));
        expect(MODAL_LAYER.lock).toBeGreaterThan(highest);
    });

    it('keeps the quit guard above the app-wide confirm', () => {
        expect(MODAL_LAYER.guardedClose).toBeGreaterThan(MODAL_LAYER.globalConfirm);
        expect(MODAL_LAYER.lock).toBeGreaterThan(MODAL_LAYER.guardedClose);
    });

    it('has the Find Duplicates modal and its own confirm on the modal tiers', () => {
        // The pair from #537, named explicitly: the modal must stay under the
        // app-wide confirm, and its own confirm must stay over itself.
        const dedupe = overlays().filter((o) => o.file.includes('DuplicateFinderDialog'));
        expect(dedupe.map((o) => o.layer).sort()).toEqual(['modal', 'modalConfirm']);
    });
});

describe('MODAL_Z classes are literal enough for Tailwind to emit', () => {
    it('spells every class out', () => {
        for (const value of Object.values(MODAL_Z)) {
            expect(value).toMatch(/^z-(?:\[\d+\]|\d+)$/);
        }
    });
});
