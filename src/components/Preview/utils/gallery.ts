// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Preview gallery: the files of the same kind in the opened file's folder,
 * paged with the arrows, the ← → keys and a touchpad swipe (#128). The
 * gallery wraps around: after the last picture comes the first.
 */

import { getPreviewCategory } from './fileTypes';

export interface GalleryEntry {
    name: string;
    path: string;
    is_dir?: boolean;
}

/** The parent folder of a path, for either separator. */
export function parentDir(path: string): string {
    const i = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
    if (i < 0) return '';
    if (i === 0) return path.slice(0, 1);
    // "C:\" keeps its separator.
    if (i === 2 && path[1] === ':') return path.slice(0, 3);
    return path.slice(0, i);
}

/**
 * The files of the opened file's kind among `entries`, and where the opened
 * file is among them; null when there is nothing to page (the file is not
 * there, or it is the only one of its kind).
 */
export function buildGallery<T extends GalleryEntry>(file: GalleryEntry, entries: readonly T[]): { files: T[]; index: number } | null {
    const kind = getPreviewCategory(file.name);
    const files = entries.filter((f) => !f.is_dir && getPreviewCategory(f.name) === kind);
    const index = files.findIndex((f) => f.path === file.path);
    if (files.length < 2 || index < 0) return null;
    return { files, index };
}

/** Folder order for a gallery read from disk: by name, numbers as numbers. */
export function sortByName<T extends GalleryEntry>(entries: readonly T[]): T[] {
    return [...entries].sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' }));
}

/** The next index, going round: after the last comes the first. */
export function galleryStep(index: number, length: number, direction: 1 | -1): number {
    if (length <= 0) return -1;
    return (index + direction + length) % length;
}
