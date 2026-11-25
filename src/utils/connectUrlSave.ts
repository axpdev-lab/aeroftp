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
import { readSavedServerProfilesForWrite, storeSavedServerProfiles } from './serverProfileStore';

export async function saveLaunchUrlProfile(
    request: OpenRequest,
    name: string,
    maskedPassword: string | undefined,
): Promise<ServerProfile> {
    const read = await readSavedServerProfilesForWrite();
    const id = `srv_${Date.now()}_${Math.random().toString(36).slice(2, 11)}`;
    // The masked field wins over an inline URL password only when the URL has
    // none: the backend already refused both. Either way the secret goes to
    // the vault, never into the profile record.
    const secret = maskedPassword || request.password || '';
    if (secret) {
        await invoke('store_credential', { account: `server_${id}`, password: secret });
    }
    const profile: ServerProfile = {
        id,
        name: name.trim() || request.server || request.protocol || 'server',
        host: request.server ?? '',
        port: request.port ?? 0,
        username: request.username ?? '',
        hasStoredCredential: !!secret,
        protocol: (request.protocol ?? 'ftp') as ProviderType,
        initialPath: request.initialPath ?? undefined,
        providerId: request.providerId ?? undefined,
        options: request.options as ServerProfile['options'],
    };
    const stored = await storeSavedServerProfiles([...read.profiles, profile], false, read.userId, read.revision);
    return stored.find((p) => p.id === id) ?? profile;
}
