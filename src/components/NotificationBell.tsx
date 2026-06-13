// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Titlebar notification bell + dropdown (Finding 8): the durable, always-visible
 * surface for AeroShare events. A received file (silent under auto-accept) and
 * its ephemeral toast leave no trace once dismissed; the bell keeps the history
 * with an unread badge, and offers "open the received file's folder" / "open the
 * inbox" right from the dropdown so the inbox is reachable without faking a send.
 */

import * as React from 'react';
import { Bell, FolderOpen, Trash2, Inbox, Check, Download, Upload, AlertCircle } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useNotificationCenter, type NotificationKind } from '../hooks/useNotificationCenter';
import { aeroShareInboxRoot, openInFileManager } from '../utils/aeroShare';

function relativeTime(ts: number, t: ReturnType<typeof useTranslation>): string {
  const sec = Math.max(0, Math.floor((Date.now() - ts) / 1000));
  if (sec < 60) return t('notifications.justNow');
  const min = Math.floor(sec / 60);
  if (min < 60) return t('notifications.minutesAgo', { n: String(min) });
  const hr = Math.floor(min / 60);
  if (hr < 24) return t('notifications.hoursAgo', { n: String(hr) });
  const day = Math.floor(hr / 24);
  return t('notifications.daysAgo', { n: String(day) });
}

const kindIcon: Record<NotificationKind, React.ReactNode> = {
  receive: <Download size={14} className="text-violet-500 shrink-0" />,
  send: <Upload size={14} className="text-blue-500 shrink-0" />,
  error: <AlertCircle size={14} className="text-red-500 shrink-0" />,
};

export const NotificationBell: React.FC = () => {
  const t = useTranslation();
  const { notifications, unreadCount, markAllRead, clear, remove } = useNotificationCenter();
  const [open, setOpen] = React.useState(false);
  const ref = React.useRef<HTMLDivElement>(null);

  React.useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', onClick);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onClick);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  // Opening the panel marks everything read (the badge is "new since last look").
  const toggle = React.useCallback(() => {
    setOpen((wasOpen) => {
      if (!wasOpen) markAllRead();
      return !wasOpen;
    });
  }, [markAllRead]);

  const openInbox = React.useCallback(() => {
    aeroShareInboxRoot()
      .then(openInFileManager)
      .catch(() => {
        /* best-effort */
      });
  }, []);

  const reveal = React.useCallback((path: string) => {
    openInFileManager(path).catch(() => {
      /* best-effort */
    });
  }, []);

  return (
    <div ref={ref} className="relative">
      <button
        onClick={toggle}
        className="relative w-7 h-7 flex items-center justify-center rounded-lg hover:bg-[var(--color-bg-tertiary)] transition-colors cursor-pointer"
        title={t('notifications.title')}
        aria-label={t('notifications.title')}
      >
        <Bell size={14} className="text-[var(--color-text-secondary)]" />
        {unreadCount > 0 && (
          <span className="absolute -top-0.5 -right-0.5 min-w-[15px] h-[15px] px-1 flex items-center justify-center rounded-full bg-red-500 text-white text-[9px] font-bold leading-none">
            {unreadCount > 9 ? '9+' : unreadCount}
          </span>
        )}
      </button>

      {open && (
        <div className="absolute top-full right-0 mt-1 w-[340px] max-h-[70vh] flex flex-col bg-[var(--color-bg-secondary)] border border-[var(--color-border)] rounded-lg shadow-xl z-[9999]">
          <div className="flex items-center justify-between px-3 py-2 border-b border-[var(--color-border)]">
            <span className="text-xs font-semibold text-[var(--color-text-primary)]">
              {t('notifications.title')}
            </span>
            <div className="flex items-center gap-1">
              <button
                onClick={openInbox}
                className="flex items-center gap-1 px-1.5 py-1 rounded text-[11px] text-violet-500 hover:bg-[var(--color-bg-tertiary)]"
                title={t('aeroShare.inbox.openFolder')}
              >
                <Inbox size={12} />
                {t('aeroShare.inbox.openFolder')}
              </button>
              {notifications.length > 0 && (
                <button
                  onClick={clear}
                  className="flex items-center gap-1 px-1.5 py-1 rounded text-[11px] text-[var(--color-text-tertiary)] hover:bg-[var(--color-bg-tertiary)] hover:text-[var(--color-text-primary)]"
                  title={t('notifications.clearAll')}
                >
                  <Trash2 size={12} />
                </button>
              )}
            </div>
          </div>

          <div className="overflow-y-auto py-1">
            {notifications.length === 0 ? (
              <p className="px-3 py-6 text-center text-xs text-[var(--color-text-tertiary)]">
                {t('notifications.empty')}
              </p>
            ) : (
              notifications.map((n) => (
                <div
                  key={n.id}
                  className="group flex items-start gap-2 px-3 py-2 hover:bg-[var(--color-bg-tertiary)]"
                >
                  <div className="mt-0.5">{kindIcon[n.kind]}</div>
                  <div className="flex-1 min-w-0">
                    <p className="text-xs font-medium text-[var(--color-text-primary)] truncate">
                      {n.title}
                    </p>
                    {n.body && (
                      <p className="text-[11px] text-[var(--color-text-secondary)] truncate">
                        {n.body}
                      </p>
                    )}
                    <p className="text-[10px] text-[var(--color-text-tertiary)] mt-0.5">
                      {relativeTime(n.ts, t)}
                    </p>
                  </div>
                  <div className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
                    {n.filePath && (
                      <button
                        onClick={() => reveal(n.filePath as string)}
                        className="p-1 rounded text-[var(--color-text-tertiary)] hover:text-violet-500"
                        title={t('aeroShare.inbox.revealFile')}
                      >
                        <FolderOpen size={13} />
                      </button>
                    )}
                    <button
                      onClick={() => remove(n.id)}
                      className="p-1 rounded text-[var(--color-text-tertiary)] hover:text-[var(--color-text-primary)]"
                      title={t('notifications.dismiss')}
                    >
                      <Check size={13} />
                    </button>
                  </div>
                </div>
              ))
            )}
          </div>
        </div>
      )}
    </div>
  );
};

export default NotificationBell;
