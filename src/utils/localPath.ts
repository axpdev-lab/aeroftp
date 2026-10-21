// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** Local paths with an explicit Windows drive or UNC prefix. Remote names are opaque. */
export function isWindowsLocalPath(path: string): boolean {
    return /^[A-Za-z]:(?:[\\/]|$)/.test(path) || path.startsWith('\\\\');
}

/** Adapt native local separators without changing legal Unix filename characters. */
export function normalizeLocalPath(path: string): string {
    return isWindowsLocalPath(path) ? path.replace(/\\/g, '/') : path;
}

/** Editor events must identify the same file, including its parent and namespace. */
export function editorPathsMatch(changed: string, opened: string, remote: boolean): boolean {
    return remote ? changed === opened : normalizeLocalPath(changed) === normalizeLocalPath(opened);
}

/** Basename for native picker/drop paths; a Unix backslash belongs to the name. */
export function localPathBasename(path: string): string {
    const normalized = normalizeLocalPath(path).replace(/\/+$/, '');
    return normalized.slice(normalized.lastIndexOf('/') + 1);
}

/** Exact destination submitted by the native file drop handler. */
export function localDropTarget(source: string, destinationDir: string): string | null {
    const name = localPathBasename(source);
    return name ? `${normalizeLocalPath(destinationDir).replace(/\/+$/, '')}/${name}` : null;
}
