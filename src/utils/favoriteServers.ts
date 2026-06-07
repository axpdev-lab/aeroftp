import { secureGetWithFallback, secureStoreAndClean } from './secureStorage';

// Favourites live in the vault under `favorite_servers` (the secureStorage
// layer prefixes it to `config_favorite_servers`, which the CLI reads to render
// the ⭐ column). A legacy localStorage copy under `aeroftp-favorite-servers`
// is kept as an offline fallback. Both keys are shared by MyServersPanel (the
// toggle UI) and ConnectionScreen (carry-over on protocol switch), so they live
// here as the single source of truth.
export const FAVORITES_VAULT_KEY = 'favorite_servers';
export const FAVORITES_STORAGE_KEY = 'aeroftp-favorite-servers';

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
        const favs = await secureGetWithFallback<string[]>(
            FAVORITES_VAULT_KEY,
            FAVORITES_STORAGE_KEY,
        );
        if (!Array.isArray(favs) || !favs.includes(oldId)) return;
        const next = new Set(favs);
        if (removeOld) next.delete(oldId);
        next.add(newId);
        await secureStoreAndClean(FAVORITES_VAULT_KEY, FAVORITES_STORAGE_KEY, [...next]);
    } catch {
        // Favourites are best-effort cosmetic state; never block a profile
        // switch on a vault hiccup.
    }
}
