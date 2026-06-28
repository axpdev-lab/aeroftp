// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Status-bar AeroShare receiver indicator + toggle, sitting next to the
 * AeroCloud indicator (Ehud #284 placement, reframed as a status indicator
 * rather than a fourth launcher: the +friend titlebar button, the My Servers
 * button bar and the Discover tile already OPEN the hub, so what was missing was
 * an at-a-glance view of, and a quick toggle for, the standing receive loop).
 *
 * Mirrors the AeroCloud pattern: a colored pill reflects live state (receiving
 * ON = emerald + steady dot, OFF = neutral) and clicking flips the receiver by
 * patching the `aeroShareReceiving` setting. The AeroShareHub reacts to that
 * setting and starts/stops the actual `peer_receiver` loop, so this stays a thin
 * control over the single source of truth (no duplicate loop lifecycle here).
 *
 * Only rendered once AeroShare is activated, consistent with the always-on
 * model, so it never clutters the status bar for users who do not use AeroShare.
 */

import { useState } from 'react';
import { Share2 } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useAeroShareEnabled } from '../hooks/useAeroShareEnabled';
import { useAeroShareReceiveSettings } from '../hooks/useAeroShareReceiveSettings';
import { patchAeroShareSettings } from '../utils/aeroShare';

export function AeroShareStatusButton() {
  const t = useTranslation();
  const aeroShareEnabled = useAeroShareEnabled();
  const { receiving } = useAeroShareReceiveSettings();
  const [busy, setBusy] = useState(false);

  if (!aeroShareEnabled) return null;

  const toggle = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await patchAeroShareSettings({ aeroShareReceiving: !receiving });
    } finally {
      setBusy(false);
    }
  };

  return (
    <button
      type="button"
      onClick={() => void toggle()}
      disabled={busy}
      className={`flex items-center gap-1.5 px-2 py-0.5 rounded transition-colors disabled:opacity-50 ${
        receiving
          ? 'bg-emerald-100 dark:bg-emerald-900/40 text-emerald-600 dark:text-emerald-400'
          : 'hover:bg-gray-200 dark:hover:bg-gray-700'
      }`}
      title={t('aeroShare.receiveLabel')}
      aria-pressed={receiving}
    >
      <Share2 size={12} />
      <span>{t('aeroShare.feature')}</span>
      {receiving && <span className="w-1.5 h-1.5 rounded-full bg-emerald-500" />}
    </button>
  );
}

export default AeroShareStatusButton;
