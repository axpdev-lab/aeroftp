// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroShare Share surface, slice 2: the "Shared by me" panel.
 *
 * A dedicated section inside My Servers listing the folders the active user is
 * sharing (so they do not forget what they share), each with its drive name,
 * folder path, recipients, a serving/idle dot, and management actions:
 *   - Stop:    cancel the live publish task (the share stays in the list, idle).
 *   - Re-serve: bring an idle share back up (reuses the drive key; P1 has no
 *               auto-republish, so every share is idle after a restart).
 *   - Remove:   forget the share from this list (with a confirm dialog).
 *
 * Data is the durable share registry (`peer_shares_list`), which survives
 * restarts because the encrypted drive record remembers neither folder nor
 * recipients. Live serving state is reconciled from `peer://share-status`
 * events and an on-demand re-pull after each action.
 */

import * as React from 'react';
import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { FolderUp, Share2, Square, Play, Trash2, Users } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { AlertDialog } from '../Dialogs';
import { logger } from '../../utils/logger';
import {
  peerSharesList,
  peerShareStop,
  peerShareResume,
  peerShareRemove,
  shortAfid,
  type PeerShareInfo,
} from '../../utils/aeroShare';

interface SharedByMePanelProps {
  /** The AeroShare experimental flag (the panel is inert when off). */
  enabled: boolean;
  /** Open the AeroShare dialog on the share tab (the section's add affordance). */
  onShareFolder: () => void;
  /** Bump to force a re-pull (e.g. after the dialog created a new share). */
  refreshKey?: number;
}

export function SharedByMePanel({ enabled, onShareFolder, refreshKey = 0 }: SharedByMePanelProps) {
  const t = useTranslation();
  const [shares, setShares] = useState<PeerShareInfo[]>([]);
  const [busy, setBusy] = useState<Set<string>>(new Set());
  const [removeTarget, setRemoveTarget] = useState<PeerShareInfo | null>(null);

  const reload = useCallback(() => {
    if (!enabled) {
      setShares([]);
      return;
    }
    peerSharesList()
      .then(setShares)
      .catch(() => {
        /* no identity / not ready: leave the list as-is */
      });
  }, [enabled]);

  // Initial + on-demand pull.
  useEffect(() => {
    reload();
  }, [reload, refreshKey]);

  // A share going up/down (start_share, stop, resume) re-pulls the serving set.
  useEffect(() => {
    if (!enabled) return;
    let active = true;
    const p = listen('peer://share-status', () => {
      if (active) reload();
    });
    return () => {
      active = false;
      void p.then((u) => u());
    };
  }, [enabled, reload]);

  const withBusy = useCallback(
    async (namespace: string, op: () => Promise<void>) => {
      setBusy((prev) => new Set(prev).add(namespace));
      try {
        await op();
      } catch (e) {
        logger.error('AeroShare: share action failed', e);
      } finally {
        setBusy((prev) => {
          const next = new Set(prev);
          next.delete(namespace);
          return next;
        });
        reload();
      }
    },
    [reload],
  );

  const confirmRemove = useCallback(() => {
    const target = removeTarget;
    setRemoveTarget(null);
    if (target) void withBusy(target.namespace, () => peerShareRemove(target.namespace));
  }, [removeTarget, withBusy]);

  if (!enabled || shares.length === 0) return null;

  return (
    <div className="px-1 pb-3">
      <div className="rounded-xl border border-violet-200 dark:border-violet-500/30 bg-violet-50/60 dark:bg-violet-900/10 overflow-hidden">
        <div className="flex items-center justify-between gap-2 px-3 py-2 border-b border-violet-200/70 dark:border-violet-500/20">
          <div className="flex items-center gap-2 min-w-0">
            <Share2 size={15} className="text-violet-600 dark:text-violet-300 shrink-0" />
            <div className="min-w-0">
              <h3 className="text-sm font-semibold text-violet-800 dark:text-violet-200 truncate">
                {t('aeroShare.shared.title')}
                <span className="ml-1.5 text-xs font-normal text-violet-500 dark:text-violet-400">
                  {shares.length}
                </span>
              </h3>
              <p className="text-[11px] text-violet-500/80 dark:text-violet-400/70 truncate">
                {t('aeroShare.shared.subtitle')}
              </p>
            </div>
          </div>
          <button
            onClick={onShareFolder}
            className="flex items-center gap-1.5 shrink-0 px-2.5 py-1.5 bg-violet-600 hover:bg-violet-500 text-white rounded-lg text-xs font-semibold transition-colors"
          >
            <FolderUp size={14} />
            {t('aeroShare.shared.shareFolder')}
          </button>
        </div>

        <ul className="divide-y divide-violet-200/50 dark:divide-violet-500/15">
          {shares.map((s) => {
            const isBusy = busy.has(s.namespace);
            return (
              <li key={s.namespace} className="flex items-center gap-3 px-3 py-2.5">
                <span
                  className={`shrink-0 w-2.5 h-2.5 rounded-full ${
                    s.serving ? 'bg-emerald-500' : 'bg-gradient-to-br from-blue-600 to-blue-800'
                  }`}
                  title={s.serving ? t('aeroShare.shared.serving') : t('aeroShare.shared.idle')}
                />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2 min-w-0">
                    <span className="text-sm font-medium text-gray-800 dark:text-gray-100 truncate">
                      {s.drive_name}
                    </span>
                    <span className="text-[11px] text-gray-500 dark:text-gray-400">
                      {s.serving ? t('aeroShare.shared.serving') : t('aeroShare.shared.idle')}
                    </span>
                  </div>
                  <div className="text-xs text-gray-500 dark:text-gray-400 truncate" title={s.folder}>
                    {s.folder}
                  </div>
                  <div className="flex items-center gap-1 mt-0.5 text-[11px] text-gray-500 dark:text-gray-400 min-w-0">
                    <Users size={11} className="shrink-0" />
                    {s.recipients.length === 0 ? (
                      <span className="italic">{t('aeroShare.shared.noRecipients')}</span>
                    ) : (
                      <span className="truncate">
                        {t('aeroShare.shared.sharedWith')}:{' '}
                        {s.recipients.map((r) => r.alias || shortAfid(r.afid)).join(', ')}
                      </span>
                    )}
                  </div>
                </div>

                <div className="flex items-center gap-1 shrink-0">
                  {s.serving ? (
                    <button
                      onClick={() => void withBusy(s.namespace, () => peerShareStop(s.namespace))}
                      disabled={isBusy}
                      className="flex items-center gap-1 px-2 py-1 rounded-md text-xs font-medium text-amber-700 dark:text-amber-300 bg-amber-100 dark:bg-amber-900/30 hover:bg-amber-200 dark:hover:bg-amber-900/50 disabled:opacity-50 transition-colors"
                      title={t('aeroShare.shared.stop')}
                    >
                      <Square size={12} />
                      {t('aeroShare.shared.stop')}
                    </button>
                  ) : (
                    <button
                      onClick={() => void withBusy(s.namespace, () => peerShareResume(s.namespace))}
                      disabled={isBusy}
                      className="flex items-center gap-1 px-2 py-1 rounded-md text-xs font-medium text-emerald-700 dark:text-emerald-300 bg-emerald-100 dark:bg-emerald-900/30 hover:bg-emerald-200 dark:hover:bg-emerald-900/50 disabled:opacity-50 transition-colors"
                      title={t('aeroShare.shared.resume')}
                    >
                      <Play size={12} />
                      {t('aeroShare.shared.resume')}
                    </button>
                  )}
                  <button
                    onClick={() => setRemoveTarget(s)}
                    disabled={isBusy}
                    className="flex items-center justify-center w-7 h-7 rounded-md text-gray-500 dark:text-gray-400 hover:text-red-600 dark:hover:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/20 disabled:opacity-50 transition-colors"
                    title={t('aeroShare.shared.remove')}
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      </div>

      {removeTarget && (
        <AlertDialog
          title={t('aeroShare.shared.removeTitle')}
          message={t('aeroShare.shared.removeMessage').replace('{name}', removeTarget.drive_name)}
          type="warning"
          onClose={() => setRemoveTarget(null)}
          actionLabel={t('aeroShare.shared.remove')}
          onAction={confirmRemove}
          actionIcon={<Trash2 size={14} />}
        />
      )}
    </div>
  );
}

export default SharedByMePanel;
