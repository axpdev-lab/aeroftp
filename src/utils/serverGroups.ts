import { secureGetWithFallback, secureStoreAndClean } from './secureStorage';

// Server groups are the generalisation of the ⭐ favourites: where a favourite
// is a single anonymous bucket of server ids, a group is a *named* bucket. They
// live in the vault under `server_groups` (the secureStorage layer prefixes it
// to `config_server_groups`) with a legacy localStorage copy under
// `aeroftp-server-groups` as an offline fallback, exactly like
// `favoriteServers.ts`. Membership is stored here in the group blob, NOT on the
// ServerProfile, so the profile schema is untouched and an unknown id simply
// renders nothing (graceful, forward/backward compatible). See discussion #320.
export const GROUPS_VAULT_KEY = 'server_groups';
export const GROUPS_STORAGE_KEY = 'aeroftp-server-groups';

export interface ServerGroup {
    /** Stable id: `grp_${ts}_${rand}`. */
    id: string;
    /** User-facing name, e.g. "Production". */
    name: string;
    /** Optional chip tint (CSS colour). Reserved for future palette use. */
    color?: string;
    /** Manual ordering of the chips/sections. */
    order: number;
    /** ServerProfile ids that belong to this group. */
    members: string[];
}

export function newServerGroupId(): string {
    return `grp_${Date.now()}_${Math.random().toString(36).slice(2, 11)}`;
}

/**
 * Move the group `id` to a new 0-based position and re-stamp a dense, gap-free
 * `order` (0..N) on the whole list. Pure mirror of the CLI `apply_group_reorder`
 * so the GUI drag-reorder and `aeroftp-cli groups -i` re-index converge on the
 * same vault order. `targetIndex` is clamped to `0..len-1`. Returns the original
 * array unchanged (same reference) when the id is unknown or the move is a no-op.
 */
export function reorderServerGroups(
    groups: ServerGroup[],
    id: string,
    targetIndex: number,
): ServerGroup[] {
    const src = groups.findIndex((g) => g.id === id);
    if (src < 0) return groups;
    const dst = Math.max(0, Math.min(groups.length - 1, targetIndex));
    if (dst === src) return groups;
    const next = groups.slice();
    const [moved] = next.splice(src, 1);
    next.splice(dst, 0, moved);
    return next.map((g, i) => (g.order === i ? g : { ...g, order: i }));
}

/**
 * Coerce an untrusted payload (vault or localStorage) into a clean, sorted
 * `ServerGroup[]`. Drops malformed entries instead of throwing so a single bad
 * record never blanks the whole list.
 */
export function normalizeServerGroups(raw: unknown): ServerGroup[] {
    if (!Array.isArray(raw)) return [];
    const groups: ServerGroup[] = [];
    for (const entry of raw) {
        if (!entry || typeof entry !== 'object') continue;
        const g = entry as Partial<ServerGroup>;
        if (typeof g.id !== 'string' || typeof g.name !== 'string') continue;
        groups.push({
            id: g.id,
            name: g.name,
            color: typeof g.color === 'string' ? g.color : undefined,
            order: typeof g.order === 'number' ? g.order : groups.length,
            members: Array.isArray(g.members)
                ? g.members.filter((m): m is string => typeof m === 'string')
                : [],
        });
    }
    return groups.sort((a, b) => a.order - b.order);
}

/** Synchronous read from the localStorage fallback (for first-paint seeding). */
export function readServerGroupsFromLocalStorage(): ServerGroup[] {
    try {
        const stored = localStorage.getItem(GROUPS_STORAGE_KEY);
        return stored ? normalizeServerGroups(JSON.parse(stored)) : [];
    } catch {
        return [];
    }
}

/** Authoritative read from the vault, falling back to localStorage. */
export async function loadServerGroups(): Promise<ServerGroup[]> {
    try {
        const groups = await secureGetWithFallback<unknown>(GROUPS_VAULT_KEY, GROUPS_STORAGE_KEY);
        return normalizeServerGroups(groups);
    } catch {
        return readServerGroupsFromLocalStorage();
    }
}

/** Dual-write to the vault + localStorage backup. Best-effort. */
export async function saveServerGroups(groups: ServerGroup[]): Promise<void> {
    await secureStoreAndClean(GROUPS_VAULT_KEY, GROUPS_STORAGE_KEY, groups);
}

/**
 * Carry a profile's group memberships from one id to another when a saved
 * profile is converted/duplicated (#215 delete+recreate flow), mirroring
 * `carryFavoriteServer`. No-op when the source belonged to no group.
 *
 * @param oldId    id of the source profile.
 * @param newId    id of the freshly-created profile.
 * @param removeOld true for an in-place convert (the old profile is gone);
 *                  false for a duplicate (the old profile stays in its groups).
 */
export async function carryServerGroups(
    oldId: string,
    newId: string,
    removeOld: boolean,
): Promise<void> {
    try {
        const groups = await loadServerGroups();
        let changed = false;
        const next = groups.map((g) => {
            if (!g.members.includes(oldId)) return g;
            changed = true;
            const members = new Set(g.members);
            if (removeOld) members.delete(oldId);
            members.add(newId);
            return { ...g, members: [...members] };
        });
        if (changed) await saveServerGroups(next);
    } catch {
        // Group membership is best-effort cosmetic state; never block a
        // profile switch on a vault hiccup.
    }
}

/** Remove a deleted profile id from every group. Best-effort. */
export async function pruneServerFromGroups(serverId: string): Promise<void> {
    try {
        const groups = await loadServerGroups();
        let changed = false;
        const next = groups.map((g) => {
            if (!g.members.includes(serverId)) return g;
            changed = true;
            return { ...g, members: g.members.filter((m) => m !== serverId) };
        });
        if (changed) await saveServerGroups(next);
    } catch {
        // ignore
    }
}
