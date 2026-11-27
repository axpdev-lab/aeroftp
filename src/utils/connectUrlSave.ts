// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * "Save to My Servers" from the Connect to URL... dialog: store the profile
 * in the active account's vault-backed list and write the secret only into
 * the encrypted vault under `server_<id>`, never into the profile's options
 * or localStorage. Fail closed: `readSavedServerProfilesForWrite` throws when
 * the vault is locked or the active partition cannot be reached, and the
 * write carries the account id and revision so an account switch or a
 * concurrent save is refused instead of overwriting.
 */

import { invoke } from '@tauri-apps/api/core';
import type { OpenRequest, ProviderType, ServerProfile } from '../types';
import { connectionProtocol } from '../providers/connectionManifest';
import { readSavedServerProfilesForWrite, storeSavedServerProfiles } from './serverProfileStore';

export async function saveLaunchUrlProfile(
    request: OpenRequest,
    name: string,
    maskedPassword: string | undefined,
): Promise<ServerProfile> {
    // Fail closed: this read throws when the vault is locked or the active
    // partition cannot be reached, and the write carries the account id and
    // revision, so a refused save never leaves an orphan vault credential:
    // the profile list is stored FIRST and the secret only after it lands.
    const read = await readSavedServerProfilesForWrite();
    const id = `srv_${Date.now()}_${Math.random().toString(36).slice(2, 11)}`;
    // The masked field wins over an inline URL password only when the URL has
    // none: the backend already refused both. Either way the secret goes to
    // the vault, never into the profile record.
    const secret = maskedPassword || request.password || '';
    const protocol = (request.protocol ?? 'ftp') as ProviderType;
    const profile: ServerProfile = {
        id,
        name: name.trim() || request.server || request.protocol || 'server',
        host: request.server ?? '',
        // Never port 0: a URL without a port saves the protocol's default.
        port: request.port ?? connectionProtocol(protocol)?.defaultPort ?? 21,
        username: request.username ?? '',
        hasStoredCredential: !!secret,
        protocol,
        initialPath: request.initialPath ?? undefined,
        providerId: request.providerId ?? undefined,
        options: request.options as ServerProfile['options'],
    };
    const stored = await storeSavedServerProfiles([...read.profiles, profile], false, read.userId, read.revision);
    if (secret) {
        try {
            await invoke('store_credential', { account: `server_${id}`, password: secret });
        } catch (error) {
            // The profile write already landed: never leave a profile that
            // claims a stored credential the vault refused (a retry would
            // otherwise save a duplicate). Roll the new profile back out of
            // a fresh read, best effort, then propagate the vault error.
            try {
                const fresh = await readSavedServerProfilesForWrite();
                await storeSavedServerProfiles(
                    fresh.profiles.filter((p) => p.id !== id),
                    false,
                    fresh.userId,
                    fresh.revision,
                );
            } catch {
                // best-effort rollback: the original vault error is reported
            }
            throw error;
        }
    }
    return stored.find((p) => p.id === id) ?? profile;
}
