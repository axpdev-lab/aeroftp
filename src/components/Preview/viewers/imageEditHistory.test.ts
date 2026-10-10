import { describe, expect, it } from 'vitest';
import { INITIAL_EDIT_STATE, type CropRect, type EditState } from '../types';
import { COALESCE_MS, applyCrop, commit, composeCrop, createHistory, redo, sameEdits, undo, unorientRect } from './imageEditHistory';

const state = (partial: Partial<EditState>): EditState => ({ ...INITIAL_EDIT_STATE, ...partial });

describe('edit history', () => {
    it('undoes and redoes each step in order', () => {
        let h = createHistory(INITIAL_EDIT_STATE);
        h = commit(h, state({ rotation: 90 }), 0);
        h = commit(h, state({ rotation: 90, flipH: true }), 10_000);
        h = undo(h);
        expect(h.present).toEqual(state({ rotation: 90 }));
        h = undo(h);
        expect(h.present).toEqual(INITIAL_EDIT_STATE);
        expect(undo(h)).toBe(h);
        h = redo(redo(h));
        expect(h.present).toEqual(state({ rotation: 90, flipH: true }));
        expect(redo(h)).toBe(h);
    });

    it('makes one step of a slider gesture, and two of two gestures', () => {
        let h = createHistory(INITIAL_EDIT_STATE);
        for (let v = 1; v <= 30; v++) h = commit(h, state({ brightness: v }), v * 20);
        expect(h.past).toHaveLength(1);
        h = commit(h, state({ brightness: 40 }), 30 * 20 + COALESCE_MS + 1);
        expect(h.past).toHaveLength(2);
        expect(undo(h).present).toEqual(state({ brightness: 30 }));
    });

    it('never merges a toggle or a different field into the previous step', () => {
        let h = createHistory(INITIAL_EDIT_STATE);
        h = commit(h, state({ brightness: 5 }), 0);
        h = commit(h, state({ brightness: 5, contrast: 5 }), 10);
        h = commit(h, state({ brightness: 5, contrast: 5, invert: true }), 20);
        h = commit(h, state({ brightness: 5, contrast: 5, invert: false }), 30);
        expect(h.past).toHaveLength(4);
    });

    it('drops the redo steps when a new edit is made', () => {
        let h = createHistory(INITIAL_EDIT_STATE);
        h = commit(h, state({ invert: true }), 0);
        h = undo(h);
        h = commit(h, state({ grayscale: true }), 5000);
        expect(h.future).toHaveLength(0);
    });

    it('ignores a change to the same values', () => {
        const h = commit(createHistory(INITIAL_EDIT_STATE), state({}), 0);
        expect(h.past).toHaveLength(0);
        expect(sameEdits(state({ crop: { x: 1, y: 2, width: 3, height: 4 } }), state({ crop: { x: 1, y: 2, width: 3, height: 4 } }))).toBe(true);
    });
});

describe('crop composition', () => {
    const original = { width: 800, height: 600 };

    it('moves a second crop by the first one, in the original file', () => {
        const first: CropRect = { x: 100, y: 50, width: 400, height: 300 };
        expect(composeCrop(first, { x: 10, y: 20, width: 100, height: 100 }, original)).toEqual({ x: 110, y: 70, width: 100, height: 100 });
    });

    it('never leaves the picture shown', () => {
        const first: CropRect = { x: 100, y: 50, width: 400, height: 300 };
        expect(composeCrop(first, { x: 350, y: 250, width: 100, height: 100 }, original)).toEqual({ x: 450, y: 300, width: 50, height: 50 });
        expect(composeCrop(null, { x: 700, y: 500, width: 200, height: 200 }, original)).toEqual({ x: 700, y: 500, width: 100, height: 100 });
    });

    it('keeps the scale of a size already chosen', () => {
        const next = applyCrop(state({ resize: { width: 400, height: 300 } }), { x: 0, y: 0, width: 400, height: 300 }, original);
        expect(next.resize).toEqual({ width: 200, height: 150 });
    });
});

// The save's geometry, pixel by pixel, as image_edit.rs runs it: rotate
// clockwise (image::rotate90/180/270), then mirror.
type Grid = number[][];
const makeGrid = (w: number, h: number): Grid => Array.from({ length: h }, (_, y) => Array.from({ length: w }, (_, x) => y * w + x));
const rotate90 = (g: Grid): Grid => {
    const h = g.length, w = g[0].length;
    return Array.from({ length: w }, (_, y) => Array.from({ length: h }, (_, x) => g[h - 1 - x][y]));
};
const orient = (g: Grid, rotation: EditState['rotation'], flipH: boolean, flipV: boolean): Grid => {
    let out = g;
    for (let i = 0; i < rotation / 90; i++) out = rotate90(out);
    if (flipH) out = out.map((row) => [...row].reverse());
    if (flipV) out = [...out].reverse();
    return out;
};
const pick = (g: Grid, r: CropRect): number[] =>
    g.slice(r.y, r.y + r.height).flatMap((row) => row.slice(r.x, r.x + r.width)).sort((a, b) => a - b);

describe('a selection on the turned and mirrored preview', () => {
    const source = makeGrid(5, 3);
    const rect: CropRect = { x: 1, y: 0, width: 2, height: 2 };
    for (const rotation of [0, 90, 180, 270] as const) {
        for (const flipH of [false, true]) {
            for (const flipV of [false, true]) {
                it(`covers the same pixels of the file (rotate ${rotation}, flipH ${flipH}, flipV ${flipV})`, () => {
                    const shown = orient(source, rotation, flipH, flipV);
                    const back = unorientRect(rect, { width: 5, height: 3 }, rotation, flipH, flipV);
                    expect(pick(source, back)).toEqual(pick(shown, rect));
                });
            }
        }
    }

    it('crops what is on screen after a turn', () => {
        // Turned 90 degrees, the 800 x 600 file shows as 600 x 800; its top
        // half on screen is the left half of the file.
        const next = applyCrop(state({ rotation: 90 }), { x: 0, y: 0, width: 600, height: 400 }, { width: 800, height: 600 });
        expect(next.crop).toEqual({ x: 0, y: 0, width: 400, height: 600 });
    });
});
