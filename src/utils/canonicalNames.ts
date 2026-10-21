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
