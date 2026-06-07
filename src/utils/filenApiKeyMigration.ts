// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import type { ServerProfile } from '../types';

/**
 * Issue #230 migration: relocate any Filen CLI API key that earlier builds
 * persisted inline in a saved profile's `options.filen_api_key` into the secure
 * vault (keyed `filen_api_key_<id>`), strip it from the stored profile, and set
 * `hasStoredFilenApiKey` so the long-lived secret no longer lands in the profile
 * store / localStorage.
 *
 * Pure and injectable so it can be unit-tested without the Tauri runtime:
 * `storeCredential` performs the one real side effect (the production caller
 * passes `invoke('store_credential', {account, password})`). A profile whose
 * store call throws is returned unchanged so its key stays in place and the next
 * launch retries it (`onError` lets the caller log). `changed` is true iff at
 * least one key was relocated, so the caller only rewrites the profile store
 * when something actually moved.
 */
export async function migrateFilenApiKeysToVault(
  profiles: ServerProfile[],
  storeCredential: (account: string, key: string) => Promise<void>,
  onError?: (profileId: string, err: unknown) => void,
): Promise<{ migrated: ServerProfile[]; changed: boolean }> {
  let changed = false;
  const migrated = await Promise.all(
    profiles.map(async (p) => {
      const key = p.options?.filen_api_key;
      if (!key) return p;
      try {
        await storeCredential(`filen_api_key_${p.id}`, key);
      } catch (e) {
        onError?.(p.id, e);
        return p; // leave the key in place and retry on the next launch
      }
      changed = true;
      const restOptions = { ...(p.options || {}) };
      delete restOptions.filen_api_key;
      return { ...p, options: restOptions, hasStoredFilenApiKey: true };
    }),
  );
  return { migrated, changed };
}
