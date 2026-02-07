// Unified Secure Storage — vault-backed with localStorage fallback
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
 * Store in vault and remove the localStorage copy
 * Used during writes to ensure data moves to encrypted storage
 */
export async function secureStoreAndClean(
  vaultKey: string,
  localStorageKey: string,
  value: unknown
): Promise<void> {
  await secureStore(vaultKey, value);
  localStorage.removeItem(localStorageKey);
}
