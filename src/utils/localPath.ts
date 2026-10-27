// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** Local Windows syntax; a slash-only UNC prefix requires a Windows host. */
export function isWindowsLocalPath(path: string): boolean {
    return /^[A-Za-z]:(?:[\\/]|$)/.test(path) || path.startsWith('\\\\')
        || (path.startsWith('//') && typeof navigator !== 'undefined' && /^Win/i.test(navigator.platform));
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

/** Remote entries must remain a single leaf when mapped to a local download root. */
export function isSafeRemoteDownloadName(name: string): boolean {
    return name.length > 0 && name !== '.' && name !== '..'
        && !name.includes('/') && !name.includes('\\') && !name.includes('\0');
}
