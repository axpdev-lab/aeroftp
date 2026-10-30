// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';
import { ServerProfile } from '../../types';
import { readSavedServerProfilesForWrite, storeSavedServerProfiles } from '../../utils/serverProfileStore';

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
 * Secret-presence flags an overwrite keeps set when the stored profile has
 * them: a secret the imported file does not carry stays in the vault. Twin of
 * `STORED_SECRET_FLAGS` in `src-tauri/src/profile_import_plan.rs`.
 */
export const STORED_SECRET_FLAGS = [
    'hasStoredCredential',
    'hasStoredFilenApiKey',
    'hasStoredAeroCryptPassword',
    'hasStoredAeroCryptSalt',
    'hasStoredAeroCryptKeyfilePath',
    'hasStoredAeroCryptConfig',
] as const;

/**
 * Profile fields the export format omits when empty, so a null says nothing
 * about the backup and an overwrite keeps the stored value. Twin of
 * `OMITTED_WHEN_EMPTY` in `src-tauri/src/profile_import_plan.rs`.
 */
export const OMITTED_WHEN_EMPTY = [
    'publicUrlBase',
    'customIconUrl',
    'faviconUrl',
    'skipDeltaEligibilityPrompt',
    'aeroCryptOverlay',
    'persistModeCredentials',
] as const;

/**
 * Options keys that carry secrets, stripped from a backup exported without
 * credentials. Twin of `OPTIONS_SECRET_KEYS` in `src-tauri/src/profile_export.rs`.
 */
export const OPTIONS_SECRET_KEYS = [
    'totp_secret',
    'key_passphrase',
    'sessionToken',
    'sasToken',
    'clientSecret',
    'client_secret',
    'secret_key',
    'secretKey',
    'access_key_secret',
    'password',
    'passphrase',
    'privateKey',
    'private_key',
    'api_key',
    'apiKey',
    'token',
    'refresh_token',
    'refreshToken',
    'peerTicket',
    'proton_native_session',
] as const;

/**
 * The profile an overwrite stores: `stored`, the record read in the same write,
 * with the fields of `imported` laid over it. A field the format always writes
 * follows the file, null included, and so does `options` as a whole, except
 * that an options-borne secret the file does not carry stays. A field of
 * `OMITTED_WHEN_EMPTY` left null keeps the stored value, as does a field outside
 * the format, one only this device has. Each secret-presence flag is set when
 * either side has it. `undefined` is not a value (JSON has none) and never
 * clears one. Twin of `overwrite_profile` in
 * `src-tauri/src/profile_import_plan.rs`; the backend has already refused an
 * overwrite that would point the profile at another server.
 */
export function overwriteProfile(stored: ServerProfile, imported: ServerProfile): ServerProfile {
    const omitted: readonly string[] = OMITTED_WHEN_EMPTY;
    const merged: Record<string, unknown> = {
        ...stored,
        ...Object.fromEntries(Object.entries(imported).filter(([key, value]) =>
            value !== undefined && !(value === null && omitted.includes(key)))),
    };
    const storedOptions = (stored.options ?? {}) as Record<string, unknown>;
    const importedOptions = (imported.options ?? {}) as Record<string, unknown>;
    const keptSecrets = OPTIONS_SECRET_KEYS.filter(key => key in storedOptions && !(key in importedOptions));
    if (keptSecrets.length > 0) {
        merged.options = {
            ...(merged.options as Record<string, unknown> | null | undefined),
            ...Object.fromEntries(keptSecrets.map(key => [key, storedOptions[key]])),
        };
    }
    const has = (p: ServerProfile, flag: string) => (p as unknown as Record<string, unknown>)[flag] === true;
    for (const flag of STORED_SECRET_FLAGS) {
        if (has(stored, flag) || has(imported, flag)) merged[flag] = true;
    }
    return merged as unknown as ServerProfile;
}

/**
 * Persist imported profiles onto the vault's current contents and hand the
 * merged list back for the caller's local state.
 *
 * Two properties every import callback needs, and three of the four call sites
 * had both wrong. The vault is the only ground truth, read through
 * `readSavedServerProfilesForWrite` so that an empty result means an empty vault
 * and never a vault that could not be reached; it can be empty precisely
 * because `commitImportedServers` has just removed the profiles it is
 * replacing, and falling back to the component's own snapshot in that case puts
 * the replaced profile back next to its replacement. And a failed write has to
 * propagate, so `commitImportedServers` can restore the backup it took:
 * swallowing it reports a successful import over a vault that has already lost
 * them. The list handed back is the one stored: an imported profile whose
 * name another profile holds is stored with the first free " (1)", " (2)",
 * ... (`storeSavedServerProfiles`).
 *
 * @param newServers the imported profiles.
 * @param replaceIds ids of imported profiles laid over the stored profile of
 *   the same id where it stands (`overwriteProfile`, against the record read
 *   here): the profiles a `.aeroftp` import set to Overwrite reported as
 *   overwritten. Every other
 *   imported profile is appended, as before; the bridge imports name none. A
 *   named profile whose id the vault no longer holds when this writes (deleted
 *   from another window in the meantime) is appended too, never dropped.
 * @param expectedUserId the account the import was made for: the one a
 *   `.aeroftp` import compared the file with and restored the secrets into
 *   (`userId` of its report), or the one a bridge import read before removing
 *   the profiles it updates (`null`: it read the legacy store, which has no
 *   account). Undefined skips the check. An account that is not this one is
 *   refused, including one that became active after a legacy read.
 * @throws when the vault's active account is not `expectedUserId` (switched
 *   from another window during the import); when a profile to append has an id
 *   the vault already holds, or two profiles of this batch share one id: another
 *   window or process saved it after the import was planned, and storing it
 *   would leave two profiles of one id; or when a stored profile to replace was
 *   pointed at another server since (`profile_same_target`), whose secrets must
 *   not follow the file. Nothing is written then.
 */
export async function appendImportedProfiles(
    newServers: ServerProfile[],
    replaceIds: ReadonlySet<string> = new Set(),
    expectedUserId?: number | null,
): Promise<ServerProfile[]> {
    const { profiles: current, userId } = await readSavedServerProfilesForWrite();
    if (expectedUserId !== undefined && (userId ?? null) !== expectedUserId) {
        throw new Error('The active account changed during the import, so the profiles were not saved. Make the account you imported into the active one again and import the same file again.');
    }
    // Nothing to store (every profile skipped): the list is not rewritten, so a
    // write that could only fail cannot report a failed import.
    if (newServers.length === 0) return current;
    const currentIds = new Set(current.map(s => s.id));
    const seen = new Set<string>();
    for (const server of newServers) {
        if (seen.has(server.id)) {
            throw new Error(`Two profiles of this import share the id ${server.id}, so nothing was written.`);
        }
        if (!replaceIds.has(server.id) && currentIds.has(server.id)) {
            throw new Error(`Profile ${server.id} was saved by another window during the import, so nothing was written.`);
        }
        seen.add(server.id);
    }
    const replacing = new Map(newServers.filter(s => replaceIds.has(s.id)).map(s => [s.id, s]));
    for (const stored of current) {
        const imported = replacing.get(stored.id);
        if (imported && !(await invoke<boolean>('profile_same_target', { stored, imported }))) {
            throw new Error(`Profile ${stored.id} was pointed at another server by another window during the import, so nothing was written.`);
        }
    }
    const merged = [
        ...current.map(s => {
            const imported = replacing.get(s.id);
            return imported ? overwriteProfile(s, imported) : s;
        }),
        ...newServers.filter(s => !replaceIds.has(s.id) || !currentIds.has(s.id)),
    ];
    return storeSavedServerProfiles(merged, false, userId);
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
    onImport: (servers: ServerProfile[], replaceIds?: ReadonlySet<string>, expectedUserId?: number | null) => void | Promise<void>,
): Promise<CommitOutcome> {
    const key = bridgeProfileKey;
    const added = selected.filter(s => !existingServerKeys.has(key(s)));
    const updated = selected.filter(s => existingServerKeys.has(key(s)));

    // The whole replace path hangs off this read: it decides which profiles are
    // removed, and it is the only copy that can be put back if the import then
    // fails. Swallowing a failed read and carrying on imports against an unknown
    // vault, skips the removal, and lands the replacement next to the profile it
    // was meant to replace while reporting it as an update. A vault with no
    // profiles yet reads as an empty list, not as a failure, so refusing here
    // costs a first import nothing.
    let backup: ServerProfile[];
    let backupUserId: number | undefined;
    try {
        ({ profiles: backup, userId: backupUserId } = await readSavedServerProfilesForWrite());
    } catch {
        return {
            added: 0,
            updated: 0,
            error: 'Import failed. The saved profiles could not be read, so nothing was changed.',
        };
    }

    try {
        if (updated.length > 0 && backup.length > 0) {
            const updatedKeys = new Set(updated.map(key));
            const filtered = backup.filter(s => !updatedKeys.has(key(s)));
            await storeSavedServerProfiles(filtered, false, backupUserId);
        }
        // Written for the account the removal above was made in: a switch in
        // between refuses the write, and the rollback below restores that account.
        await onImport([...updated, ...added], new Set(), backupUserId ?? null);
    } catch {
        // "No changes were made" is a claim about the vault, so it may only be
        // made when the rollback actually succeeded. A failed restore leaves the
        // profiles this call removed gone, and saying nothing happened would
        // send the user away from the one screen they need to look at.
        const restored = await storeSavedServerProfiles(backup, false, backupUserId).then(() => true, () => false);
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
