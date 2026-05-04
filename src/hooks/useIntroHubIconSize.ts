// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useState } from 'react';
import { secureGetWithFallback } from '../utils/secureStorage';
import {
  clampIntroHubIconSize,
  DEFAULT_INTRO_HUB_ICON_SIZE,
} from './useSettings';

const SETTINGS_KEY = 'aeroftp_settings';
const SETTINGS_VAULT_KEY = 'app_settings';

/**
 * Lightweight reader for the IntroHub provider-icon size preference. Mirrors
 * useCardLayout so card components can react to Settings changes without
 * prop-drilling through IntroHub.
 */
export const useIntroHubIconSize = (): number => {
  const [iconSize, setIconSize] = useState<number>(DEFAULT_INTRO_HUB_ICON_SIZE);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const parsed = await secureGetWithFallback<Record<string, unknown>>(
          SETTINGS_VAULT_KEY,
          SETTINGS_KEY,
        );
        if (!cancelled && parsed) {
          setIconSize(clampIntroHubIconSize(parsed.introHubIconSize));
        }
      } catch {
        /* keep default */
      }
    })();

    const onChange = (e: Event) => {
      const detail = (e as CustomEvent)?.detail as Record<string, unknown> | undefined;
      if (!detail || !('introHubIconSize' in detail)) return;
      setIconSize(clampIntroHubIconSize(detail.introHubIconSize));
    };
    window.addEventListener('aeroftp-settings-changed', onChange);
    return () => {
      cancelled = true;
      window.removeEventListener('aeroftp-settings-changed', onChange);
    };
  }, []);

  return iconSize;
};
