// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { LocalFile } from '../types';
import { normalizeLocalPath } from './localPath';
import { parentDir } from './transferQueueJournal';

/**
 * Rows of a local panel, and the names they go by.
 *
 * A row's `name` is the key a selection holds. For an entry of the directory
 * the panel is in it is the file's name. A flattened search (`*`) also lists
 * the entries below it, and names each by its path relative to the panel's
 * directory ("Photos 2026/Summer"): unique across the list, which a bare name
 * is not, but not a file name. Two things follow, and every local action has
 * to mind both:
 *
 * - a selected nested row is not in the panel's listing, which holds the
 *   directory's own entries only, so a selection is resolved against the
 *   rows shown ({@link selectedLocalRows});
 * - where an action needs the file's name (a paste or upload destination, a
 *   rename, a drop) it takes {@link localRowName}, and where it needs the
 *   folder the file is in ("extract here", "show in folder") it takes
 *   {@link localRowFolder}.
 *
 * A nested name always uses `/` (the backend normalizes it), and a file name
 * never holds `/`, so the last `/` splits folder from name on every platform.
 */

/**
 * The rows a local panel's selection names, in the order the panel shows them.
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

/** The file's own name: "Summer" for the row "Photos 2026/Summer". */
export const localRowName = (row: Pick<LocalFile, 'name'>): string =>
    row.name.slice(row.name.lastIndexOf('/') + 1);

/**
 * The folder a row is in, relative to the panel's directory: "Photos 2026"
 * for "Photos 2026/Summer", "" for an entry of the directory itself.
 */
export const localRowFolderKey = (row: Pick<LocalFile, 'name'>): string => {
    const slash = row.name.lastIndexOf('/');
    return slash === -1 ? '' : row.name.slice(0, slash);
};

/**
 * The folder a row is in, as a path: the panel's directory for its own
 * entries, the parent of the row's path for a nested one.
 */
export const localRowFolder = (row: Pick<LocalFile, 'name' | 'path'>, panelDir: string): string =>
    localRowFolderKey(row) === '' ? panelDir : parentDir(row.path);

/** The row as a file of its own folder: the same entry, named by its file name. */
export const asLocalFileEntry = <T extends Pick<LocalFile, 'name'>>(row: T): T =>
    row.name.includes('/') ? { ...row, name: localRowName(row) } : row;

/**
 * Clipboard entries for a local selection: each file by its own name, which is
 * the name a paste gives it in the folder it lands in, and a selected folder
 * with what is below it ({@link selectedLocalTrees}).
 */
export const localClipboardEntries = (
    shown: readonly LocalFile[],
    listing: readonly LocalFile[],
    selection: ReadonlySet<string>,
): { name: string; path: string; is_dir: boolean }[] =>
    selectedLocalTrees(shown, listing, selection)
        .map(f => ({ name: localRowName(f), path: f.path, is_dir: f.is_dir }));

/** A local path compared as a key: `/` separators, no trailing one. */
const pathKey = (path: string): string => normalizeLocalPath(path).replace(/\/+$/, '');

/**
 * The names that selected rows of different folders share. An action that
 * puts the rows into one folder (a paste, an upload, a drop, a copy to the
 * other panel, one archive) would give them one name there, and the later
 * would replace the earlier, so it refuses them. Compared without case, as
 * the file systems and servers that ignore it would. Rows of one folder are
 * not compared with each other: their names already coexist where they are.
 */
export const localRowNameClashes = (rows: readonly Pick<LocalFile, 'name' | 'path'>[]): string[] => {
    const byName = new Map<string, { name: string; folders: Set<string> }>();
    for (const row of rows) {
        const name = localRowName(row);
        const key = name.toLowerCase();
        const seen = byName.get(key) ?? { name, folders: new Set<string>() };
        // The folder from the path: a clipboard entry or a dragged row
        // carries its own name, not the relative one.
        seen.folders.add(pathKey(parentDir(row.path)));
        byName.set(key, seen);
    }
    return [...byName.values()].filter(n => n.folders.size > 1).map(n => n.name);
};

/**
 * The rows without those inside a selected folder: a flattened search can
 * select a folder and entries below it, and the folder already takes them.
 */
export const outermostLocalRows = <T extends Pick<LocalFile, 'path' | 'is_dir'>>(rows: readonly T[]): T[] => {
    const folders = rows.filter(r => r.is_dir).map(r => `${pathKey(r.path)}/`);
    return rows.filter(r => !folders.some(folder => pathKey(r.path).startsWith(folder)));
};

/**
 * The rows an action on the whole selection takes: a selected folder with
 * what is below it, and an entry below it not a second time. A flattened
 * search shows a folder and its entries side by side, and Select All takes
 * them all: a copy, an upload or a drop would put such an entry twice (in its
 * folder and next to it) or refuse its name as a clash with itself, an archive
 * or a vault would hold it twice, a size would count it twice, and a move or a
 * delete would find it gone when its turn came. An action on each row (a
 * rename, a tag) takes {@link selectedLocalRows}.
 */
export const selectedLocalTrees = (
    shown: readonly LocalFile[],
    listing: readonly LocalFile[],
    selection: ReadonlySet<string>,
): LocalFile[] => outermostLocalRows(selectedLocalRows(shown, listing, selection));

/**
 * Whether a local panel shows the entries of `dir`: it is the panel's folder,
 * or the panel's flattened search lists what is below its folder and `dir` is
 * one of those folders. A panel that shows it reloads after an action writes
 * there.
 */
export const localPanelShows = (dir: string, panelDir: string, flattened: boolean): boolean => {
    const d = pathKey(dir) || '/';
    const p = pathKey(panelDir) || '/';
    return d === p || (flattened && d.startsWith(p === '/' ? '/' : `${p}/`));
};

/**
 * Whether a drop on the folder `target` puts a dragged entry into itself:
 * `target` is one of the dragged entries or below one of them. A move there
 * fails halfway through the batch, and a copy copies a folder into what it is
 * copying. Whole components only: `Photos 2026` is not below `Photos`.
 */
export const dropsIntoItself = (target: string, sources: readonly string[]): boolean => {
    const t = pathKey(target) || '/';
    return sources.some(source => {
        const s = pathKey(source) || '/';
        return t === s || t.startsWith(s === '/' ? '/' : `${s}/`);
    });
};

/**
 * The rows deepest first: renaming a folder changes the path of everything
 * below it, so an entry below a renamed folder is renamed before it.
 */
export const deepestLocalRowsFirst = <T extends { path: string }>(rows: readonly T[]): T[] =>
    [...rows].sort((a, b) => pathKey(b.path).split('/').length - pathKey(a.path).split('/').length);

/**
 * The local panel an action without an explicit panel acts on. Panel 2 only
 * while it is on screen: two local panels shown and the remote panel not. The
 * focus keeps naming panel 2 after it is hidden (a server connects, dual mode
 * is turned off), and an action must not reach a panel that is not shown.
 */
export const focusedLocalPanel = (s: {
    isConnected: boolean;
    showRemotePanel: boolean;
    showDualLocalPanel: boolean;
    activeLocalPanelId: 'local' | 'local2';
}): 'local' | 'local2' =>
    (!s.isConnected || !s.showRemotePanel) && s.showDualLocalPanel ? s.activeLocalPanelId : 'local';
