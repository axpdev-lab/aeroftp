// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// Where the local files of one OS open request batch go in AeroFile.
//
// A file manager's multi-selection comes from one folder, but a shell can
// pass files from several (`aeroftp /a/x.png /b/y.png`). The first folder
// opens in the active panel with its files selected and the first previewable
// one shown; every other folder gets a tab of its own that selects its files
// the first time it is shown, so nothing the batch drained from the backend
// queue is dropped.

import type { OpenRequest } from '../types';
import type { LocalTab } from '../types/aerofile';

export interface LocalOpenGroup {
    folder: string;
    /** Entries of `folder` to select; empty when only the folder was opened. */
    names: string[];
    /** The entry to preview, the first previewable one of `names`. */
    preview: string | null;
}

/** One group per folder, in the order the batch names them; the first is
 *  the one the active panel opens. */
export function planLocalOpen(requests: readonly OpenRequest[], previewable: (name: string) => boolean): LocalOpenGroup[] {
    const groups: LocalOpenGroup[] = [];
    for (const request of requests) {
        let group = groups.find(g => g.folder === request.folder);
        if (!group) {
            group = { folder: request.folder, names: [], preview: null };
            groups.push(group);
        }
        if (!request.isDir && !group.names.includes(request.name)) group.names.push(request.name);
    }
    for (const group of groups) group.preview = group.names.find(previewable) ?? null;
    return groups;
}

/** The entries a tab was created to select, and the tabs without them: the
 *  selection applies once, the first time the tab is shown. */
export function takeTabSelection(tabs: readonly LocalTab[], id: string): { select: string[]; tabs: LocalTab[] } {
    const select = tabs.find(tab => tab.id === id)?.select ?? [];
    if (select.length === 0) return { select, tabs: [...tabs] };
    return {
        select,
        tabs: tabs.map(tab => {
            if (tab.id !== id) return tab;
            const { select: _taken, ...rest } = tab;
            return rest;
        }),
    };
}
