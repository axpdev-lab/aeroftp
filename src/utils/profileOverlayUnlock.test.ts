// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { profileOverlayApplyParams } from './profileOverlayUnlock';
import type { AeroCryptOverlayBinding } from '../types';

const base = {
  savedServerId: 'srv_1',
  overlayScope: '/Vault',
  password: 'pw',
  salt: '',
  keyfilePath: '',
};

describe('profileOverlayApplyParams', () => {
  it('carries the default-salt intent of the saved binding (#276)', () => {
    // Dropping it made a profile saved with "Default salt" on create its vault
    // with a random per-vault salt, and it is what lets the password alone
    // reopen the vault once marker and keystore copy are gone.
    const binding: AeroCryptOverlayBinding = { enabled: true, kind: 'aerocrypt', useDefaultSalt: true };
    expect(profileOverlayApplyParams({ ...base, binding }).useDefaultSalt).toBe(true);
    expect(
      profileOverlayApplyParams({ ...base, binding: { enabled: true, kind: 'aerocrypt' } }).useDefaultSalt,
    ).toBe(false);
  });

  it('carries the headed intent and the owning profile', () => {
    const binding: AeroCryptOverlayBinding = { enabled: true, kind: 'aerocrypt', withHeader: true };
    const params = profileOverlayApplyParams({ ...base, binding });
    expect(params.withHeader).toBe(true);
    expect(params.profileId).toBe('srv_1');
    expect(params.remoteScope).toBe('/Vault');
  });

  it('leaves the rclone-crypt forms to the backend and nulls empty secrets', () => {
    const binding: AeroCryptOverlayBinding = {
      enabled: true,
      kind: 'rclone-crypt',
      passwordForm: 'obscured',
      directoryNameEncryption: false,
      filenameEncryption: 'obfuscate',
    };
    const params = profileOverlayApplyParams({ ...base, binding, salt: 's2' });
    expect(params.passwordForm).toBeNull();
    expect(params.saltForm).toBeNull();
    expect(params.salt).toBe('s2');
    expect(params.keyfilePath).toBeNull();
    expect(params.directoryNameEncryption).toBe(false);
    expect(params.filenameEncryption).toBe('obfuscate');
  });
});
