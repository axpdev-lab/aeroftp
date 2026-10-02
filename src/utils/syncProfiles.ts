// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { SyncProfile } from '../types';

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

/**
 * A preset the user saved (an imported AeroSync script lands here) can be
 * deleted; the built-in ones live in code, not in the profiles folder.
 */
export function isDeletableSyncProfile(profile: Pick<SyncProfile, 'builtin'>): boolean {
    return profile.builtin !== true;
}

/**
 * Delete a saved sync preset and return the list as the backend now has it.
 * Every import of the same script saves another `-imported-N` copy, and until
 * this existed nothing could remove one.
 */
export async function deleteSavedSyncProfile(invoke: Invoke, profile: SyncProfile): Promise<SyncProfile[]> {
    if (!isDeletableSyncProfile(profile)) {
        throw new Error(`"${profile.name}" is built in and cannot be deleted`);
    }
    await invoke('delete_sync_profile_cmd', { id: profile.id });
    return invoke<SyncProfile[]>('load_sync_profiles_cmd');
}
