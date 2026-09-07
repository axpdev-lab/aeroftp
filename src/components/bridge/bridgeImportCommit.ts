// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { ServerProfile } from '../../types';
import { loadSavedServerProfiles, storeSavedServerProfiles } from '../../utils/serverProfileStore';

export interface CommitOutcome {
    added: number;
    updated: number;
    error?: string;
}

/** A plain remote and its Crypt view can share the same account. */
export const bridgeProfileKey = (s: Pick<ServerProfile, 'host' | 'port' | 'username' | 'protocol' | 'initialPath' | 'aeroCryptOverlay'>): string =>
    JSON.stringify([s.protocol || 'ftp', s.host, s.port, s.username, s.initialPath || '',
        s.aeroCryptOverlay?.enabled ? s.aeroCryptOverlay.kind : '',
        s.aeroCryptOverlay?.enabled ? (s.aeroCryptOverlay.remoteScope || '') : '']);

/**
 * Persist imported profiles onto the vault's current contents and hand the
 * merged list back for the caller's local state.
 *
 * Two properties every import callback needs, and three of the four call sites
 * had both wrong. The vault is the only ground truth: `loadSavedServerProfiles`
 * throws when the read fails, so an empty result is an empty vault, and it can
 * be empty precisely because `commitImportedServers` has just removed the
 * profiles it is replacing. Falling back to the component's own snapshot in
 * that case puts the replaced profile back next to its replacement. And a
 * failed write has to propagate, so `commitImportedServers` can restore the
 * backup it took: swallowing it reports a successful import over a vault that
 * has already lost them.
 */
export async function appendImportedProfiles(newServers: ServerProfile[]): Promise<ServerProfile[]> {
    const current = await loadSavedServerProfiles();
    const merged = [...current, ...newServers];
    await storeSavedServerProfiles(merged);
    return merged;
}

/**
 * Merge imported profiles into the active user's vault partition with
 * the same add-vs-update split and rollback semantics used by the
 * legacy rclone/WinSCP/FileZilla confirm handlers. Single source of
 * truth so every bridge source behaves identically.
 */
export async function commitImportedServers(
    selected: ServerProfile[],
    existingServerKeys: Set<string>,
    onImport: (servers: ServerProfile[]) => void | Promise<void>,
): Promise<CommitOutcome> {
    const key = bridgeProfileKey;
    const added = selected.filter(s => !existingServerKeys.has(key(s)));
    const updated = selected.filter(s => existingServerKeys.has(key(s)));

    const backup = await loadSavedServerProfiles().catch(() => null);
    try {
        if (updated.length > 0 && backup && backup.length > 0) {
            const updatedKeys = new Set(updated.map(key));
            const filtered = backup.filter(s => !updatedKeys.has(key(s)));
            await storeSavedServerProfiles(filtered);
        }
        await onImport([...updated, ...added]);
    } catch {
        // "No changes were made" is a claim about the vault, so it may only be
        // made when the rollback actually succeeded. A failed restore leaves the
        // profiles this call removed gone, and saying nothing happened would
        // send the user away from the one screen they need to look at.
        const restored = backup === null
            || (await storeSavedServerProfiles(backup).then(() => true, () => false));
        return {
            added: 0,
            updated: 0,
            error: restored
                ? 'Import failed. No changes were made.'
                : 'Import failed, and the previous profiles could not be restored. Check My Servers before importing again.',
        };
    }

    return { added: added.length, updated: updated.length };
}
