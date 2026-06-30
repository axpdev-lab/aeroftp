// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';

export const PROFILE_VAULT_SECRET_PREFIXES = [
  'server',
  'server_modes',
  'filen_api_key',
  'aerocrypt_overlay_pw',
  'aerocrypt_overlay_salt',
] as const;

export type ProfileVaultSecretPrefix = typeof PROFILE_VAULT_SECRET_PREFIXES[number];
export type ProfileVaultSecretCopyResult = Record<ProfileVaultSecretPrefix, boolean>;

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

export const copyProfileVaultSecrets = async (
  sourceProfileId: string,
  targetProfileId: string,
): Promise<ProfileVaultSecretCopyResult> => {
  const result = Object.fromEntries(
    PROFILE_VAULT_SECRET_PREFIXES.map(prefix => [prefix, false]),
  ) as ProfileVaultSecretCopyResult;

  for (const prefix of PROFILE_VAULT_SECRET_PREFIXES) {
    try {
      const value = await getCredentialWithRetry(`${prefix}_${sourceProfileId}`);
      if (value) {
        await invoke('store_credential', {
          account: `${prefix}_${targetProfileId}`,
          password: value,
        });
        result[prefix] = true;
      }
    } catch {
      // Missing source keys are expected for many profile kinds.
    }
  }

  return result;
};

export const deleteProfileVaultSecrets = async (profileId: string): Promise<void> => {
  await Promise.all(PROFILE_VAULT_SECRET_PREFIXES.map(async (prefix) => {
    try {
      await invoke('delete_credential', { account: `${prefix}_${profileId}` });
    } catch {
      // Best-effort cleanup: missing keys are expected for many profile kinds.
    }
  }));
};
