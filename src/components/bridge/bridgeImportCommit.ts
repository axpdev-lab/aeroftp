// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { ServerProfile } from '../../types';
import { loadSavedServerProfiles, storeSavedServerProfiles } from '../../utils/serverProfileStore';

export interface CommitOutcome {
    added: number;
    updated: number;
    error?: string;
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
    onImport: (servers: ServerProfile[]) => void,
): Promise<CommitOutcome> {
    const key = (s: ServerProfile) => `${s.host}:${s.port}:${s.username}`;
    const added = selected.filter(s => !existingServerKeys.has(key(s)));
    const updated = selected.filter(s => existingServerKeys.has(key(s)));

    const backup = await loadSavedServerProfiles().catch(() => null);
    if (updated.length > 0 && backup && backup.length > 0) {
        try {
            const updatedKeys = new Set(updated.map(key));
            const filtered = backup.filter(s => !updatedKeys.has(key(s)));
            await storeSavedServerProfiles(filtered).catch(() => {});
        } catch { /* fall through: onImport rebuilds the list */ }
    }

    try {
        onImport([...updated, ...added]);
    } catch {
        if (backup !== null) await storeSavedServerProfiles(backup).catch(() => {});
        return { added: 0, updated: 0, error: 'Import failed. No changes were made.' };
    }

    return { added: added.length, updated: updated.length };
}
