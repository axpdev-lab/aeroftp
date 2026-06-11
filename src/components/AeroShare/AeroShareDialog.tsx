// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroShare handshake dialog (P2P-GUI-DESIGN.md §6; Phase 1 task 8).
 *
 * Modal chrome ONLY: the overlay, the dialog box, the title header (with the
 * Beta tag + close button) and Escape-to-close. The actual RECEIVE/SHARE
 * handshake form lives in the reusable <AeroShareHandshakeBody>, shared with
 * the ConnectionScreen peer-ADD path so there is ONE form body, no duplication.
 */

import { useEffect } from 'react';
import { X, Share2 } from 'lucide-react';
import { useTranslation } from '../../i18n';
import type { ServerProfile } from '../../types';
import {
  AeroShareHandshakeBody,
  type AeroShareMode,
} from './AeroShareHandshakeBody';

// Re-exported so existing consumers keep importing it from AeroShareDialog.
export type { AeroShareMode };

interface AeroShareDialogProps {
  /** Which tab to open on. */
  initialMode?: AeroShareMode;
  /** Pre-fill the friend's AFID/alias (launched from a friend card). */
  prefillAfid?: string;
  prefillAlias?: string;
  /** Default destination folder for the RECEIVE flow. */
  defaultLocalPath?: string;
  /** Re-pull friends/drives after a successful handshake. */
  onFriendSaved?: () => void;
  /** "Connect now" after receiving a drive: open the dual panel on it. */
  onConnectFriend?: (profile: ServerProfile) => void;
  onClose: () => void;
}

export function AeroShareDialog({
  initialMode = 'receive',
  prefillAfid,
  prefillAlias,
  defaultLocalPath,
  onFriendSaved,
  onConnectFriend,
  onClose,
}: AeroShareDialogProps) {
  const t = useTranslation();

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[520px] max-h-[90vh] flex flex-col animate-scale-in"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={t('aeroShare.dialog.title')}
      >
        {/* Header (title + Beta tag + close); the RECEIVE/SHARE tabs live in the body */}
        <div className="flex items-center justify-between px-4 py-3">
          <div className="flex items-center gap-2">
            <Share2 size={18} className="text-violet-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{t('aeroShare.dialog.title')}</h2>
            <span className="text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded bg-amber-100 dark:bg-amber-900/30 text-amber-700 dark:text-amber-400">
              {t('aeroShare.experimentalTag')}
            </span>
          </div>
          <button onClick={onClose} className="p-1.5 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-400">
            <X size={14} />
          </button>
        </div>

        <AeroShareHandshakeBody
          initialMode={initialMode}
          prefillAfid={prefillAfid}
          prefillAlias={prefillAlias}
          defaultLocalPath={defaultLocalPath}
          onFriendSaved={onFriendSaved}
          onConnectFriend={onConnectFriend}
          onClose={onClose}
        />
      </div>
    </div>
  );
}

export default AeroShareDialog;
