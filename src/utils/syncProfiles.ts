// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { SyncProfile } from '../types';

export const SYNC_PROFILES_CHANGED_EVENT = 'aeroftp-sync-profiles-changed';

/** Notify mounted preset lists only once a storage mutation has succeeded. */
export function notifySyncProfilesChanged(): void {
    if (typeof window !== 'undefined') window.dispatchEvent(new Event(SYNC_PROFILES_CHANGED_EVENT));
}

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

/**
 * A preset the user saved (an imported AeroSync script lands here) can be
 * deleted; the built-in ones live in code, not in the profiles folder.
 */
export function isDeletableSyncProfile(profile: Pick<SyncProfile, 'builtin'>): boolean {
    return profile.builtin !== true;
}

/**
 * Delete a saved sync preset and return the list as the backend now has it
 * (or, when that reload fails, the shown list without the deleted preset and
 * the reload error). Every import of the same script saves another
 * `-imported-N` copy, and until this existed nothing could remove one.
 */
export async function deleteSavedSyncProfile(
    invoke: Invoke,
    profile: SyncProfile,
    shown: SyncProfile[],
): Promise<{ left: SyncProfile[]; reloadError: string | null }> {
    if (!isDeletableSyncProfile(profile)) {
        throw new Error(`"${profile.name}" is built in and cannot be deleted`);
    }
    await invoke('delete_sync_profile_cmd', { id: profile.id });
    notifySyncProfilesChanged();
    try {
        return { left: await invoke<SyncProfile[]>('load_sync_profiles_cmd'), reloadError: null };
    } catch (err) {
        // The preset is gone: a failed reload must not leave it selectable,
        // where Export would send its id.
        return { left: shown.filter((p) => p.id !== profile.id), reloadError: String(err) };
    }
}
