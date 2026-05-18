// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Unified Secure Storage: vault-backed with localStorage fallback
// All sensitive config data is stored encrypted in the Universal Vault
// localStorage is only used as read-only fallback during migration period

import { invoke } from '@tauri-apps/api/core';

const VAULT_PREFIX = 'config_';

/**
 * Store data in the encrypted vault
 */
export async function secureStore(key: string, value: unknown): Promise<void> {
  const json = JSON.stringify(value);
  await invoke('store_credential', { account: VAULT_PREFIX + key, password: json });
}

/**
 * Retrieve data from the encrypted vault
 */
export async function secureGet<T>(key: string): Promise<T | null> {
  try {
    const json = await invoke<string>('get_credential', { account: VAULT_PREFIX + key });
    return JSON.parse(json) as T;
  } catch {
    return null;
  }
}

/**
 * Delete data from the encrypted vault
 */
export async function secureDelete(key: string): Promise<void> {
  try {
    await invoke('delete_credential', { account: VAULT_PREFIX + key });
  } catch { /* not found is ok */ }
}

/**
 * Like {@link secureGet} but resilient to the vault not being ready yet.
 *
 * At app boot a component can mount and read the vault before
 * `init_credential_store` has finished caching it, in which case
 * `get_credential` rejects with `STORE_NOT_READY`. Plain `secureGet`
 * collapses that into `null`, indistinguishable from "key absent", so a
 * boot-time reconcile gives up and the UI stays on its per-origin
 * localStorage cache. When that cache was wiped out-of-band (an
 * aggressive disk/cache cleanup that removes WebKitGTK localStorage, or a
 * fresh machine profile) the list would then stay empty even though the
 * vault still holds every profile. This variant retries with backoff
 * *only* on `STORE_NOT_READY`. Any other outcome (key absent, parse
 * error) still resolves to `null` immediately, so the #194 semantics
 * where a genuinely empty vault must propagate are preserved unchanged.
 */
export async function secureGetAwaitReady<T>(
  key: string,
  maxRetries = 6,
  baseDelayMs = 200,
): Promise<T | null> {
  for (let attempt = 0; attempt < maxRetries; attempt++) {
    try {
      const json = await invoke<string>('get_credential', { account: VAULT_PREFIX + key });
      return JSON.parse(json) as T;
    } catch (err) {
      if (String(err).includes('STORE_NOT_READY') && attempt < maxRetries - 1) {
        await new Promise(resolve => setTimeout(resolve, baseDelayMs * (attempt + 1)));
        continue;
      }
      return null;
    }
  }
  return null;
}

/**
 * Load data from vault with localStorage fallback (backward compatibility)
 * Tries vault first, falls back to localStorage if vault returns null
 */
export async function secureGetWithFallback<T>(
  vaultKey: string,
  localStorageKey: string
): Promise<T | null> {
  // Try vault first
  const vaultData = await secureGet<T>(vaultKey);
  if (vaultData !== null) return vaultData;

  // Fallback to localStorage (read-only, for pre-migration data)
  try {
    const raw = localStorage.getItem(localStorageKey);
    if (raw) return JSON.parse(raw) as T;
  } catch { /* parse error */ }

  return null;
}

/**
 * Store in vault and keep localStorage as resilient backup.
 * On Windows, vault.db file locking or ACL issues can cause transient failures.
 * Keeping localStorage ensures server profiles are never permanently lost.
 */
export async function secureStoreAndClean(
  vaultKey: string,
  localStorageKey: string,
  value: unknown
): Promise<void> {
  // Always keep localStorage as backup (write-through)
  localStorage.setItem(localStorageKey, JSON.stringify(value));
  await secureStore(vaultKey, value);
}
