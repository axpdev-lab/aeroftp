// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// MU-7 soft warning: after a profile is saved from the connection form, ask
// the backend whether another user account on this device already stores the
// same server/account, and say so in a toast. It never blocks or undoes the
// save: two people sharing one machine may legitimately use the same server,
// and the intra-user duplicate check stays Activity-Log only.

import type { ServerProfile } from '../types';
import type { TranslationFunction } from '../i18n/types';
import type { OperationStatus, OperationType } from '../hooks/useActivityLog';
import { findCrossUserDedup } from './userPartitions';
import { getStorageDedupKey } from './storageDedup';

type LogActivity = (operation: OperationType, message: string, status?: OperationStatus, details?: string) => unknown;

/**
 * Warn when `saved` is already stored by another user account. Pass the
 * profile as it was before the save as `previous` on an edit: the warning is
 * about pointing a profile at an account another user holds, so an edit that
 * keeps the same server/account (a rename, a new icon) does not repeat it.
 *
 * Returns the names of the other accounts (empty when there is nothing to
 * say). Never throws: a probe failure only means no warning.
 */
export async function warnIfSavedByOtherAccount(
    saved: ServerProfile,
    previous: ServerProfile | undefined,
    t: TranslationFunction,
    logActivity: LogActivity,
): Promise<string[]> {
    if (previous && getStorageDedupKey(previous) === getStorageDedupKey(saved)) return [];
    let names: string[];
    try {
        const matches = await findCrossUserDedup(saved as unknown as Record<string, unknown>);
        names = matches.map((m) => m.userName);
    } catch (err) {
        console.warn('[MU-7] cross-user duplicate probe failed:', err);
        return [];
    }
    if (names.length === 0) return [];
    const accounts = names.join(', ');
    window.dispatchEvent(new CustomEvent('aeroftp-toast', {
        detail: {
            type: 'warning',
            title: t('manageUsers.crossUserDedupTitle'),
            message: t('manageUsers.crossUserDedupMessage', { name: saved.name, accounts }),
            duration: 8000,
        },
    }));
    // The toast honours the notifications setting; the Activity Log entry is
    // the record that survives it being off.
    logActivity(
        'PROFILE_DUPLICATE',
        `Profile "${saved.name}" is also saved in another account: ${accounts}`,
        'success',
        `dedupKey=${getStorageDedupKey(saved)}`,
    );
    return names;
}
