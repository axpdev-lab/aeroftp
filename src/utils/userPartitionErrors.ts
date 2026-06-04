// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Translate the opaque user-partition / crypto error codes the Rust backend
 * returns (user_partitions.rs, user_crypto.rs) into actionable, localized
 * messages. Shared by the account lock screen and the Manage Users panel so a
 * single code path owns the mapping.
 *
 * Unknown codes fall through to the raw message: better to show something than
 * to swallow an error we did not anticipate.
 *
 * The headline F-012 case is the cross-machine import: a passphrase-less
 * partition is wrapped under the source machine's key, so the destination's
 * AES-KW unwrap fails with "Unwrap user data key: integrity check failed".
 * That used to surface verbatim ("variabili non tradotte"); it now maps to a
 * message that tells the user exactly what to do.
 */
type Translate = (key: string, params?: Record<string, string | number>) => string;

export function mapUserPartitionError(err: unknown, t: Translate): string {
    const message = typeof err === 'string' ? err : String(err);
    const has = (code: string) => message.includes(code);

    // F-012: data key cannot be unwrapped on this device.
    if (
        has('Unwrap user data key') ||
        has('integrity check failed') ||
        has('DEK_VERIFIER_MISMATCH')
    ) {
        return t('accountLock.dataKeyUnreadable');
    }
    if (has('WRONG_PASSPHRASE')) return t('accountLock.wrongPassphrase');
    if (has('PASSPHRASE_REQUIRED')) return t('accountLock.passphraseRequired');
    if (has('PASSPHRASE_NOT_NEEDED')) return t('accountLock.passphraseNotNeeded');
    if (has('USER_LOCKED')) return t('accountLock.userLocked');
    if (has('NO_ACTIVE_USER') || has('NOT_ACTIVE_USER')) return t('accountLock.noActiveUser');
    if (has('NOT_AUTHORIZED')) return t('accountLock.notAuthorized');
    if (has('VAULT_LOCKED')) return t('accountLock.vaultLocked');
    if (has('STORE_NOT_READY')) return t('accountLock.storeNotReady');
    if (has('CANNOT_DEMOTE_LAST_ADMIN')) return t('accountLock.cannotDemoteLastAdmin');
    if (has('CANNOT_DELETE_LAST_ADMIN')) return t('accountLock.cannotDeleteLastAdmin');
    if (has('CANNOT_DELETE_LAST_USER')) return t('accountLock.cannotDeleteLastUser');
    if (has('ADMIN_RESET_NOT_FOR_SELF')) return t('accountLock.adminResetNotForSelf');
    if (has('USER_NOT_FOUND')) return t('accountLock.userNotFound');
    return message;
}
