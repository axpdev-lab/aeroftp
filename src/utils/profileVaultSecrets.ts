// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';

// Base prefixes of the per-profile vault keys, kept for reference and for the
// copy-result flag lookups below. The AUTHORITATIVE enumeration now lives in the
// Rust `per_profile_vault_keys` (single source of truth shared by delete,
// duplicate and export), reached through the `purge_profile_secrets` /
// `copy_profile_secrets` commands so the GUI and CLI can never drift. Do not
// delete/copy by iterating this list on the frontend: call the commands.
export const PROFILE_VAULT_SECRET_PREFIXES = [
  'server',
  'server_modes',
  'jottacloud_refresh',
  'aerocrypt_overlay_pw',
  'aerocrypt_overlay_salt',
  'aerocrypt_overlay_keyfile_path',
  'aerocrypt_overlay_config',
  'filen_api_key',
  'onedrive_drive_id',
  'onedrive_drive_type',
  // OAuth token key is `oauth_<slug>_<id>`; the backend derives the slug.
] as const;

export type ProfileVaultSecretCopyResult = Record<string, boolean>;

export const getCredentialWithRetry = async (account: string, maxRetries = 3): Promise<string> => {
  for (let attempt = 0; attempt < maxRetries; attempt++) {
    try {
      return await invoke<string>('get_credential', { account });
    } catch (err) {
      const errorMsg = String(err);
      if (errorMsg.includes('STORE_NOT_READY') && attempt < maxRetries - 1) {
        await new Promise(resolve => setTimeout(resolve, 200 * (attempt + 1)));
        continue;
      }
      throw err;
    }
  }
  throw new Error('Failed to get credential after retries');
};

/**
 * Copy EVERY vault secret scoped to `sourceProfileId` onto `targetProfileId` so
 * a duplicated profile is a complete working copy (crypt overlay, Filen key,
 * OAuth/Jotta token, per-mode snapshots, OneDrive hints), not just the password.
 * Delegates to the Rust `copy_profile_secrets` single source of truth. Returns a
 * map `base_prefix -> bool` of what was copied so the caller can set the
 * duplicate's `hasStored*` flags. `protocol` lets the backend derive the OAuth
 * token key.
 */
export const copyProfileVaultSecrets = async (
  sourceProfileId: string,
  targetProfileId: string,
  protocol?: string,
): Promise<ProfileVaultSecretCopyResult> => {
  try {
    return await invoke<ProfileVaultSecretCopyResult>('copy_profile_secrets', {
      sourceId: sourceProfileId,
      targetId: targetProfileId,
      protocol: protocol ?? null,
    });
  } catch {
    // Best-effort: a copy failure leaves the duplicate credential-free, exactly
    // as before this helper existed.
    return {};
  }
};

/**
 * Remove EVERY vault secret scoped to a profile id so deleting a profile leaves
 * nothing behind. Delegates to the Rust `purge_profile_secrets` single source of
 * truth. `protocol` lets the backend derive the OAuth token key.
 */
export const deleteProfileVaultSecrets = async (
  profileId: string,
  protocol?: string,
): Promise<void> => {
  try {
    await invoke('purge_profile_secrets', { profileId, protocol: protocol ?? null });
  } catch {
    // Best-effort cleanup: a purge failure must not block the profile removal.
  }
};
