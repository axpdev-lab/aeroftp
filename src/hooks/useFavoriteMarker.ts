// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Lightweight reader for the `favoriteMarker` Appearance setting (#270).
 * Subscribes to the global `aeroftp-settings-changed` custom event so the
 * My Servers card/table can switch between the star and heart marker without
 * prop-drilling through IntroHub.
 *
 * Mirrors {@link useCardLayout}: the full {@link useSettings} hook is avoided
 * here since a read-only consumer does not need its setters or mount round-trip.
 */

import { useEffect, useState } from 'react';
import { onVaultReady, secureGetWithFallback } from '../utils/secureStorage';

const SETTINGS_KEY = 'aeroftp_settings';
const SETTINGS_VAULT_KEY = 'app_settings';

export type FavoriteMarker = 'star' | 'heart';

const parse = (raw: unknown): FavoriteMarker => {
  return raw === 'heart' ? 'heart' : 'star';
};

export const useFavoriteMarker = (): FavoriteMarker => {
  const [marker, setMarker] = useState<FavoriteMarker>('star');

  useEffect(() => {
    let cancelled = false;
    let generation = 0;
    const load = async () => {
      const current = ++generation;
      try {
        const parsed = await secureGetWithFallback<Record<string, unknown>>(
          SETTINGS_VAULT_KEY,
          SETTINGS_KEY,
        );
        if (!cancelled && current === generation && parsed) {
          setMarker(parse(parsed.favoriteMarker));
        }
      } catch {
        /* keep default */
      }
    };
    void load();

    const onChange = (e: Event) => {
      const detail = (e as CustomEvent)?.detail as Record<string, unknown> | undefined;
      if (!detail || !('favoriteMarker' in detail)) return;
      generation++;
      setMarker(parse(detail.favoriteMarker));
    };
    const unsubscribe = onVaultReady(load);
    window.addEventListener('aeroftp-settings-changed', onChange);
    return () => {
      cancelled = true;
      generation++;
      unsubscribe();
      window.removeEventListener('aeroftp-settings-changed', onChange);
    };
  }, []);

  return marker;
};
