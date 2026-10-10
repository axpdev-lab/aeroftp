// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroImage edit history and crop composition (#1075).
 *
 * Every change to the edit state is a step that Undo and Redo walk through.
 * A slider or a size field moves through many values in one gesture, so the
 * changes to the same continuous field that arrive close together are one
 * step, not one per value.
 *
 * An applied crop is kept in the coordinates of the original file, because
 * the save runs the whole pipeline on the original, crop first and then
 * rotate and flip. The preview shows the picture as edited (cropped, turned,
 * mirrored), and a new selection is drawn on that: it is turned back and
 * moved by the crop already applied before it is stored, so the saved file
 * is the picture on screen.
 */

import type { CropRect, EditState } from '../types';

export interface EditHistory {
    past: EditState[];
    present: EditState;
    future: EditState[];
    /** The fields the last step changed, to merge a gesture into one step. */
    lastKey: string | null;
    lastAt: number;
}

/** Fields a single gesture changes through many values. */
const CONTINUOUS_FIELDS: ReadonlySet<string> = new Set(['brightness', 'contrast', 'hue', 'blur', 'sharpen', 'resize']);

/** Changes to the same continuous field closer than this are one step. */
export const COALESCE_MS = 800;

const sameValue = (a: unknown, b: unknown): boolean => JSON.stringify(a) === JSON.stringify(b);

/** The fields that differ between two edit states, in a stable order. */
export function changedFields(a: EditState, b: EditState): string[] {
    return (Object.keys(b) as (keyof EditState)[])
        .filter((k) => !sameValue(a[k], b[k]))
        .sort();
}

export function createHistory(state: EditState): EditHistory {
    return { past: [], present: state, future: [], lastKey: null, lastAt: 0 };
}

export function commit(h: EditHistory, next: EditState, now: number): EditHistory {
    const fields = changedFields(h.present, next);
    if (fields.length === 0) return h;
    const key = fields.join(',');
    const continuous = fields.every((f) => CONTINUOUS_FIELDS.has(f));
    if (continuous && key === h.lastKey && now - h.lastAt < COALESCE_MS) {
        return { ...h, present: next, future: [], lastAt: now };
    }
    return { past: [...h.past, h.present], present: next, future: [], lastKey: key, lastAt: now };
}

export function undo(h: EditHistory): EditHistory {
    if (h.past.length === 0) return h;
    const previous = h.past[h.past.length - 1];
    return { past: h.past.slice(0, -1), present: previous, future: [h.present, ...h.future], lastKey: null, lastAt: 0 };
}

export function redo(h: EditHistory): EditHistory {
    if (h.future.length === 0) return h;
    const [next, ...rest] = h.future;
    return { past: [...h.past, h.present], present: next, future: rest, lastKey: null, lastAt: 0 };
}

/** True when the two states would save the same picture. */
export function sameEdits(a: EditState, b: EditState): boolean {
    return changedFields(a, b).length === 0;
}

type Size = { width: number; height: number };

/** The size after a rotation by a right angle. */
export function orientedSize(size: Size, rotation: EditState['rotation']): Size {
    return rotation === 90 || rotation === 270 ? { width: size.height, height: size.width } : size;
}

/**
 * A rectangle on the turned and mirrored picture, on the picture before the
 * turn and the mirror. `size` is the picture before them. The save rotates
 * clockwise and then mirrors, as the preview does.
 */
export function unorientRect(
    rect: CropRect,
    size: Size,
    rotation: EditState['rotation'],
    flipH: boolean,
    flipV: boolean,
): CropRect {
    const shown = orientedSize(size, rotation);
    let { x, y } = rect;
    const { width: w, height: h } = rect;
    if (flipH) x = shown.width - x - w;
    if (flipV) y = shown.height - y - h;
    switch (rotation) {
        case 90:
            // (x, y) before the turn lands at (H - y, x).
            return { x: y, y: size.height - (x + w), width: h, height: w };
        case 180:
            return { x: size.width - (x + w), y: size.height - (y + h), width: w, height: h };
        case 270:
            // (x, y) before the turn lands at (y, W - x).
            return { x: size.width - (y + h), y: x, width: h, height: w };
        default:
            return { x, y, width: w, height: h };
    }
}

/**
 * A selection drawn on the preview, moved into the original file's
 * coordinates: the preview already shows `applied`, so the selection starts
 * at its corner. The result never leaves the original picture.
 */
export function composeCrop(
    applied: CropRect | null,
    selection: CropRect,
    original: { width: number; height: number },
): CropRect {
    const ox = applied?.x ?? 0;
    const oy = applied?.y ?? 0;
    const maxW = applied?.width ?? original.width;
    const maxH = applied?.height ?? original.height;
    const x0 = Math.min(Math.max(0, selection.x), maxW);
    const y0 = Math.min(Math.max(0, selection.y), maxH);
    const x1 = Math.min(Math.max(x0, selection.x + selection.width), maxW);
    const y1 = Math.min(Math.max(y0, selection.y + selection.height), maxH);
    return { x: ox + x0, y: oy + y0, width: x1 - x0, height: y1 - y0 };
}

/**
 * Applies a selection drawn on the preview (turned and mirrored as the edit
 * says) to the edit state. A size already chosen keeps its scale relative to
 * the picture: the resize runs after the crop, so a fixed target size would
 * otherwise stretch the smaller picture.
 */
export function applyCrop(
    state: EditState,
    selection: CropRect,
    original: { width: number; height: number },
): EditState {
    const before = state.crop ? { width: state.crop.width, height: state.crop.height } : original;
    const upright = unorientRect(selection, before, state.rotation, state.flipH, state.flipV);
    const crop = composeCrop(state.crop, upright, original);
    let resize = state.resize;
    if (resize) {
        const prevW = state.crop?.width ?? original.width;
        const prevH = state.crop?.height ?? original.height;
        resize = {
            width: Math.max(1, Math.round(resize.width * crop.width / prevW)),
            height: Math.max(1, Math.round(resize.height * crop.height / prevH)),
        };
    }
    return { ...state, crop, resize };
}
