// SPDX-License-Identifier: GPL-3.0-or-later
export interface NamedEntry { name: string; isDir: boolean }

/** Exact names win; fallback is allowed only for unique canonical siblings. */
export function canonicalNameMatcher(source: NamedEntry[], destination: NamedEntry[]) {
    const index = (entries: NamedEntry[]) => {
        const groups = new Map<string, NamedEntry[]>();
        for (const entry of entries) {
            const key = entry.name.normalize('NFC');
            const group = groups.get(key) ?? [];
            group.push(entry);
            groups.set(key, group);
        }
        return groups;
    };
    const from = index(source);
    const to = index(destination);
    const exact = new Map(destination.map(e => [e.name, e]));
    return (name: string): NamedEntry | undefined => {
        if (exact.has(name)) return exact.get(name);
        const key = name.normalize('NFC');
        const candidates = to.get(key) ?? [];
        if (candidates.length && (from.get(key)?.length !== 1 || candidates.length !== 1)) {
            throw new Error(`Ambiguous Unicode name: ${name}`);
        }
        return candidates[0];
    };
}

export function matchCanonicalName(name: string, source: NamedEntry[], destination: NamedEntry[]): NamedEntry | undefined {
    return canonicalNameMatcher(source, destination)(name);
}

const join = (base: string, name: string) => `${base.replace(/\/$/, '')}/${name}`;

/** Listing failures and ambiguous matches reject; only a proven absence is missing. */
export async function resolveCanonicalNavigation(
    relative: string,
    sourceRoot: string,
    destinationRoot: string,
    listSource: (path: string) => Promise<NamedEntry[]>,
    listDestination: (path: string) => Promise<NamedEntry[]>,
    isCurrent: () => boolean = () => true,
): Promise<{ path: string; missing: boolean }> {
    let source = sourceRoot;
    let destination = destinationRoot;
    const parts = relative.split('/').filter(Boolean);
    for (let i = 0; i < parts.length; i++) {
        const name = parts[i];
        if (!isCurrent()) throw new Error('Navigation superseded');
        const from = await listSource(source);
        if (!isCurrent()) throw new Error('Navigation superseded');
        const to = await listDestination(destination);
        if (!isCurrent()) throw new Error('Navigation superseded');
        const match = matchCanonicalName(name, from, to);
        if (!match) return { path: join(destination, parts.slice(i).join('/')), missing: true };
        if (!match.isDir) throw new Error(`Destination is not a directory: ${match.name}`);
        source = join(source, name);
        destination = join(destination, match.name);
    }
    return { path: destination, missing: false };
}
