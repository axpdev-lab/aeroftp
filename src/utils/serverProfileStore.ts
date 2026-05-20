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
// Custom DOM event fired whenever the saved-server vault is mutated. App.tsx
// listens on `window` and bumps `serversRefreshKey` so the IntroHub card list
// reflects the change without waiting for an unrelated route refresh. Mirrors
// the `transfer-toast-update` / `editor-reload` pattern used elsewhere.
export const PROFILES_CHANGED_EVENT = 'aeroftp-profiles-changed';

export const storeSavedServerProfiles = async (profiles: ServerProfile[]): Promise<void> => {
    await secureStore(SAVED_SERVERS_ACCOUNT, profiles);
    try {
        localStorage.removeItem(SAVED_SERVERS_STORAGE_KEY);
    } catch {
        // best-effort cleanup
    }
    try {
        window.dispatchEvent(new CustomEvent(PROFILES_CHANGED_EVENT));
    } catch {
        // SSR / non-DOM environment: dispatch is a best-effort notification.
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
    }));
};

export const clearProfileConnectFailure = async (
    profileId: string,
): Promise<void> => {
    if (!profileId) return;
    await mergeSavedServerProfile(profileId, profile => {
        if (!profile.lastConnectionError) return profile;
        const { lastConnectionError: _omit, ...rest } = profile;
        return rest as ServerProfile;
    });
};
