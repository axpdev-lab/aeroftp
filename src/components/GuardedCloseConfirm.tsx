// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * GuardedCloseConfirm: the inline confirm shown when the user tries to close a
 * modal that is busy (operation running) or dirty (unsaved edits). Pair with
 * useGuardedClose. Rendered as a fixed overlay so it sits above its host modal
 * regardless of the host's stacking context.
 */

import React, { useEffect } from 'react';
import { AlertTriangle, Loader2 } from 'lucide-react';
import { useTranslation } from '../i18n';
import type { GuardKind } from '../hooks/useGuardedClose';

interface GuardedCloseConfirmProps {
  kind: GuardKind;
  /** Keep running (busy) / keep editing (dirty). */
  onKeep: () => void;
  /** Stop and close (busy) / discard and close (dirty). */
  onConfirm: () => void;
}

export const GuardedCloseConfirm: React.FC<GuardedCloseConfirmProps> = ({ kind, onKeep, onConfirm }) => {
  const t = useTranslation();
  const busy = kind === 'busy';

  // Esc keeps working (the safe default); it never confirms the destructive action.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onKeep();
      }
    };
    window.addEventListener('keydown', handler, true);
    return () => window.removeEventListener('keydown', handler, true);
  }, [onKeep]);

  return (
    <div
      className="fixed inset-0 z-[10001] flex items-center justify-center bg-black/40 backdrop-blur-[1px]"
      role="alertdialog"
      aria-modal="true"
      onClick={(e) => { if (e.target === e.currentTarget) onKeep(); }}
    >
      <div className="w-[360px] max-w-[90vw] bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl p-5 animate-scale-in">
        <div className="flex items-start gap-3">
          <div className="shrink-0 mt-0.5 text-amber-500">
            {busy ? <Loader2 size={20} className="animate-spin" /> : <AlertTriangle size={20} />}
          </div>
          <div className="min-w-0">
            <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {busy ? t('modalGuard.busyTitle') : t('modalGuard.dirtyTitle')}
            </h3>
            <p className="mt-1 text-xs text-gray-600 dark:text-gray-400 leading-relaxed">
              {busy ? t('modalGuard.busyBody') : t('modalGuard.dirtyBody')}
            </p>
          </div>
        </div>
        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            onClick={onKeep}
            className="px-3 py-1.5 text-xs font-medium rounded-lg bg-gray-100 dark:bg-gray-700 text-gray-700 dark:text-gray-200 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors"
          >
            {busy ? t('modalGuard.wait') : t('modalGuard.keepEditing')}
          </button>
          <button
            type="button"
            onClick={onConfirm}
            className="px-3 py-1.5 text-xs font-medium rounded-lg bg-red-600 hover:bg-red-500 text-white transition-colors"
          >
            {busy ? t('modalGuard.stopAndClose') : t('modalGuard.discardAndClose')}
          </button>
        </div>
      </div>
    </div>
  );
};
