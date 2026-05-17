// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { ServerProfile } from '../../types';

export interface CommitOutcome {
    added: number;
    updated: number;
    error?: string;
}

/**
 * Merge imported profiles into the saved-server list with the same
 * add-vs-update split, localStorage rollback and ground-truth read used
 * by the legacy rclone/WinSCP/FileZilla confirm handlers. Single source
 * of truth so every bridge source behaves identically.
 */
export function commitImportedServers(
    selected: ServerProfile[],
    existingServerKeys: Set<string>,
    onImport: (servers: ServerProfile[]) => void,
): CommitOutcome {
    const key = (s: ServerProfile) => `${s.host}:${s.port}:${s.username}`;
    const added = selected.filter(s => !existingServerKeys.has(key(s)));
    const updated = selected.filter(s => existingServerKeys.has(key(s)));

    const backup = localStorage.getItem('aeroftp-saved-servers');
    if (updated.length > 0) {
        try {
            if (backup) {
                const current: ServerProfile[] = JSON.parse(backup);
                const updatedKeys = new Set(updated.map(key));
                localStorage.setItem(
                    'aeroftp-saved-servers',
                    JSON.stringify(current.filter(s => !updatedKeys.has(key(s)))),
                );
            }
        } catch { /* fall through: onImport rebuilds the list */ }
    }

    try {
        onImport([...updated, ...added]);
    } catch {
        if (backup !== null) localStorage.setItem('aeroftp-saved-servers', backup);
        return { added: 0, updated: 0, error: 'Import failed. No changes were made.' };
    }

    return { added: added.length, updated: updated.length };
}
