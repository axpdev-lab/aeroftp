// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * No two saved profiles share a name.
 *
 * Two profiles of one name cannot be told apart in the lists, and
 * `--profile NAME` takes the first, so a profile saved onto a name another
 * one holds gets the first free " (1)", " (2)", ... The backend applies the
 * rule to every write of the profile list (`src-tauri/src/util/profile_name.rs`);
 * this twin of its `unique_profile_names` covers the writes that go to the
 * legacy store.
 */

/** What two names collide on: the CLI selects a profile by its name,
 *  case-insensitive (`resolve_profile_selector`). */
const nameKey = (name: string): string => name.toLowerCase();

type Named = { id?: string; name?: string };

/**
 * `profiles`, about to replace `previous`, with no two of one name. A
 * profile that keeps the id and the name it had in `previous` keeps its name
 * (the first in list order, when an older list already held two of one
 * name), wherever it sits in the list; a new or renamed profile whose name
 * another one holds gets the first free " (1)", " (2)", ... A profile whose
 * name is kept is returned as it was.
 */
export function uniqueProfileNames<T extends Named>(
    previous: readonly Named[],
    profiles: readonly T[],
): T[] {
    const before = new Map<string, string>();
    for (const p of previous) {
        if (typeof p.id === 'string' && typeof p.name === 'string') before.set(p.id, p.name);
    }
    const keeps = profiles.map(p => typeof p.id === 'string' && typeof p.name === 'string' && before.get(p.id) === p.name);
    const taken = new Set<string>();
    const stored = [...profiles];
    // Settled before any rename, as in the backend: the profiles that had
    // their name claim first, then the others in list order.
    for (const pass of [true, false]) {
        stored.forEach((profile, i) => {
            if (keeps[i] !== pass || typeof profile.name !== 'string') return;
            const original = profile.name;
            let name = original;
            for (let n = 1; taken.has(nameKey(name)); n++) {
                name = `${original} (${n})`;
            }
            taken.add(nameKey(name));
            if (name !== original) stored[i] = { ...profile, name };
        });
    }
    return stored;
}
