// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import type { ServerProfile } from '../types';
import { secureGetWithFallback, secureStoreAndClean } from './secureStorage';

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

export const loadSavedServerProfiles = async (): Promise<ServerProfile[]> => {
    const secureProfiles = await secureGetWithFallback<ServerProfile[]>(
        SAVED_SERVERS_ACCOUNT,
        SAVED_SERVERS_STORAGE_KEY,
    );
    if (secureProfiles && secureProfiles.length > 0) return secureProfiles;
    return readLocalProfiles();
};

export const storeSavedServerProfiles = async (profiles: ServerProfile[]): Promise<void> => {
    await secureStoreAndClean(SAVED_SERVERS_ACCOUNT, SAVED_SERVERS_STORAGE_KEY, profiles);
    try {
        localStorage.setItem(SAVED_SERVERS_STORAGE_KEY, JSON.stringify(profiles));
    } catch {
        // best-effort sync backup
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
