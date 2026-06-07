// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { describe, it, expect, vi } from 'vitest';
import { migrateFilenApiKeysToVault } from './filenApiKeyMigration';
import type { ServerProfile } from '../types';

// Minimal ServerProfile factory: the migration only reads id + options.
const prof = (over: Partial<ServerProfile>): ServerProfile =>
  ({ id: 'id', name: 'n', protocol: 'filen', ...over }) as ServerProfile;

describe('migrateFilenApiKeysToVault (#230)', () => {
  it('moves an inline filen_api_key into the vault, strips it, sets the flag', async () => {
    const store = vi.fn().mockResolvedValue(undefined);
    const p = prof({
      id: 'f1',
      options: { filen_api_key: 'SECRET', tlsMode: 'none' } as ServerProfile['options'],
    });
    const { migrated, changed } = await migrateFilenApiKeysToVault([p], store);

    expect(store).toHaveBeenCalledTimes(1);
    expect(store).toHaveBeenCalledWith('filen_api_key_f1', 'SECRET');
    expect(changed).toBe(true);
    expect(migrated[0].options?.filen_api_key).toBeUndefined();
    // unrelated options are preserved
    expect(migrated[0].options?.tlsMode).toBe('none');
    expect(migrated[0].hasStoredFilenApiKey).toBe(true);
  });

  it('leaves a profile without an inline key untouched (no store, not changed)', async () => {
    const store = vi.fn().mockResolvedValue(undefined);
    const p = prof({ id: 'f2', options: {} as ServerProfile['options'] });
    const { migrated, changed } = await migrateFilenApiKeysToVault([p], store);

    expect(store).not.toHaveBeenCalled();
    expect(changed).toBe(false);
    expect(migrated[0]).toBe(p); // same reference, no rewrite
  });

  it('keeps the key in place and reports onError when the vault write fails', async () => {
    const store = vi.fn().mockRejectedValue(new Error('vault down'));
    const onError = vi.fn();
    const p = prof({
      id: 'f3',
      options: { filen_api_key: 'SECRET' } as ServerProfile['options'],
    });
    const { migrated, changed } = await migrateFilenApiKeysToVault([p], store, onError);

    expect(changed).toBe(false);
    // not stripped, flag not set -> next launch retries
    expect(migrated[0].options?.filen_api_key).toBe('SECRET');
    expect(migrated[0].hasStoredFilenApiKey).toBeUndefined();
    expect(onError).toHaveBeenCalledWith('f3', expect.any(Error));
  });

  it('migrates only the profiles that carry a key in a mixed set', async () => {
    const store = vi.fn().mockResolvedValue(undefined);
    const a = prof({ id: 'a', options: { filen_api_key: 'KA' } as ServerProfile['options'] });
    const b = prof({ id: 'b', protocol: 'sftp', options: {} as ServerProfile['options'] });
    const { migrated, changed } = await migrateFilenApiKeysToVault([a, b], store);

    expect(changed).toBe(true);
    expect(store).toHaveBeenCalledTimes(1);
    expect(store).toHaveBeenCalledWith('filen_api_key_a', 'KA');
    expect(migrated[0].hasStoredFilenApiKey).toBe(true);
    expect(migrated[1]).toBe(b); // untouched
  });
});
