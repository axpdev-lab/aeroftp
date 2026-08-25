// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Map a reordered FormTab id sequence back onto the full form-tab state.
 *
 * The header's FormTab type does not carry connectionParams. Storing the
 * array it hands back would drop typed fields; lookup by id keeps them
 * attached to the same tab after a drag.
 */
export function applyFormTabReorder<T extends { id: string }>(
    current: readonly T[],
    reordered: readonly { id: string }[],
): T[] {
    const byId = new Map(current.map((ft) => [ft.id, ft]));
    return reordered.map((ft) => byId.get(ft.id)).filter((ft): ft is T => !!ft);
}
