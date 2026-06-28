// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * One-time prompt shown the FIRST time AeroShare auto-activates (the user adds a
 * friend or shares a folder, which flips `aeroShareEnabled` on and fires
 * AERO_SHARE_ACTIVATED_EVENT). AeroShare itself is now active either way; this
 * prompt only decides the RECEIVER (the standing P2P listener), which is never
 * started silently: the user explicitly opts IN ("Turn on receiving") or OUT
 * ("Not now"). The choice is recorded so the prompt never shows again
 * (`aeroShareReceivePrompted`); both receiving and auto-accept stay changeable
 * from Settings afterwards.
 */

import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { Inbox, X } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { AERO_SHARE_ACTIVATED_EVENT, patchAeroShareSettings } from '../../utils/aeroShare';
import { secureGetWithFallback } from '../../utils/secureStorage';

const SETTINGS_VAULT_KEY = 'app_settings';
const SETTINGS_LOCAL_KEY = 'aeroftp_settings';

export function AeroShareActivationPrompt() {
  const t = useTranslation();
  const [open, setOpen] = useState(false);
  const [autoAccept, setAutoAccept] = useState(true);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const onActivated = () => {
      void (async () => {
        // Belt-and-suspenders: never re-prompt if the user already answered.
        try {
          const s = await secureGetWithFallback<Record<string, unknown>>(
            SETTINGS_VAULT_KEY,
            SETTINGS_LOCAL_KEY,
          );
          if (s?.aeroShareReceivePrompted === true) return;
        } catch {
          /* show the prompt on read failure rather than swallow it */
        }
        setOpen(true);
      })();
    };
    window.addEventListener(AERO_SHARE_ACTIVATED_EVENT, onActivated);
    return () => window.removeEventListener(AERO_SHARE_ACTIVATED_EVENT, onActivated);
  }, []);

  const answer = async (receiving: boolean) => {
    if (busy) return;
    setBusy(true);
    try {
      await patchAeroShareSettings({
        aeroShareReceivePrompted: true,
        aeroShareReceiving: receiving,
        // Only meaningful when receiving; harmless otherwise.
        ...(receiving ? { aeroShareAutoAcceptFriends: autoAccept } : {}),
      });
    } finally {
      setBusy(false);
      setOpen(false);
    }
  };

  if (!open) return null;

  return createPortal(
    <div
      className="fixed inset-0 z-[10000] flex items-center justify-center bg-black/50"
      onClick={() => void answer(false)}
    >
      <div
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[440px] max-w-[92vw] flex flex-col animate-scale-in"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
      >
        <div className="flex items-start gap-3 p-5 pb-3">
          <div className="shrink-0 w-10 h-10 rounded-lg bg-violet-100 dark:bg-violet-900/40 flex items-center justify-center">
            <Inbox size={20} className="text-violet-600 dark:text-violet-400" />
          </div>
          <div className="flex-1 min-w-0">
            <h2 className="text-base font-semibold text-gray-900 dark:text-gray-100">
              {t('aeroShare.activate.title')}
            </h2>
            <p className="mt-1 text-sm text-gray-600 dark:text-gray-300">
              {t('aeroShare.activate.body')}
            </p>
          </div>
          <button
            onClick={() => void answer(false)}
            className="shrink-0 text-gray-400 hover:text-gray-600 dark:hover:text-gray-200 transition-colors"
            title={t('aeroShare.activate.notNow')}
          >
            <X size={18} />
          </button>
        </div>

        <label className="flex items-center gap-2 px-5 py-2 text-sm text-gray-700 dark:text-gray-200 cursor-pointer">
          <input
            type="checkbox"
            checked={autoAccept}
            onChange={(e) => setAutoAccept(e.target.checked)}
            className="rounded border-gray-300 dark:border-gray-600 text-violet-600 focus:ring-violet-500"
          />
          {t('aeroShare.autoAcceptLabel')}
        </label>

        <div className="flex items-center justify-end gap-2 p-4 pt-3">
          <button
            onClick={() => void answer(false)}
            disabled={busy}
            className="px-4 py-2 rounded-lg text-sm font-medium text-gray-700 dark:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors disabled:opacity-50"
          >
            {t('aeroShare.activate.notNow')}
          </button>
          <button
            onClick={() => void answer(true)}
            disabled={busy}
            className="px-4 py-2 rounded-lg text-sm font-semibold text-white bg-violet-600 hover:bg-violet-500 transition-colors disabled:opacity-50"
          >
            {t('aeroShare.activate.enable')}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

export default AeroShareActivationPrompt;
