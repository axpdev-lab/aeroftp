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

/** Why a path a user wrote or saved cannot be used as a location. */
export type LocationProblem = 'parent' | 'current' | 'empty' | 'nul';

/**
 * Whether a path a user wrote or saved may be used as a location: joined to
 * a root, stripped of a prefix, or compared with a folder boundary. The same
 * rule as the backend's `util::path_location::location_problem`, with the
 * cases both sides pass in `src/shared/path-location-cases.json`. `/` and
 * `\` both end a step; a `.` or `..` step, a NUL and an empty step are
 * refused, never collapsed into the folder they might mean. An empty step is
 * allowed before a leading separator, as the second one of a leading `\\`
 * (a Windows UNC path) and after a single trailing separator. The empty path
 * is not refused here: each caller says what it means.
 */
export function locationProblem(path: string): LocationProblem | null {
    if (path.includes('\0')) return 'nul';
    const steps = path.split(/[\\/]/u);
    const last = steps.length - 1;
    for (let index = 0; index < steps.length; index++) {
        const step = steps[index];
        if (step === '..') return 'parent';
        if (step === '.') return 'current';
        if (step !== '') continue;
        if (index === 0) continue;
        if (index === last && (steps[index - 1] !== '' || index === 1)) continue;
        if (index === 1 && path.startsWith('\\\\')) continue;
        return 'empty';
    }
    return null;
}

const PROBLEM_REASON: Record<LocationProblem, string> = {
    parent: "a '..' step",
    current: "a '.' step",
    empty: 'an empty step (a doubled slash)',
    nul: 'a NUL character',
};

/**
 * The refusal of a location: `what` it is and why, never the path. A refused
 * value is hand-written or saved by an older version, may be a URL with a
 * password or carry a token in a step or a query, and the message is shown,
 * logged and copied into reports; a redaction that recognises secrets by
 * their shape cannot know every one. The backend twin is
 * `util::path_location::unusable`, with the same text.
 */
export function locationRefusal(what: string, problem: LocationProblem): string {
    return `The ${what} contains ${PROBLEM_REASON[problem]}: write it as a plain path, without '.' or '..' steps or doubled slashes`;
}

/**
 * Whether a location saved before the rule above may still be used: the
 * backend's `util::path_location::saved_location_problem`, with the same
 * shared table. Only a `..` step (either separator) and a NUL are refused;
 * a `.` or empty step is used as saved, byte for byte, as it was before.
 */
export function savedLocationProblem(path: string): LocationProblem | null {
    if (path.includes('\0')) return 'nul';
    return path.split(/[\\/]/u).includes('..') ? 'parent' : null;
}

/**
 * A Windows path: a drive letter (`C:\`, `C:/`) or a UNC share (`\\host`).
 * Only there is `\` a separator; on POSIX it is a character of the name.
 */
const isWindowsPath = (path: string): boolean => /^[A-Za-z]:[\\/]/u.test(path) || path.startsWith('\\\\');

/**
 * `path` and `base` with one separator: when either is a Windows path, both
 * have every `\` written as `/`, so `C:\Users\u` and `C:/Users/u` are one
 * folder; a POSIX pair is kept byte for byte.
 */
const withOneSeparator = (path: string, base: string): [string, string] => isWindowsPath(path) || isWindowsPath(base)
    ? [path.replace(/\\/gu, '/'), base.replace(/\\/gu, '/')]
    : [path, base];

/** A component boundary is required; a sibling such as /base-other is outside. */
export function relativeSyncSegments(path: string, base: string): string[] | null {
    if (locationProblem(path) || locationProblem(base)) return null;
    const [onePath, oneBase] = withOneSeparator(path, base);
    const root = rootPath(oneBase);
    const target = rootPath(onePath);
    if (target === root) return [];
    const prefix = root.endsWith('/') ? root : `${root}/`;
    if (!target.startsWith(prefix)) return null;
    const parts = target.slice(prefix.length).split('/').filter(Boolean);
    return parts.some(p => p === '.' || p === '..' || p.includes('\0')) ? null : parts;
}

/**
 * Whether `path` is `folder` or under it, at a component boundary: a sibling
 * such as `/docs2` is not under `/docs`. On a Windows path either separator
 * ends a component (`C:/Users/u/AeroCloud/a` is under `C:\Users\u\AeroCloud`);
 * on POSIX only `/` does, so `/home/u/AeroCloud\a` is a sibling named with a
 * backslash. Empty names nothing, and a path that is not plain
 * (`locationProblem`) is under no folder: `/docs/../private` starts with
 * `/docs` and names a folder outside it.
 */
export function isWithinSyncFolder(path: string, folder: string): boolean {
    if (!path || !folder) return false;
    if (locationProblem(path) || locationProblem(folder)) return false;
    const [onePath, oneFolder] = withOneSeparator(path, folder);
    const root = oneFolder.replace(/\/+$/u, '');
    if (!root) return onePath.startsWith('/');
    return onePath === root || onePath === oneFolder || onePath.startsWith(`${root}/`);
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
