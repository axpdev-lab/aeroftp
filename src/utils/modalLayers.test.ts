// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { MODAL_LAYER, MODAL_Z, modalZIndexOf } from './modalLayers';

describe('modal stacking scale (#537)', () => {
    it('puts the app-wide confirm above every modal', () => {
        // The whole point of the scale. `ConfirmDialog` is mounted before every
        // modal in App.tsx, so at equal z-index the modal wins on DOM order and
        // the confirm becomes invisible and unclickable underneath it.
        expect(MODAL_LAYER.globalConfirm).toBeGreaterThan(MODAL_LAYER.modal);
        expect(MODAL_LAYER.globalConfirm).toBeGreaterThan(MODAL_LAYER.modalConfirm);
    });

    it("puts a modal's own confirm above that modal", () => {
        expect(MODAL_LAYER.modalConfirm).toBeGreaterThan(MODAL_LAYER.modal);
    });

    it('keeps the Tailwind classes in step with the numbers', () => {
        // A class that drifts from its number is worse than no scale at all: the
        // test would keep passing while the rendered order silently changed.
        for (const key of Object.keys(MODAL_LAYER) as (keyof typeof MODAL_LAYER)[]) {
            expect(modalZIndexOf(MODAL_Z[key]), key).toBe(MODAL_LAYER[key]);
        }
    });

    it('reads both the built-in scale and arbitrary z classes', () => {
        expect(modalZIndexOf('z-50')).toBe(50);
        expect(modalZIndexOf('z-[60]')).toBe(60);
        expect(modalZIndexOf('z-[70]')).toBe(70);
        expect(modalZIndexOf('z-auto')).toBeNull();
        expect(modalZIndexOf('flex')).toBeNull();
    });
});
