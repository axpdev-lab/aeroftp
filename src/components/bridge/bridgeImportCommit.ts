// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { ServerProfile } from '../../types';
import { makeSafe } from '../../utils/profileSecurity';
import { readSavedServerProfilesForWrite, storeSavedServerProfiles } from '../../utils/serverProfileStore';

/** A profile as `import_bridge_config` returns it, credentials redacted. */
export interface ImportedServer {
    id: string;
    fileIndex?: number;
    name: string;
    host: string;
    port: number;
    username: string;
    protocol?: string;
    initialPath?: string;
    options?: Record<string, unknown>;
    providerId?: string;
    hasStoredCredential?: boolean;
    aeroCryptOverlay?: ServerProfile['aeroCryptOverlay'];
    hasStoredAeroCryptPassword?: boolean;
    hasStoredAeroCryptSalt?: boolean;
    /** The Filen CLI API key the backend moved into the vault (#230). */
    hasStoredFilenApiKey?: boolean;
    /** The AWS STS session token the backend moved into the vault. */
    hasStoredSessionToken?: boolean;
    /** The pre-reset host and options when `import_bridge_config` reset a relaxing option. */
    relaxedHost?: string;
    relaxedOptions?: Record<string, unknown>;
}

/**
 * The saved profile an imported one becomes. Only the fields the backend
 * returned are kept: what it took out of the options (a Filen API key, the
 * crypt secrets) stays in the vault, each `hasStored*` flag says only what the
 * vault holds, and security-relaxing options stay in their safe state unless
 * explicitly kept in `keepRelaxed` (#1506 Requested 198).
 */
export const importedServerProfile = (
    s: ImportedServer,
    keepRelaxed: readonly string[] = [],
): ServerProfile => {
    const useRelaxed = keepRelaxed.length > 0 && (s.relaxedHost !== undefined || s.relaxedOptions !== undefined);
    const { profile: safe } = makeSafe(
        {
            host: useRelaxed ? (s.relaxedHost ?? s.host) : s.host,
            port: s.port,
            providerId: s.providerId,
            protocol: s.protocol,
            options: useRelaxed ? (s.relaxedOptions ?? s.options) : s.options,
        },
        keepRelaxed,
    );
    return {
        id: s.id, name: s.name, host: safe.host ?? s.host, port: s.port,
        username: s.username,
        protocol: s.protocol as ServerProfile['protocol'],
        initialPath: s.initialPath,
        options: safe.options as ServerProfile['options'],
        providerId: s.providerId,
        hasStoredCredential: s.hasStoredCredential || false,
        aeroCryptOverlay: s.aeroCryptOverlay,
        hasStoredAeroCryptPassword: s.hasStoredAeroCryptPassword,
        hasStoredAeroCryptSalt: s.hasStoredAeroCryptSalt,
        hasStoredFilenApiKey: s.hasStoredFilenApiKey,
        hasStoredSessionToken: s.hasStoredSessionToken,
    };
};

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
 * merged list back for the caller's local state. The removal of the profiles
 * being replaced (`removeKeys`, keyed by `bridgeProfileKey`) and the append go
 * out in THIS ONE write: the read, the account check and the write are a single
 * account-bound operation, so a refusal (account switch, revision taken,
 * duplicate id) leaves the vault exactly as it was and there is no half-applied
 * replace to roll back.
 *
 * The vault is the only ground truth, read through
 * `readSavedServerProfilesForWrite`: an empty result means an empty vault,
 * never a vault that could not be reached (an unavailable owner read throws
 * there instead of arriving here as a false `null` legacy account). The list
 * handed back is the one stored: an imported profile whose name another
 * profile holds is stored with the first free " (1)", " (2)", ...
 * (`storeSavedServerProfiles`).
 *
 * @param newServers the imported profiles.
 * @param expectedUserId the account a bridge import read before storing
 *   (`null`: a confirmed legacy read, which has no account). Undefined skips
 *   the check. An account that is not this one is refused, including one that
 *   became active after a legacy read.
 * @param removeKeys the `bridgeProfileKey`s of the existing profiles this
 *   import replaces; they leave the list in the same write that adds
 *   `newServers`.
 * @throws when the vault's active account is not `expectedUserId` (switched
 *   from another window during the import); when a profile to append has an id
 *   the vault already holds, or two profiles of this batch share one id; or when
 *   another window or process saved between the read and the write
 *   (`PROFILES_CHANGED`). Nothing is written then.
 */
export async function appendImportedProfiles(
    newServers: ServerProfile[],
    expectedUserId?: number | null,
    removeKeys?: ReadonlySet<string>,
): Promise<ServerProfile[]> {
    const { profiles: current, userId, revision } = await readSavedServerProfilesForWrite();
    // Undefined skips the check (older call sites); null is a CONFIRMED legacy
    // read; a number is the captured owner. A locked or unreadable owner never
    // reaches this comparison as null: readSavedServerProfilesForWrite throws
    // for it, so an unavailable read is a failure here, not a false
    // account-changed refusal.
    if (expectedUserId !== undefined && (userId ?? null) !== expectedUserId) {
        throw new Error('The active account changed during the import, so the profiles were not saved. Make the account you imported into the active one again and import the same file again.');
    }
    const removing = removeKeys !== undefined && removeKeys.size > 0;
    // Nothing to store (every profile skipped, nothing to replace): the list is
    // not rewritten, so a write that could only fail cannot report a failed import.
    if (newServers.length === 0 && !removing) return current;
    const base = removing ? current.filter(s => !removeKeys.has(bridgeProfileKey(s))) : current;
    const baseIds = new Set(base.map(s => s.id));
    const seen = new Set<string>();
    for (const server of newServers) {
        if (seen.has(server.id)) {
            throw new Error(`Two profiles of this import share the id ${server.id}, so nothing was written.`);
        }
        if (baseIds.has(server.id)) {
            throw new Error(`Profile ${server.id} was saved by another window during the import, so nothing was written.`);
        }
        seen.add(server.id);
    }
    return storeSavedServerProfiles([...base, ...newServers], false, userId, revision);
}

/**
 * `commitImportedServers` bound to the account the scan wrote into: the
 * callback receives `scanUserId` as its expected account, so a switch between
 * the scan and the commit refuses the write instead of landing `hasStored*`
 * flags that point at another account's vault partition. `undefined` means the
 * scan could not read an owner (locked vault): unknown, so the scan check is
 * skipped and the write below still carries its own account+revision check.
 */
export async function commitImportedServersForScan(
    selected: ServerProfile[],
    existingServerKeys: Set<string>,
    scanUserId: number | null | undefined,
    onImport: (servers: ServerProfile[], expectedUserId?: number | null, removeKeys?: ReadonlySet<string>) => void | Promise<void>,
): Promise<CommitOutcome> {
    return commitImportedServers(
        selected,
        existingServerKeys,
        (servers, expectedUserId, removeKeys) => onImport(servers, expectedUserId ?? scanUserId ?? null, removeKeys),
        scanUserId,
    );
}

/**
 * Merge imported profiles into the active user's vault partition with the same
 * add-vs-update split used by the legacy rclone/WinSCP/FileZilla confirm
 * handlers. Single source of truth so every bridge source behaves identically.
 *
 * The replacement is ATOMIC: the profiles to remove and the imported ones are
 * written in one account-bound write (inside `onImport` ->
 * `appendImportedProfiles`), so a refused or failed write leaves the vault
 * untouched; there is no separate removal write whose failure would need an
 * unexecutable rollback after an account switch.
 */
export async function commitImportedServers(
    selected: ServerProfile[],
    existingServerKeys: Set<string>,
    onImport: (servers: ServerProfile[], expectedUserId?: number | null, removeKeys?: ReadonlySet<string>) => void | Promise<void>,
    scanAccountId?: number | null,
): Promise<CommitOutcome> {
    const key = bridgeProfileKey;
    const added = selected.filter(s => !existingServerKeys.has(key(s)));
    const updated = selected.filter(s => existingServerKeys.has(key(s)));

    // The scan account is validated BEFORE anything is written: a switch
    // between the scan and this commit must refuse the whole replace, never
    // remove under one account and append under another. Swallowing a failed
    // read and carrying on imports against an unknown vault, and a vault with
    // no profiles yet reads as an empty list, not as a failure, so refusing
    // here costs a first import nothing.
    let commitUserId: number | null;
    try {
        const read = await readSavedServerProfilesForWrite();
        if (scanAccountId !== undefined && (read.userId ?? null) !== (scanAccountId ?? null)) {
            return {
                added: 0,
                updated: 0,
                error: 'The active account changed since the scan, so nothing was removed or saved. Make the scanned account the active one again and import the same file again.',
            };
        }
        commitUserId = read.userId ?? null;
    } catch {
        return {
            added: 0,
            updated: 0,
            error: 'Import failed. The saved profiles could not be read, so nothing was changed.',
        };
    }

    // One write carries both halves of the replace, bound to the one validated
    // account: a switch in between refuses that write (ACCOUNT_CHANGED /
    // PROFILES_CHANGED from the backend), and nothing was removed, so nothing
    // needs rolling back.
    const removeKeys = updated.length > 0 ? new Set(updated.map(key)) : undefined;
    try {
        await onImport([...updated, ...added], commitUserId, removeKeys);
    } catch (e) {
        return {
            added: 0,
            updated: 0,
            error: `Import failed. No changes were made. ${String(e ?? '')}`,
        };
    }

    return { added: added.length, updated: updated.length };
}
