// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * No two saved profiles share a name.
 *
 * Two profiles of one name cannot be told apart in the lists, and
 * `--profile NAME` takes the first, so a profile saved onto a name another
 * one holds gets the first free " (1)", " (2)", ... The backend applies the
 * rule to every write of the profile list (`src-tauri/src/util/profile_name.rs`,
 * which also lets the profile that had the name keep it); this twin covers
 * the writes that go to the legacy store, where the list's order decides.
 */

/** What two names collide on: the CLI selects a profile by its name,
 *  case-insensitive (`resolve_profile_selector`). */
const nameKey = (name: string): string => name.toLowerCase();

/**
 * `incoming` with every name that `existing`, or an earlier profile of
 * `incoming`, already holds replaced by the first free "name (1)",
 * "name (2)", ... Profiles keep their identity when their name is free.
 */
export function withUniqueProfileNames<T extends { name: string }>(
    existing: readonly { name: string }[],
    incoming: readonly T[],
): T[] {
    const taken = new Set(existing.map(p => nameKey(p.name ?? '')));
    return incoming.map(profile => {
        const original = profile.name ?? '';
        let name = original;
        for (let n = 1; taken.has(nameKey(name)); n++) {
            name = `${original} (${n})`;
        }
        taken.add(nameKey(name));
        return name === original ? profile : { ...profile, name };
    });
}
