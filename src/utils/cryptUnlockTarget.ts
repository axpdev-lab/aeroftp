// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Where an unlock dialog (AeroCrypt or rclone crypt) opens its overlay.
//
// The dialogs used to hand App `remoteScope: ''`, which the apply reads as "the
// whole remote": a vault in a subfolder was opened at the server root, where the
// AeroCrypt backend refused it as a non-empty folder and rclone crypt listed
// nothing (#1081 row 43). The folder now travels with the dialog from the moment
// it opens, so the probe, the create and the apply all look at the same place.

import { normCryptScope } from './cryptScope';

export interface CryptUnlockTarget {
  /** Absolute plaintext folder the overlay is opened at; '/' is the remote root. */
  scope: string;
  /**
   * Set when the dialog unlocks a saved profile's binding (the locked-overlay
   * banner): the binding then decides scope and intents, as on connect.
   */
  savedServerId?: string | null;
}

/**
 * The folder an overlay anchors at, written the way the backend probes read it.
 *
 * `aerocrypt_provider_read_config` and its siblings read an absent or empty
 * `basePath` as "the provider's current folder", so the root has to be spelled
 * `/`: passing `''` for an overlay at the root probed wherever the panel was.
 */
export const overlayAnchor = (scope?: string | null): string => normCryptScope(scope) || '/';
