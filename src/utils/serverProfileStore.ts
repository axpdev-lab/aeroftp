// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import type { ServerProfile } from '../types';
import { secureGet, secureStore } from './secureStorage';
import {
    accountChangeGeneration,
    loadActiveServerProfiles,
    loadActiveServerProfilesScoped,
    saveActiveServerProfiles,
    storeLegacyServerProfiles,
} from './userPartitions';

export const SAVED_SERVERS_ACCOUNT = 'server_profiles';
export const SAVED_SERVERS_STORAGE_KEY = 'aeroftp-saved-servers';

let profileWriteQueue: Promise<void> = Promise.resolve();

const readLocalProfiles = (): ServerProfile[] => {
    try {
        const stored = localStorage.getItem(SAVED_SERVERS_STORAGE_KEY);
        return stored ? JSON.parse(stored) : [];
    } catch {
        return [];
    }
};

const canUseLegacyProfileFallback = (error: unknown): boolean => {
    const message = String(error ?? '');
    return (
        message.includes('STORE_NOT_READY') ||
        message.includes('unknown command') ||
        message.includes('not found') ||
        message.includes('failed to invoke') ||
        message.includes('window.__TAURI_INTERNALS__')
    );
};

const loadLegacySavedServerProfiles = async (): Promise<ServerProfile[]> => {
    const vaultProfiles = await secureGet<ServerProfile[]>(SAVED_SERVERS_ACCOUNT);
    if (vaultProfiles !== null) return vaultProfiles;

    const legacy = readLocalProfiles();
    if (legacy.length > 0) {
        try {
            await secureStore(SAVED_SERVERS_ACCOUNT, legacy);
            localStorage.removeItem(SAVED_SERVERS_STORAGE_KEY);
        } catch {
            // Vault write failure: keep legacy data in localStorage as a
            // best-effort fallback so the user does not lose profiles.
        }
    }
    return legacy;
};

const seedLegacyLocalProfilesForPartitionMigration = async (): Promise<void> => {
    const legacy = readLocalProfiles();
    if (legacy.length === 0) return;

    const vaultProfiles = await secureGet<ServerProfile[]>(SAVED_SERVERS_ACCOUNT);
    if (vaultProfiles !== null) return;

    try {
        await secureStore(SAVED_SERVERS_ACCOUNT, legacy);
        localStorage.removeItem(SAVED_SERVERS_STORAGE_KEY);
    } catch {
        // If the vault is not ready yet, the partition command will also
        // report STORE_NOT_READY and the normal legacy fallback below keeps
        // the profiles visible for this boot.
    }
};

/**
 * Load saved server profiles.
 *
 * Vault is the sole source of truth: once the vault has been initialised
 * for this installation (even if it holds an empty array), localStorage
 * is never consulted. This is what makes two portable installations
 * truly independent: deleting a profile in installation A leaves the
 * vault as `[]`, and installation B's vault is a completely separate
 * file untouched by that delete.
 *
 * Legacy migration: the very first time we run for this installation
 * (vault key has never been written, so `secureGet` returns `null`) we
 * import any profiles still sitting in localStorage from pre-v3.7.8
 * builds, write them into the vault, and clear the localStorage entry
 * so it cannot be re-read by an older co-installed binary.
 */
export const loadSavedServerProfiles = async (): Promise<ServerProfile[]> => {
    try {
        await seedLegacyLocalProfilesForPartitionMigration();
        return await loadActiveServerProfiles();
    } catch (error) {
        if (!canUseLegacyProfileFallback(error)) throw error;
    }
    return loadLegacySavedServerProfiles();
};

/**
 * `loadSavedServerProfiles` for a caller that is going to WRITE the list back.
 *
 * The plain read answers `[]` for two different states: a vault that genuinely
 * holds no profiles, and an active partition it could not reach
 * (`STORE_NOT_READY`) whose legacy store happens to be empty. Both are the
 * right answer for rendering a list, and the second is the wrong answer for a
 * read-modify-write, where persisting `[]` plus the imported profiles drops
 * everything the partition still holds. Here that second state throws, so the
 * caller can refuse instead of writing over what it could not read.
 */
export const loadSavedServerProfilesStrict = async (): Promise<ServerProfile[]> => {
    try {
        await seedLegacyLocalProfilesForPartitionMigration();
        return await loadActiveServerProfiles();
    } catch (error) {
        if (!canUseLegacyProfileFallback(error)) throw error;
        // A legacy store with content is a real answer: it is exactly what the
        // plain read migrates. An empty one, reached only because the active
        // partition refused, is not an answer at all.
        const legacy = await loadLegacySavedServerProfiles();
        if (legacy.length > 0) return legacy;
        throw error;
    }
};

/**
 * A saved-profile list read for a read-modify-write, with the account it was
 * read from. `userId` is absent when the list came from the legacy store,
 * which has no accounts.
 */
export interface SavedProfilesRead {
    profiles: ServerProfile[];
    userId?: number;
    revision?: string;
}

// What the revision of a list read from the legacy store starts with, followed by
// that list. A write carrying it stays in that store: the backend refuses it, under
// the list lock, if the partition can be reached by then or the store no longer
// holds that list.
const LEGACY_STORE_REVISION = 'legacy-store:';

/**
 * `loadSavedServerProfilesStrict` plus the account the list belongs to. Pass
 * `userId` to `storeSavedServerProfiles` so the write lands in that account or
 * is refused (`ACCOUNT_CHANGED`) when another account became active in between:
 * an edit takes seconds, and a switch from the GUI or the CLI in that window
 * used to receive the previous account's list. Pass `revision` with it so that
 * a list another window or process saved in between is refused
 * (`PROFILES_CHANGED`) and not overwritten; it is absent from the legacy store
 * too.
 */
export const readSavedServerProfilesForWrite = async (): Promise<SavedProfilesRead> => {
    try {
        await seedLegacyLocalProfilesForPartitionMigration();
        const scoped = await loadActiveServerProfilesScoped();
        return { profiles: scoped.profiles, userId: scoped.userId, revision: scoped.revision };
    } catch (error) {
        if (!canUseLegacyProfileFallback(error)) throw error;
        const legacy = await loadLegacySavedServerProfiles();
        if (legacy.length > 0) return { profiles: legacy, revision: LEGACY_STORE_REVISION + JSON.stringify(legacy) };
        throw error;
    }
};

/**
 * Persist saved server profiles to the vault and remove any stale
 * localStorage backup. Writing only to the vault prevents bleed-through
 * between co-installed builds (e.g. a portable folder next to an MSI
 * install) once the vault per-installation isolation is in place.
 */
// Custom DOM event fired whenever the saved-server vault is mutated. App.tsx
// listens on `window` and bumps `serversRefreshKey` so the IntroHub card list
// reflects the change without waiting for an unrelated route refresh. Mirrors
// the `transfer-toast-update` / `editor-reload` pattern used elsewhere.
export const PROFILES_CHANGED_EVENT = 'aeroftp-profiles-changed';

// Detail of PROFILES_CHANGED_EVENT. `profiles` is the list a write just stored,
// so a view can show it at once instead of waiting for a re-read (seconds on a
// large vault). It is left out when an account switch or lock overlapped the
// write, since the list may belong to the other account; other dispatchers (an
// account switch) send no list either.
export interface ProfilesChangedDetail {
    connectionMetadata?: boolean;
    profiles?: ServerProfile[];
}

// Changes with every profile write made from this window and every account
// switch or lock. A read that started before a write answers with the list as
// it was before it, and one that started before a switch may answer with the
// other account's list; a view compares the value at both ends of its read and
// drops such an answer (the write's event carried the newer list, and a switch
// is followed by its own profiles-changed event and re-read).
let profilesWriteGeneration = 0;
export const savedProfilesGeneration = (): number => profilesWriteGeneration + accountChangeGeneration();

/**
 * Resolves to the list stored. No two profiles share a name: one saved onto a
 * name another profile holds is stored with the first free " (1)", " (2)",
 * ... (`util::profile_name` in the backend, `uniqueProfileNames` for the
 * legacy store), so a caller that keeps the list shows this one, and the
 * change event carries it.
 */
export const storeSavedServerProfiles = async (
    profiles: ServerProfile[],
    connectionMetadata = false,
    expectedUserId?: number,
    expectedRevision?: string,
): Promise<ServerProfile[]> => {
    const accountBefore = accountChangeGeneration();
    let stored: ServerProfile[];
    try {
        if (expectedRevision?.startsWith(LEGACY_STORE_REVISION)) {
            const expected = JSON.parse(expectedRevision.slice(LEGACY_STORE_REVISION.length)) as ServerProfile[];
            stored = await storeLegacyServerProfiles(profiles, expected);
        } else {
            const written = await saveActiveServerProfiles(profiles, expectedUserId, expectedRevision);
            stored = Array.isArray(written) ? written : profiles;
        }
    } catch (error) {
        // A list bound to an account never falls back to the legacy store, which has
        // no accounts, and one already bound to that store has just been refused.
        if (expectedUserId !== undefined || expectedRevision !== undefined || !canUseLegacyProfileFallback(error)) throw error;
        stored = await storeLegacyServerProfiles(profiles);
    }
    announceStoredServerProfiles(stored, accountBefore, connectionMetadata);
    return stored;
};

/**
 * Tell this window the list changed, for a list the backend stored itself (a
 * `.aeroftp` import). `accountBefore` is `accountChangeGeneration()` from before
 * the write: the list goes out only if no switch overlapped it.
 */
export const announceStoredServerProfiles = (
    stored: ServerProfile[],
    accountBefore: number,
    connectionMetadata = false,
): void => {
    profilesWriteGeneration += 1;
    try {
        localStorage.removeItem(SAVED_SERVERS_STORAGE_KEY);
    } catch {
        // best-effort cleanup
    }
    try {
        const sameAccount = accountChangeGeneration() === accountBefore;
        window.dispatchEvent(new CustomEvent<ProfilesChangedDetail>(PROFILES_CHANGED_EVENT, {
            detail: sameAccount ? { connectionMetadata, profiles: stored } : { connectionMetadata },
        }));
    } catch {
        // SSR / non-DOM environment: dispatch is a best-effort notification.
    }
};

/**
 * Run a read-modify-write on a fresh read, and once more on a new read if another
 * writer saved in between (`PROFILES_CHANGED`). The second try belongs to the account
 * of the first: if the account changed, the data the write was made from may be the
 * previous account's, so it stops with `ACCOUNT_CHANGED` and writes nothing.
 */
export const withFreshSavedProfiles = async <T>(
    write: (read: SavedProfilesRead) => Promise<T>,
    read: () => Promise<SavedProfilesRead> = readSavedServerProfilesForWrite,
): Promise<T> => {
    const accountBefore = accountChangeGeneration();
    const first = await read();
    try {
        return await write(first);
    } catch (error) {
        if (!String(error).includes('PROFILES_CHANGED')) throw error;
        const again = await read();
        if (again.userId !== first.userId || accountChangeGeneration() !== accountBefore) {
            throw new Error('ACCOUNT_CHANGED: the active account changed since these profiles were read; nothing was saved');
        }
        return write(again);
    }
};

export const mergeSavedServerProfile = async (
    profileId: string,
    updater: (profile: ServerProfile) => ServerProfile | Promise<ServerProfile>,
    connectionMetadata = false,
): Promise<ServerProfile[]> => {
    let result: ServerProfile[] = [];
    // Same outcome as the plain read: nothing reachable to merge into.
    const readForMerge = () => readSavedServerProfilesForWrite().catch((error: unknown): SavedProfilesRead => {
        if (!canUseLegacyProfileFallback(error)) throw error;
        return { profiles: [] };
    });
    const run = () => withFreshSavedProfiles(async read => {
        const profiles = read.profiles;
        let found = false;
        const next: ServerProfile[] = [];
        for (const profile of profiles) {
            if (profile.id !== profileId) {
                next.push(profile);
                continue;
            }
            found = true;
            // The updater may read this profile and await work before returning.
            // The write queue holds the turn until that promise settles.
            next.push(await updater(profile));
        }
        result = found ? next : profiles;
        // As stored: a name another profile holds comes back as " (1)".
        if (found) result = await storeSavedServerProfiles(result, connectionMetadata, read.userId, read.revision);
    }, readForMerge);

    const queued = profileWriteQueue.then(run, run);
    profileWriteQueue = queued.then(() => undefined, () => undefined);
    await queued;
    return result;
};

/**
 * Replace the active account's list with one that is meant to replace it whole (a
 * migration), bound to the account and the revision read just before.
 */
export const replaceSavedServerProfiles = (profiles: ServerProfile[]): Promise<ServerProfile[]> =>
    withFreshSavedProfiles(({ userId, revision }) => storeSavedServerProfiles(profiles, false, userId, revision));

/**
 * After a keystore restore, which stored the list itself: read it and tell this
 * window. Resolves to the list.
 */
export const reloadRestoredServerProfiles = async (): Promise<ServerProfile[]> => {
    const accountBefore = accountChangeGeneration();
    const profiles = await loadSavedServerProfiles();
    announceStoredServerProfiles(profiles, accountBefore);
    return profiles;
};

/**
 * The list `profiles` with a convert undone: the converted profile (`convertedId`)
 * gives way to `original` where it stands, and everything else stays. Converted
 * profile already deleted: `original` comes back at the end. Its id held again
 * by then: the converted profile only goes, so no id is stored twice.
 */
export const revertConvertedProfile = (
    profiles: ServerProfile[],
    convertedId: string,
    original: ServerProfile,
): ServerProfile[] => {
    if (original.id !== convertedId && profiles.some(s => s.id === original.id)) {
        return profiles.filter(s => s.id !== convertedId);
    }
    return profiles.some(s => s.id === convertedId)
        ? profiles.map(s => (s.id === convertedId ? original : s))
        : [...profiles, original];
};

// Trim a connect-error reason to a single short line for tooltip display.
// We intentionally cap and strip newlines so a multi-paragraph backend
// error does not blow up the card tooltip.
const MAX_ERROR_REASON_CHARS = 280;
const formatConnectErrorReason = (raw: string): string => {
    const single = raw.replace(/\s+/g, ' ').trim();
    return single.length > MAX_ERROR_REASON_CHARS
        ? single.slice(0, MAX_ERROR_REASON_CHARS - 1) + '…'
        : single;
};

export const recordProfileConnectFailure = async (
    profileId: string,
    reason: unknown,
): Promise<void> => {
    const message = formatConnectErrorReason(
        typeof reason === 'string' ? reason : String(reason ?? ''),
    );
    if (!profileId) return;
    await mergeSavedServerProfile(profileId, profile => ({
        ...profile,
        lastConnectionError: {
            timestamp: new Date().toISOString(),
            message,
        },
    }), true);
};

export const clearProfileConnectFailure = async (
    profileId: string,
): Promise<void> => {
    if (!profileId) return;
    await mergeSavedServerProfile(profileId, profile => {
        if (!profile.lastConnectionError) return profile;
        const { lastConnectionError: _omit, ...rest } = profile;
        return rest as ServerProfile;
    }, true);
};
