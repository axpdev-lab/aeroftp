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
// queue is dropped. A folder that already has a tab reuses it.
//
// Windows Explorer opens a multi-selection with "%1" as one process per file
// while the app runs, so the files arrive as separate batches a moment apart.
// A batch for the folder the panel just opened, within OPEN_MERGE_WINDOW_MS of
// the previous one, adds to that selection instead of replacing it.

import type { OpenRequest } from '../types';
import type { LocalTab } from '../types/aerofile';
import { isWindowsLocalPath, normalizeLocalPath } from './localPath';

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

/** How close two OS open batches on one folder must be to merge. */
export const OPEN_MERGE_WINDOW_MS = 3000;

export interface LastLocalOpen {
    panelId: string;
    folder: string;
    names: string[];
    at: number;
}

/** The selection when `names` joins the previous batch, or `null` when the
 *  batch stands alone (another panel or folder, too late, or a folder open). */
export function mergeOpenBatch(
    last: LastLocalOpen | null,
    panelId: string,
    folder: string,
    names: readonly string[],
    now: number,
    windowMs = OPEN_MERGE_WINDOW_MS,
): string[] | null {
    if (!last || names.length === 0 || last.panelId !== panelId || !sameLocalFolder(last.folder, folder)) return null;
    if (now - last.at > windowMs || now < last.at) return null;
    return [...last.names, ...names.filter(name => !last.names.includes(name))];
}

/** Whether two local folder paths name the same folder. On Windows the
 *  separators are unified and case is ignored (`C:\\A` is `c:/a/`); on Unix a
 *  backslash belongs to a name and only a trailing `/` is dropped. */
export function sameLocalFolder(a: string, b: string): boolean {
    const key = (path: string) => {
        const normalized = normalizeLocalPath(path);
        const trimmed = /^[A-Za-z]:\/$/.test(normalized) || normalized === '/'
            ? normalized
            : normalized.replace(/\/+$/, '');
        return isWindowsLocalPath(path) ? trimmed.toLowerCase() : trimmed;
    };
    return key(a) === key(b);
}

/** The tab already showing `folder`, if any. */
export function findTabFor(tabs: readonly LocalTab[], folder: string): LocalTab | undefined {
    return tabs.find(tab => sameLocalFolder(tab.path, folder));
}

/** The tabs with one more per further folder of a batch, each selecting its
 *  files when first shown; a folder that already has a tab gets the files
 *  added to that tab instead. At most `max` tabs in all. */
export function addFolderTabs(
    tabs: readonly LocalTab[],
    groups: readonly LocalOpenGroup[],
    newTab: (group: LocalOpenGroup) => LocalTab,
    max = 12,
): LocalTab[] {
    const next = [...tabs];
    for (const group of groups) {
        const index = next.findIndex(tab => sameLocalFolder(tab.path, group.folder));
        if (index >= 0) {
            const select = [...(next[index].select ?? [])];
            for (const name of group.names) if (!select.includes(name)) select.push(name);
            if (select.length > 0) next[index] = { ...next[index], select };
        } else if (next.length < max) {
            next.push(newTab(group));
        }
    }
    return next;
}

/** Opening `names` of `folder` in the tab that already shows it: the names
 *  that tab still had waiting for its first showing join them, and the tabs
 *  come back without them. The caller applies `tabs` only once the panel has
 *  reached the folder, so a failed navigation keeps them waiting. */
export function openInFolderTab(
    tabs: readonly LocalTab[],
    folder: string,
    names: readonly string[],
): { tab: LocalTab | undefined; names: string[]; tabs: LocalTab[] } {
    const tab = findTabFor(tabs, folder);
    if (!tab) return { tab, names: [...names], tabs: [...tabs] };
    const taken = takeTabSelection(tabs, tab.id);
    return { tab, names: [...taken.select, ...names.filter(name => !taken.select.includes(name))], tabs: taken.tabs };
}
