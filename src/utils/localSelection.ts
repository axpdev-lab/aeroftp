// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { LocalFile } from '../types';

/**
 * The rows a local panel's selection names, in the order the panel shows them.
 *
 * A selection holds row names. The panel's listing holds only the entries of
 * the directory it is in, while a flattened search (`*`) shows nested ones
 * named by their relative path ("sub/inner"): looked up in the listing alone,
 * a selected nested row is not found, and Properties on it opened nothing.
 * The rows shown are read first; the listing then supplies a selected entry
 * the rows do not show, one a filter hides.
 */
export const selectedLocalRows = (
    shown: readonly LocalFile[],
    listing: readonly LocalFile[],
    selection: ReadonlySet<string>,
): LocalFile[] => {
    const rows: LocalFile[] = [];
    const found = new Set<string>();
    for (const f of [...shown, ...listing]) {
        if (!selection.has(f.name) || found.has(f.name)) continue;
        found.add(f.name);
        rows.push(f);
    }
    return rows;
};
