// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { AeroCryptOverlayBinding, CryptSecretForm } from '../types';

export type ProviderCryptOverlayKind = 'rclone-crypt' | 'aerocrypt';

/** What `provider_apply_crypt_overlay` is asked to apply to the live connection. */
export interface ProviderCryptOverlayApply {
  kind: ProviderCryptOverlayKind;
  remoteScope?: string | null;
  filenameEncryption?: string | null;
  directoryNameEncryption?: boolean | null;
  password: string;
  salt?: string | null;
  /** rclone-crypt: how `password` / `salt` are written; null = not known (read automatically). */
  passwordForm?: CryptSecretForm | null;
  saltForm?: CryptSecretForm | null;
  /** AeroCrypt Tier 1 optional keyfile second factor (local path, resolved to a digest backend-side). */
  keyfilePath?: string | null;
  profileId?: string | null;
  /** Headed vault: write/heal remote marker when missing (tracker #421 #7). */
  withHeader?: boolean | null;
  /** AeroCrypt default salt: create with the public constant, reopen from the password alone. */
  useDefaultSalt?: boolean | null;
  /**
   * Open an existing vault, never create one. The unlock dialog's Open sets it:
   * a folder with no vault there is an error to show, not an empty folder to
   * initialise (#1081 row 43).
   */
  openOnly?: boolean | null;
}

/**
 * The overlay a saved profile's binding asks for on connect (auto-unlock).
 *
 * Every intent the binding records has to travel with the unlock, because the
 * backend reads a missing one as "off". `useDefaultSalt` is the case that went
 * wrong (#276): the connect dropped it, so a profile saved with "Default salt"
 * on created its vault with a random per-vault salt, and once the marker and
 * keystore copy are gone it is also what lets the password alone reopen it.
 */
export function profileOverlayApplyParams(input: {
  binding: AeroCryptOverlayBinding;
  savedServerId: string;
  overlayScope: string;
  password: string;
  salt: string;
  keyfilePath: string;
}): ProviderCryptOverlayApply {
  const { binding } = input;
  return {
    kind: binding.kind,
    remoteScope: input.overlayScope,
    filenameEncryption: binding.filenameEncryption || 'standard',
    directoryNameEncryption: binding.directoryNameEncryption ?? true,
    password: input.password,
    salt: input.salt || null,
    // No forms here: the backend reads them from the saved profile
    // (profileId), by the rule every other reader uses.
    passwordForm: null,
    saltForm: null,
    keyfilePath: input.keyfilePath || null,
    profileId: input.savedServerId,
    // Headed intent from the saved profile: missing remote marker is
    // healed from the keystore with a one-shot safety toast (#421 #7).
    withHeader: !!binding.withHeader,
    useDefaultSalt: !!binding.useDefaultSalt,
  };
}
