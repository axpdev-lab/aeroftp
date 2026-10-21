// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

export interface SyncNavigationEntry {
    name: string;
    is_dir: boolean;
}

export type SyncNavigationResolution =
    | { kind: 'found' | 'missing'; path: string }
    | { kind: 'conflict'; path: string; candidates: string[] }
    | { kind: 'cancelled' };

const rootPath = (path: string): string => /^[A-Za-z]:\/+$/u.test(path)
    ? `${path.slice(0, 2)}/`
    : path.replace(/\/+$/u, '') || '/';
const join = (base: string, name: string): string => `${base.replace(/\/+$/u, '')}/${name}`;

/** A component boundary is required; a sibling such as /base-other is outside. */
export function relativeSyncSegments(path: string, base: string): string[] | null {
    const root = rootPath(base);
    const target = rootPath(path);
    if (target === root) return [];
    const prefix = root.endsWith('/') ? root : `${root}/`;
    if (!target.startsWith(prefix)) return null;
    const parts = target.slice(prefix.length).split('/').filter(Boolean);
    return parts.some(p => p === '.' || p === '..' || p.includes('\0')) ? null : parts;
}

/** Display coherence only; this equality never chooses a destination for I/O. */
export function canonicalSyncSuffixEqual(left: string, right: string): boolean {
    return left.normalize('NFC') === right.normalize('NFC');
}

/**
 * Resolve against actual destination listings. Exact names win; canonical
 * fallback requires a unique directory in both inventories. Each selected spelling remains
 * untouched for the next listing and final navigation. Listing errors propagate
 * and must never be interpreted as proof that a directory is missing.
 */
export async function resolveSyncNavigationPath(
    base: string,
    segments: readonly string[],
    listEntries: (path: string) => Promise<readonly SyncNavigationEntry[]>,
    source: { base: string; listEntries: (path: string) => Promise<readonly SyncNavigationEntry[]> },
    isCurrent: () => boolean = () => true,
): Promise<SyncNavigationResolution> {
    let current = rootPath(base);
    let sourceParent = rootPath(source.base);
    if (segments.some(name => !name || name === '.' || name === '..' || name.includes('/') || name.includes('\0'))) {
        return { kind: 'conflict', path: current, candidates: [] };
    }
    for (let i = 0; i < segments.length; i++) {
        if (!isCurrent()) return { kind: 'cancelled' };
        const name = segments[i];
        const entries = await listEntries(current);
        if (!isCurrent()) return { kind: 'cancelled' };
        const exact = entries.filter(e => e.name === name);
        const matches = exact.length > 0 ? exact : entries.filter(e => e.name.normalize('NFC') === name.normalize('NFC'));
        if (matches.length === 0) {
            return { kind: 'missing', path: segments.slice(i).reduce(join, current) };
        }
        if (matches.length !== 1 || !matches[0].is_dir) {
            return { kind: 'conflict', path: join(current, name), candidates: matches.map(e => join(current, e.name)) };
        }
        if (exact.length === 0) {
            const siblings = (await source.listEntries(sourceParent)).filter(e => e.name.normalize('NFC') === name.normalize('NFC'));
            if (!isCurrent()) return { kind: 'cancelled' };
            if (siblings.length !== 1 || siblings[0].name !== name || !siblings[0].is_dir) {
                return { kind: 'conflict', path: join(current, name), candidates: siblings.map(e => join(sourceParent, e.name)) };
            }
        }
        current = join(current, matches[0].name);
        sourceParent = join(sourceParent, name);
    }
    return isCurrent() ? { kind: 'found', path: current } : { kind: 'cancelled' };
}
