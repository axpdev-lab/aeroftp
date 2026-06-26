import { secureGet } from './secureStorage';
import { getActiveUserSetting, setActiveUserSetting } from './userPartitions';

// Favourites are PER-USER (Ehud #311): they live in the active user's encrypted
// partition under the `favorite_servers` setting scope, which the CLI reads to
// render the ⭐ column. They predate the multi-user system, so the first read
// for a user best-effort seeds from the legacy GLOBAL vault blob
// (`config_favorite_servers`, reached via `secureGet(FAVORITES_VAULT_KEY)`).
// The old `aeroftp-favorite-servers` localStorage mirror is intentionally NOT
// used here: localStorage is per-origin, not per-user, so caching favourites
// there would leak one local user's set to another. Both consumers
// (MyServersPanel toggle UI, ConnectionScreen carry-over) go through the
// load/save helpers below, the single source of truth.
export const FAVORITES_VAULT_KEY = 'favorite_servers';

/**
 * Load the active user's favourite server ids from their partition. On the
 * first read for a user (no per-user value yet) it best-effort seeds, once,
 * from the legacy global blob so favourites created before multi-user carry
 * over. Returns [] on any failure (favourites are cosmetic, never hard-fail).
 */
export async function loadFavoriteServers(): Promise<string[]> {
    try {
        const perUser = await getActiveUserSetting<unknown>(FAVORITES_VAULT_KEY);
        if (perUser != null) {
            return Array.isArray(perUser)
                ? perUser.filter((id): id is string => typeof id === 'string')
                : [];
        }
        // First read for this user: seed once from the legacy global blob.
        const legacy = await secureGet<unknown>(FAVORITES_VAULT_KEY);
        const seeded = Array.isArray(legacy)
            ? legacy.filter((id): id is string => typeof id === 'string')
            : [];
        if (seeded.length > 0) await setActiveUserSetting(FAVORITES_VAULT_KEY, seeded);
        return seeded;
    } catch {
        return [];
    }
}

/** Persist the active user's favourite server ids to their partition. */
export async function saveFavoriteServers(ids: string[]): Promise<void> {
    await setActiveUserSetting(FAVORITES_VAULT_KEY, ids);
}

/**
 * Carry the ⭐ favourite flag from one profile id to another when a saved
 * profile's protocol is switched (issue #215). The convert flow deletes the old
 * profile and creates a new one, so without this the star would silently
 * vanish. No-op when the source was not a favourite.
 *
 * @param oldId    id of the source profile.
 * @param newId    id of the freshly-created profile.
 * @param removeOld true for an in-place convert (the old profile is gone);
 *                  false for a duplicate (the old profile stays, so both keep
 *                  the star).
 */
export async function carryFavoriteServer(
    oldId: string,
    newId: string,
    removeOld: boolean,
): Promise<void> {
    try {
        const favs = await loadFavoriteServers();
        if (!favs.includes(oldId)) return;
        const next = new Set(favs);
        if (removeOld) next.delete(oldId);
        next.add(newId);
        await saveFavoriteServers([...next]);
    } catch {
        // Favourites are best-effort cosmetic state; never block a profile
        // switch on a vault hiccup.
    }
}
