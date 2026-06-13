// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Reactive reader for the two AeroShare "Send file to user" RECEIVE settings
 * (Settings -> Connection, under the AeroShare flag):
 * - `receiving`: the Ricezione toggle. When on, the app runs a standing receive
 *   loop so friends can send files to it. Default OFF (decision: a toggle, not
 *   always-on; an "always-on" mode is a future addition at this same seam).
 * - `autoAcceptFriends`: OPT-IN. When on, incoming sends from a SAVED friend are
 *   accepted without a prompt; unknown senders ALWAYS prompt. Default OFF, so by
 *   default every incoming send shows an Accept/Decline prompt (fail-safe).
 *
 * Mirrors {@link useAeroShareEnabled}: a read-only vault peek on mount plus the
 * global `aeroftp-settings-changed` event, no full useSettings round-trip.
 */

import { useEffect, useState } from 'react';
import { secureGetWithFallback } from '../utils/secureStorage';

const SETTINGS_KEY = 'aeroftp_settings';
const SETTINGS_VAULT_KEY = 'app_settings';

const asBool = (raw: unknown): boolean => raw === true;

export interface AeroShareReceiveSettings {
  receiving: boolean;
  autoAcceptFriends: boolean;
  /** OPT-IN: fire an OS notification for every received file. Default OFF. */
  notifyOnReceive: boolean;
}

export const useAeroShareReceiveSettings = (): AeroShareReceiveSettings => {
  const [settings, setSettings] = useState<AeroShareReceiveSettings>({
    receiving: false,
    autoAcceptFriends: false,
    notifyOnReceive: false,
  });

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const parsed = await secureGetWithFallback<Record<string, unknown>>(
          SETTINGS_VAULT_KEY,
          SETTINGS_KEY,
        );
        if (!cancelled && parsed) {
          setSettings({
            receiving: asBool(parsed.aeroShareReceiving),
            autoAcceptFriends: asBool(parsed.aeroShareAutoAcceptFriends),
            notifyOnReceive: asBool(parsed.aeroShareNotifyOnReceive),
          });
        }
      } catch {
        /* keep defaults (off) */
      }
    })();

    const onChange = (e: Event) => {
      const detail = (e as CustomEvent)?.detail as Record<string, unknown> | undefined;
      if (!detail) return;
      setSettings((prev) => ({
        receiving:
          'aeroShareReceiving' in detail ? asBool(detail.aeroShareReceiving) : prev.receiving,
        autoAcceptFriends:
          'aeroShareAutoAcceptFriends' in detail
            ? asBool(detail.aeroShareAutoAcceptFriends)
            : prev.autoAcceptFriends,
        notifyOnReceive:
          'aeroShareNotifyOnReceive' in detail
            ? asBool(detail.aeroShareNotifyOnReceive)
            : prev.notifyOnReceive,
      }));
    };
    window.addEventListener('aeroftp-settings-changed', onChange);
    return () => {
      cancelled = true;
      window.removeEventListener('aeroftp-settings-changed', onChange);
    };
  }, []);

  return settings;
};

export default useAeroShareReceiveSettings;
