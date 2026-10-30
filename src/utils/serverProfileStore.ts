// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import type { ServerProfile } from '../types';
import { secureGet, secureStore } from './secureStorage';
import { uniqueProfileNames } from './uniqueProfileName';
import {
    accountChangeGeneration,
    loadActiveServerProfiles,
    loadActiveServerProfilesScoped,
    saveActiveServerProfiles,
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

// The revision of a list read from the legacy store. A write carrying it stays in
// that store: if the partition can be reached by then, this list is older than
// what the partition holds, and the write is refused like any other stale one.
const LEGACY_STORE_REVISION = 'legacy-store';

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
        if (legacy.length > 0) return { profiles: legacy, revision: LEGACY_STORE_REVISION };
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
        if (expectedRevision === LEGACY_STORE_REVISION) {
            await loadActiveServerProfilesScoped();
            throw new Error('PROFILES_CHANGED: the saved profiles were read from the legacy store and the account store is available now; nothing was saved');
        }
        const written = await saveActiveServerProfiles(profiles, expectedUserId, expectedRevision);
        stored = Array.isArray(written) ? written : profiles;
    } catch (error) {
        // A list bound to an account never falls back to the legacy store,
        // which has no accounts.
        if (expectedUserId !== undefined || !canUseLegacyProfileFallback(error)) throw error;
        // The list the legacy store holds decides which profile had a name
        // first, as the partition's does in the backend: a profile saved in
        // front of the one holding its name must not take the name from it.
        const held = (await secureGet<ServerProfile[]>(SAVED_SERVERS_ACCOUNT)) ?? readLocalProfiles();
        stored = uniqueProfileNames(Array.isArray(held) ? held : [], profiles);
        await secureStore(SAVED_SERVERS_ACCOUNT, stored);
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

export const mergeSavedServerProfile = async (
    profileId: string,
    updater: (profile: ServerProfile) => ServerProfile | Promise<ServerProfile>,
    connectionMetadata = false,
): Promise<ServerProfile[]> => {
    let result: ServerProfile[] = [];
    const attempt = async () => {
        let read: SavedProfilesRead;
        try {
            read = await readSavedServerProfilesForWrite();
        } catch (error) {
            // Same outcome as the plain read: nothing reachable to merge into.
            if (!canUseLegacyProfileFallback(error)) throw error;
            read = { profiles: [] };
        }
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
    };
    // A merge is one field of one profile, so it can simply be redone on the list
    // another window or process just saved. Once: a list changing that fast
    // reports the conflict like any other write.
    const run = async () => {
        try {
            await attempt();
        } catch (error) {
            if (!String(error).includes('PROFILES_CHANGED')) throw error;
            await attempt();
        }
    };

    const queued = profileWriteQueue.then(run, run);
    profileWriteQueue = queued.then(() => undefined, () => undefined);
    await queued;
    return result;
};

/**
 * Replace the active account's list with one that is meant to replace it whole (a
 * keystore restore, a migration), bound to the account and the revision read just
 * before. `list` is asked for after that read: a save landing in between is
 * refused, and redone once on what `list` answers then.
 */
export const replaceSavedServerProfiles = async (
    list: () => ServerProfile[] | null | Promise<ServerProfile[] | null>,
): Promise<ServerProfile[] | null> => {
    const attempt = async () => {
        const { userId, revision } = await readSavedServerProfilesForWrite().catch(error => {
            if (!canUseLegacyProfileFallback(error)) throw error;
            return {} as Partial<SavedProfilesRead>;
        });
        const profiles = await list();
        return profiles === null ? null : storeSavedServerProfiles(profiles, false, userId, revision);
    };
    try {
        return await attempt();
    } catch (error) {
        if (!String(error).includes('PROFILES_CHANGED')) throw error;
        return attempt();
    }
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
