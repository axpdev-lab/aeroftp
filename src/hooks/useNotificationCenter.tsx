// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The in-app notification center: the DURABLE surface behind the titlebar bell
 * (Finding 8). A received file with auto-accept on is otherwise silent and the
 * toast is ephemeral, so the user needs a place the event PERSISTS - the bell
 * dropdown, backed by this store.
 *
 * Scope today: AeroShare send/receive/failure events (pushed by AeroShareHub).
 * Built generic so other subsystems can `notify(...)` later. Persisted to
 * localStorage so history survives a restart (the owner wants the notification
 * to stay visible in the titlebar), capped to {@link MAX_NOTIFICATIONS}.
 */

import * as React from 'react';

export type NotificationKind = 'receive' | 'send' | 'error' | 'knock';

export interface AppNotification {
  id: string;
  kind: NotificationKind;
  title: string;
  body?: string;
  /** Epoch ms of the event. */
  ts: number;
  read: boolean;
  /** Absolute path of the related file, when one applies (enables "open folder"). */
  filePath?: string | null;
}

export interface NotificationCenterValue {
  notifications: AppNotification[];
  unreadCount: number;
  /** Push a new notification (prepended, marked unread). Returns its id. */
  notify: (n: Omit<AppNotification, 'id' | 'ts' | 'read'> & { ts?: number }) => string;
  markAllRead: () => void;
  remove: (id: string) => void;
  clear: () => void;
}

const STORAGE_KEY = 'aeroftp_notifications';
const MAX_NOTIFICATIONS = 50;

const NotificationCenterContext = React.createContext<NotificationCenterValue | null>(null);

function loadPersisted(): AppNotification[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    // Defensive: keep only well-formed entries (older/foreign data is ignored).
    return parsed
      .filter(
        (n): n is AppNotification =>
          n && typeof n.id === 'string' && typeof n.title === 'string' && typeof n.ts === 'number',
      )
      .slice(0, MAX_NOTIFICATIONS);
  } catch {
    return [];
  }
}

export function NotificationCenterProvider({
  children,
}: {
  children: React.ReactNode;
}): React.ReactElement {
  const [notifications, setNotifications] = React.useState<AppNotification[]>(loadPersisted);
  const counter = React.useRef(0);

  // Persist on every change (best-effort; a full localStorage just drops history).
  React.useEffect(() => {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(notifications));
    } catch {
      /* quota / unavailable: history stays in-memory only */
    }
  }, [notifications]);

  const notify = React.useCallback<NotificationCenterValue['notify']>((n) => {
    counter.current += 1;
    // No Date.now()/Math.random() needed for the id: a monotonic counter plus the
    // provided/explicit ts is unique within a session and stable for React keys.
    const id = `ntf-${counter.current}-${n.ts ?? 0}`;
    const entry: AppNotification = {
      id,
      kind: n.kind,
      title: n.title,
      body: n.body,
      filePath: n.filePath ?? null,
      ts: n.ts ?? Date.now(),
      read: false,
    };
    setNotifications((prev) => [entry, ...prev].slice(0, MAX_NOTIFICATIONS));
    return id;
  }, []);

  const markAllRead = React.useCallback(() => {
    setNotifications((prev) =>
      prev.some((n) => !n.read) ? prev.map((n) => (n.read ? n : { ...n, read: true })) : prev,
    );
  }, []);

  const remove = React.useCallback((id: string) => {
    setNotifications((prev) => prev.filter((n) => n.id !== id));
  }, []);

  const clear = React.useCallback(() => setNotifications([]), []);

  const unreadCount = React.useMemo(() => notifications.filter((n) => !n.read).length, [
    notifications,
  ]);

  const value = React.useMemo<NotificationCenterValue>(
    () => ({ notifications, unreadCount, notify, markAllRead, remove, clear }),
    [notifications, unreadCount, notify, markAllRead, remove, clear],
  );

  return (
    <NotificationCenterContext.Provider value={value}>
      {children}
    </NotificationCenterContext.Provider>
  );
}

/**
 * Read the notification center. Returns a SAFE no-op fallback when used outside
 * the provider (e.g. an isolated test render) so a missing provider degrades
 * gracefully instead of throwing.
 */
export function useNotificationCenter(): NotificationCenterValue {
  const ctx = React.useContext(NotificationCenterContext);
  if (ctx) return ctx;
  return {
    notifications: [],
    unreadCount: 0,
    notify: () => '',
    markAllRead: () => {},
    remove: () => {},
    clear: () => {},
  };
}

export default useNotificationCenter;
