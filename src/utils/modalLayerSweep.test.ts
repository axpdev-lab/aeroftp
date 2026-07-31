// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { MODAL_LAYER, MODAL_Z } from './modalLayers';

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

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const overlayModules = import.meta.glob('../**/*.tsx', {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

/** `z-50` / `z-[9999]` → 50 / 9999. Anything else → null. */
const readZ = (token: string): number | null => {
    const arbitrary = token.match(/^z-\[(\d+)\]$/);
    if (arbitrary) return Number(arbitrary[1]);
    const scale = token.match(/^z-(\d+)$/);
    return scale ? Number(scale[1]) : null;
};

interface Overlay {
    file: string;
    line: number;
    z: number;
    /** Which key of the scale it came through, when it uses `MODAL_Z`. */
    layer: keyof typeof MODAL_LAYER | null;
}

/**
 * Every `fixed inset-0` in the tree, with the z-index it ends up at. The class
 * may be a literal (`z-[9999]`) or a `MODAL_Z` reference; both are resolved.
 * The class can also sit a line or two away from `fixed inset-0` when the JSX
 * wraps, so a small window around the match is searched.
 */
const collectOverlays = (): Overlay[] => {
    const found: Overlay[] = [];
    for (const [file, source] of Object.entries(overlayModules)) {
        const lines = source.split('\n');
        for (let i = 0; i < lines.length; i++) {
            if (!lines[i].includes('fixed inset-0')) continue;
            const window = lines.slice(Math.max(0, i - 2), i + 3).join('\n');

            const viaScale = window.match(/MODAL_Z\.([A-Za-z]+)/);
            if (viaScale) {
                const key = viaScale[1] as keyof typeof MODAL_LAYER;
                expect(MODAL_LAYER[key], `${file}:${i + 1} uses MODAL_Z.${key}`).toBeTypeOf('number');
                found.push({ file, line: i + 1, z: MODAL_LAYER[key], layer: key });
                continue;
            }

            const literal = window.match(/\bz-(?:\[\d+\]|\d+)(?![\w-])/);
            const z = literal ? readZ(literal[0]) : null;
            // An overlay with no z at all falls back to `z-index: auto` and is
            // ordered purely by where it happens to sit in the DOM — the same
            // trap by another route. There are none; keep it that way.
            expect(z, `${file}:${i + 1} is a full-screen overlay with no z-index`).not.toBeNull();
            found.push({ file, line: i + 1, z: z as number, layer: null });
        }
    }
    return found;
};

const overlays = collectOverlays();
const at = (o: Overlay) => `${o.file.replace('../', 'src/')}:${o.line} (z=${o.z})`;

describe('overlay stacking sweep (#537)', () => {
    it('finds the overlays to check', () => {
        // A guard on the sweep itself: if the glob or the pattern ever stops
        // matching, the assertions below would pass over an empty set.
        expect(overlays.length).toBeGreaterThan(100);
    });

    it('lets nothing but the quit guard and the lock screens cover the app-wide confirm', () => {
        const covering = overlays.filter(
            (o) => o.z >= MODAL_LAYER.globalConfirm && o.layer !== 'globalConfirm'
                && o.layer !== 'guardedClose' && o.layer !== 'lock',
        );
        expect(covering.map(at), 'overlays at or above the app-wide confirm').toEqual([]);
    });

    it('renders the app-wide confirm above every dialog that can be waiting on it', () => {
        const confirms = overlays.filter((o) => o.layer === 'globalConfirm');
        expect(confirms.length, 'ConfirmDialog must use MODAL_Z.globalConfirm').toBe(1);
        const dialogs = overlays.filter((o) => o.layer !== 'globalConfirm' && o.layer !== 'guardedClose' && o.layer !== 'lock');
        const highest = Math.max(...dialogs.map((o) => o.z));
        expect(MODAL_LAYER.globalConfirm).toBeGreaterThan(highest);
    });

    it('lets nothing cover the lock screens', () => {
        const locks = overlays.filter((o) => o.layer === 'lock');
        // Both LockScreen and AccountLockScreen.
        expect(locks.length, 'lock screens on MODAL_Z.lock').toBe(2);
        const others = overlays.filter((o) => o.layer !== 'lock');
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
        const dedupe = overlays.filter((o) => o.file.includes('DuplicateFinderDialog'));
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
