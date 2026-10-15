// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

export type QuickLookPanelId = 'local' | 'local2';

interface QuickLookEntry {
    name: string;
    is_dir: boolean;
}

export interface QuickLookPanel<F extends QuickLookEntry> {
    sortedFiles: F[];
    currentPath: string;
    setSelection(selection: Set<string>): void;
}

/** Index Space previews in a panel: exactly one selected entry, and it is a file. */
export function quickLookStartIndex(files: readonly QuickLookEntry[], selection: ReadonlySet<string>): number | null {
    if (selection.size !== 1) return null;
    const [name] = selection;
    const index = files.findIndex(file => file.name === name);
    return index !== -1 && !files[index].is_dir ? index : null;
}

/**
 * What the overlay shows, read from the panel that opened Quick Look: in dual
 * mode that can be the second local panel, so the index is meaningless against
 * panel 1's list. Null when the index no longer points into that panel's list.
 */
export function quickLookView<F extends QuickLookEntry>(panel: QuickLookPanel<F>, index: number) {
    const file = panel.sortedFiles[index];
    if (!file) return null;
    return {
        file,
        allFiles: panel.sortedFiles,
        currentPath: panel.currentPath,
        select(to: number) {
            const next = panel.sortedFiles[to];
            if (next) panel.setSelection(new Set([next.name]));
        },
    };
}
