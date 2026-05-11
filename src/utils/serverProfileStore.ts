// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import type { ServerProfile } from '../types';
import { secureGet, secureStore } from './secureStorage';

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

/**
 * Persist saved server profiles to the vault and remove any stale
 * localStorage backup. Writing only to the vault prevents bleed-through
 * between co-installed builds (e.g. a portable folder next to an MSI
 * install) once the vault per-installation isolation is in place.
 */
export const storeSavedServerProfiles = async (profiles: ServerProfile[]): Promise<void> => {
    await secureStore(SAVED_SERVERS_ACCOUNT, profiles);
    try {
        localStorage.removeItem(SAVED_SERVERS_STORAGE_KEY);
    } catch {
        // best-effort cleanup
    }
};

export const mergeSavedServerProfile = async (
    profileId: string,
    updater: (profile: ServerProfile) => ServerProfile,
): Promise<ServerProfile[]> => {
    let result: ServerProfile[] = [];
    const run = async () => {
        const profiles = await loadSavedServerProfiles();
        let found = false;
        const next = profiles.map(profile => {
            if (profile.id !== profileId) return profile;
            found = true;
            return updater(profile);
        });
        result = found ? next : profiles;
        if (found) await storeSavedServerProfiles(result);
    };

    const queued = profileWriteQueue.then(run, run);
    profileWriteQueue = queued.then(() => undefined, () => undefined);
    await queued;
    return result;
};
