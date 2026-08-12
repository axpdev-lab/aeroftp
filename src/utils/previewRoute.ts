// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Where a "show me this file" gesture sends a file.
//
// There were four copies of this decision: the local panel's double-click, the
// remote panel's double-click, the App-level local double-click, and the Space
// key. Each listed by hand the categories it sent to Universal Preview and left
// everything else to the AeroTools editor. Ehud reported the consequence on
// #347 (2026-08-06): a .txt opened in the preview while a .sh was loaded into
// the editor, a different surface reached by a different gesture, so the same
// action produced two unrelated outcomes depending on the extension, and with
// the Editor column toggled off the .sh case showed nothing at all.
//
// One gesture, one destination: everything previewable previews. The preview's
// Edit button still hands the file to the editor, and "View source" in the
// context menu still goes there directly, so the editing path is a choice
// rather than a surprise. The rule lives here so the next surface imports it
// instead of authoring a fifth opinion.

import { isPreviewable as isMediaPreviewable } from '../components/Preview/utils/fileTypes';

export type PreviewRoute =
    /** Universal Preview: media, PDF, markdown, plain text and source code. */
    | 'universal-preview'
    /** Nothing opens; the gesture is a no-op on this file. */
    | 'none';

/**
 * The destination for a double-click / Space on `filename`.
 *
 * There is deliberately no editor arm: `getPreviewCategory` consults the
 * editor's own language map, so every name the editor can open is a name the
 * preview renders. `previewRoute.test.ts` pins that containment, because
 * without it a file the editor alone recognised would open nowhere.
 */
export function previewRouteFor(filename: string): PreviewRoute {
    return isMediaPreviewable(filename) ? 'universal-preview' : 'none';
}
