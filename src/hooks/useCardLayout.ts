// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Lightweight reader for the `cardLayout` Appearance setting. Subscribes to
 * the global `aeroftp-settings-changed` custom event so card components can
 * react to a Settings toggle without prop-drilling through IntroHub.
 *
 * The full {@link useSettings} hook is intentionally avoided here: it owns
 * many setters and a vault round-trip on mount, both unnecessary for a
 * read-only consumer.
 */

import { useEffect, useState } from 'react';
import { secureGetWithFallback } from '../utils/secureStorage';

const SETTINGS_KEY = 'aeroftp_settings';
const SETTINGS_VAULT_KEY = 'app_settings';

export type CardLayout = 'compact' | 'detailed';

const parse = (raw: unknown): CardLayout => {
  return raw === 'detailed' ? 'detailed' : 'compact';
};

export const useCardLayout = (): CardLayout => {
  const [layout, setLayout] = useState<CardLayout>('compact');

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const parsed = await secureGetWithFallback<Record<string, unknown>>(
          SETTINGS_VAULT_KEY,
          SETTINGS_KEY,
        );
        if (!cancelled && parsed) setLayout(parse(parsed.cardLayout));
      } catch {
        /* keep default */
      }
    })();

    const onChange = (e: Event) => {
      const detail = (e as CustomEvent)?.detail as Record<string, unknown> | undefined;
      if (!detail) return;
      if ('cardLayout' in detail) setLayout(parse(detail.cardLayout));
    };
    window.addEventListener('aeroftp-settings-changed', onChange);
    return () => {
      cancelled = true;
      window.removeEventListener('aeroftp-settings-changed', onChange);
    };
  }, []);

  return layout;
};
