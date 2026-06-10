// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Lightweight reader for the `aeroShareEnabled` experimental-feature flag
 * (Settings -> Connection -> AeroShare). Subscribes to the global
 * `aeroftp-settings-changed` custom event so every AeroShare surface (friend
 * cards, the "Add friend" entry, the handshake dialog) can appear/disappear
 * the instant the toggle changes, without prop-drilling through IntroHub.
 *
 * The full {@link useSettings} hook is intentionally avoided here: it owns
 * many setters and a vault round-trip on mount, both unnecessary for a
 * read-only consumer. Mirrors {@link useCardLayout}.
 *
 * Default OFF (D-GUI-2): AeroShare ships behind the flag until the
 * release-gating checklist (P2P-GUI-DESIGN.md §9) clears.
 */

import { useEffect, useState } from 'react';
import { secureGetWithFallback } from '../utils/secureStorage';

const SETTINGS_KEY = 'aeroftp_settings';
const SETTINGS_VAULT_KEY = 'app_settings';

const parse = (raw: unknown): boolean => raw === true;

export const useAeroShareEnabled = (): boolean => {
  const [enabled, setEnabled] = useState(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const parsed = await secureGetWithFallback<Record<string, unknown>>(
          SETTINGS_VAULT_KEY,
          SETTINGS_KEY,
        );
        if (!cancelled && parsed) setEnabled(parse(parsed.aeroShareEnabled));
      } catch {
        /* keep default (off) */
      }
    })();

    const onChange = (e: Event) => {
      const detail = (e as CustomEvent)?.detail as Record<string, unknown> | undefined;
      if (!detail) return;
      if ('aeroShareEnabled' in detail) setEnabled(parse(detail.aeroShareEnabled));
    };
    window.addEventListener('aeroftp-settings-changed', onChange);
    return () => {
      cancelled = true;
      window.removeEventListener('aeroftp-settings-changed', onChange);
    };
  }, []);

  return enabled;
};

export default useAeroShareEnabled;
