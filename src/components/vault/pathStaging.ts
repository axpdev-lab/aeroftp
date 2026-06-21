// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';

/**
 * Split a flat list of local OS paths (e.g. from a File-Explorer drag&drop or a
 * mixed file/folder picker) into files vs directories, so callers can route each
 * to the right backend command (`vault_*_add_files` for files,
 * `vault_*_add_directory` for folders).
 *
 * Determining file-vs-dir is done by listing each path's PARENT directory with
 * the existing `get_local_files` command and reading the `is_dir` flag off the
 * matching entry. This reuses a command the AeroFile panel already calls (so
 * path validation/scope is known to work for local paths) and avoids the
 * `@tauri-apps/plugin-fs` `stat`, which is fs-scope-gated and would reject
 * arbitrary dropped paths. Unstattable paths fall back to being treated as
 * files (best-effort, the add_files call will surface any real error).
 *
 * The match is by BASENAME within the listed parent (entry names are unique per
 * directory), not by full-path string equality: a folder picker / XDG portal can
 * return a directory path with a trailing slash or a differently-canonicalized
 * form than `get_local_files` emits, and a full-path mismatch would misclassify
 * the directory as a file -> it would then hit `vault_*_add_files`, whose raw
 * read on a directory fails with EISDIR ("is a directory", os error 21).
 */
export interface SplitPaths {
    files: string[];
    dirs: string[];
}

interface LocalEntry {
    name: string;
    path: string;
    is_dir: boolean;
}

const norm = (p: string): string => p.replace(/\\/g, '/');
const stripTrailing = (p: string): string => p.replace(/\/+$/, '') || '/';
const baseOf = (p: string): string => stripTrailing(p).split('/').pop() || p;
const parentOf = (p: string): string => {
    const cut = p.replace(/\/+$/, '').replace(/\/[^/]*$/, '');
    return cut || '/';
};

export async function splitPathsByType(rawPaths: string[]): Promise<SplitPaths> {
    const files: string[] = [];
    const dirs: string[] = [];
    const paths = rawPaths.map(norm).filter(Boolean);

    // Group by parent so we list each containing directory at most once.
    const byParent = new Map<string, string[]>();
    for (const p of paths) {
        const parent = parentOf(p);
        const group = byParent.get(parent);
        if (group) group.push(p);
        else byParent.set(parent, [p]);
    }

    for (const [parent, group] of byParent) {
        // Map each child's basename -> is_dir. Basenames are unique within a
        // directory, so this is robust to trailing-slash / canonicalization
        // differences between the picker's path and get_local_files' path.
        let dirNames = new Set<string>();
        try {
            const listing = await invoke<LocalEntry[]>('get_local_files', {
                path: parent,
                showHidden: true,
            });
            dirNames = new Set(listing.filter((e) => e.is_dir).map((e) => baseOf(norm(e.path))));
        } catch {
            // Parent unreadable: treat every path in this group as a file.
        }
        for (const p of group) {
            if (dirNames.has(baseOf(p))) dirs.push(p);
            else files.push(p);
        }
    }

    return { files, dirs };
}
