// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// Where the local files of one OS open request batch go in AeroFile.
//
// A file manager's multi-selection comes from one folder, but a shell can
// pass files from several (`aeroftp /a/x.png /b/y.png`). The first folder
// opens in the active panel with its files selected and the first previewable
// one shown; every other folder gets a tab of its own, so nothing the batch
// drained from the backend queue is dropped.

import type { OpenRequest } from '../types';

export interface LocalOpenPlan {
    /** The folder the active panel navigates to. */
    folder: string;
    /** Entries of `folder` to select; empty when only a folder was opened. */
    names: string[];
    /** The entry to preview, the first previewable one of `names`. */
    preview: string | null;
    /** The other folders, in order, each for a new tab. */
    otherFolders: string[];
}

export function planLocalOpen(requests: readonly OpenRequest[], previewable: (name: string) => boolean): LocalOpenPlan | null {
    if (requests.length === 0) return null;
    const folder = requests[0].folder;
    const names = requests.filter(r => !r.isDir && r.folder === folder).map(r => r.name);
    const otherFolders: string[] = [];
    for (const request of requests) {
        if (request.folder !== folder && !otherFolders.includes(request.folder)) otherFolders.push(request.folder);
    }
    return { folder, names, preview: names.find(previewable) ?? null, otherFolders };
}
