// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroShareHub - the always-mounted (while AeroShare is enabled) owner of the
 * "Send file to user" one-shot surface:
 *  - the RECEIVE loop lifecycle (started/stopped by the Ricezione setting);
 *  - the incoming Accept/Decline prompt (auto-accepted for saved friends when
 *    the opt-in is on; unknown senders always prompt);
 *  - the Send-file dialog (opened from the file context menu via a global event);
 *  - a lightweight session Inbox of received files;
 *  - its own toasts (useToast is component-local, so it renders its own container).
 *
 * Self-contained on purpose: it needs no App-level state, so App just mounts it
 * once behind the flag. All backend talk goes through the typed wrappers in
 * utils/aeroShare.ts; all state flows over the `peer://incoming-*` events.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { Send, Download, X, Check, Inbox, Users, Loader2 } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { ToastContainer, useToast } from '../Toast';
import { loadSavedServerProfiles } from '../../utils/serverProfileStore';
import { useAeroShareReceiveSettings } from '../../hooks/useAeroShareReceiveSettings';
import {
  peerSendFile,
  peerReceiverStart,
  peerReceiverStop,
  peerFriendsList,
  peerFriendsPresence,
  peerIncomingRespond,
  shortAfid,
  formatBytes,
  basenameOf,
  AERO_SHARE_SEND_EVENT,
  AERO_SHARE_INBOX_EVENT,
  type AeroShareSendDetail,
  type PeerFriend,
  type PeerIncomingOfferEvent,
  type PeerIncomingStatusEvent,
} from '../../utils/aeroShare';

interface IncomingPrompt {
  offer: PeerIncomingOfferEvent;
  /** Resolved friend alias (when the sender is a saved friend). */
  alias?: string;
}

interface InboxItem {
  name: string;
  senderAfid: string;
  senderAlias?: string;
  path: string | null;
  atMs: number;
}

/** Resolve a sender AFID to a saved-friend alias (or undefined when unknown). */
async function resolveFriendAlias(afid: string): Promise<string | undefined> {
  try {
    const profiles = await loadSavedServerProfiles();
    const friend = profiles.find((p) => p.protocol === 'peer' && p.host === afid);
    const alias = friend?.username?.trim() || friend?.name?.trim();
    return alias || undefined;
  } catch {
    return undefined;
  }
}

export function AeroShareHub() {
  const t = useTranslation();
  const { receiving, autoAcceptFriends } = useAeroShareReceiveSettings();

  // Component-local toasts (useToast is not a shared context).
  const { toasts, removeToast, success, error, info } = useToast();

  const [incoming, setIncoming] = useState<IncomingPrompt | null>(null);
  const [sendTarget, setSendTarget] = useState<AeroShareSendDetail | null>(null);
  const [inboxOpen, setInboxOpen] = useState(false);
  const [inbox, setInbox] = useState<InboxItem[]>([]);

  // autoAccept read at event time without re-subscribing the listener.
  const autoAcceptRef = useRef(autoAcceptFriends);
  useEffect(() => {
    autoAcceptRef.current = autoAcceptFriends;
  }, [autoAcceptFriends]);

  // ---- Receive loop lifecycle (the Ricezione toggle) ----
  useEffect(() => {
    let active = true;
    if (receiving) {
      peerReceiverStart().catch((e) => {
        if (active) error(t('aeroShare.receive.startError'), String(e));
      });
    } else {
      peerReceiverStop().catch(() => {
        /* best-effort */
      });
    }
    return () => {
      active = false;
    };
  }, [receiving, error, t]);

  // Stop the receiver when the hub unmounts (flag turned off / app teardown).
  useEffect(
    () => () => {
      peerReceiverStop().catch(() => {
        /* best-effort */
      });
    },
    [],
  );

  // ---- peer://incoming-offer: prompt or auto-accept ----
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    (async () => {
      unlisten = await listen<PeerIncomingOfferEvent>('peer://incoming-offer', async (e) => {
        const offer = e.payload;
        const alias = await resolveFriendAlias(offer.sender_afid);
        if (autoAcceptRef.current && alias) {
          // Known friend + opt-in on: accept silently, show a passive notice.
          peerIncomingRespond({
            transferId: offer.transfer_id,
            accept: true,
            senderLabel: alias,
          }).catch((err) => error(t('aeroShare.receive.failedTitle'), String(err)));
          info(
            t('aeroShare.receive.receivingTitle'),
            t('aeroShare.receive.receivingDesc', { name: offer.name, sender: alias }),
          );
        } else {
          setIncoming({ offer, alias });
        }
      });
    })();
    return () => {
      if (unlisten) unlisten();
    };
  }, [error, info, t]);

  // ---- peer://incoming-status: outcome toasts + session inbox ----
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    (async () => {
      unlisten = await listen<PeerIncomingStatusEvent>('peer://incoming-status', async (e) => {
        const ev = e.payload;
        const alias = await resolveFriendAlias(ev.sender_afid);
        const senderLabel = alias || shortAfid(ev.sender_afid);
        if (ev.state === 'completed') {
          setInbox((prev) => [
            { name: ev.name, senderAfid: ev.sender_afid, senderAlias: alias, path: ev.path, atMs: ev.at_ms },
            ...prev,
          ]);
          success(
            t('aeroShare.receive.completedTitle'),
            t('aeroShare.receive.completedDesc', { name: ev.name, sender: senderLabel }),
          );
        } else if (ev.state === 'failed') {
          error(
            t('aeroShare.receive.failedTitle'),
            `${ev.name}${ev.error ? ` · ${ev.error}` : ''}`,
          );
        }
        // 'declined' is the user's own choice: no toast.
      });
    })();
    return () => {
      if (unlisten) unlisten();
    };
  }, [success, error, t]);

  // ---- global open events (send dialog / inbox) ----
  useEffect(() => {
    const onSend = (e: Event) => {
      const detail = (e as CustomEvent<AeroShareSendDetail>).detail;
      if (detail?.filePath) setSendTarget(detail);
    };
    const onInbox = () => setInboxOpen(true);
    window.addEventListener(AERO_SHARE_SEND_EVENT, onSend);
    window.addEventListener(AERO_SHARE_INBOX_EVENT, onInbox);
    return () => {
      window.removeEventListener(AERO_SHARE_SEND_EVENT, onSend);
      window.removeEventListener(AERO_SHARE_INBOX_EVENT, onInbox);
    };
  }, []);

  const acceptIncoming = useCallback(() => {
    if (!incoming) return;
    const { offer, alias } = incoming;
    peerIncomingRespond({ transferId: offer.transfer_id, accept: true, senderLabel: alias }).catch(
      (err) => error(t('aeroShare.receive.failedTitle'), String(err)),
    );
    setIncoming(null);
  }, [incoming, error, t]);

  const declineIncoming = useCallback(() => {
    if (!incoming) return;
    peerIncomingRespond({ transferId: incoming.offer.transfer_id, accept: false }).catch(() => {
      /* best-effort */
    });
    setIncoming(null);
  }, [incoming]);

  return (
    <>
      <ToastContainer toasts={toasts} onRemove={removeToast} />

      {incoming && (
        <IncomingOfferModal
          prompt={incoming}
          onAccept={acceptIncoming}
          onDecline={declineIncoming}
        />
      )}

      {sendTarget && (
        <SendFileDialog
          filePath={sendTarget.filePath}
          onSent={(name, friend) =>
            success(t('aeroShare.send.sent'), t('aeroShare.send.sentDesc', { name, friend }))
          }
          onError={(msg) => error(t('aeroShare.send.failed'), msg)}
          onClose={() => setSendTarget(null)}
        />
      )}

      {inboxOpen && (
        <InboxModal items={inbox} receiving={receiving} onClose={() => setInboxOpen(false)} />
      )}
    </>
  );
}

// ---------------------------------------------------------------------------
// Incoming Accept/Decline modal
// ---------------------------------------------------------------------------

function IncomingOfferModal({
  prompt,
  onAccept,
  onDecline,
}: {
  prompt: IncomingPrompt;
  onAccept: () => void;
  onDecline: () => void;
}) {
  const t = useTranslation();
  const { offer, alias } = prompt;
  const senderLabel = alias || t('aeroShare.receive.unknownSender');

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onDecline();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onDecline]);

  return (
    <div className="fixed inset-0 z-[10000] flex items-center justify-center bg-black/50">
      <div
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[420px] animate-scale-in"
        role="dialog"
        aria-modal="true"
      >
        <div className="flex items-center gap-2 px-4 py-3 border-b border-gray-100 dark:border-gray-700">
          <Download size={18} className="text-violet-500" />
          <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
            {t('aeroShare.receive.offerTitle')}
          </h2>
        </div>
        <div className="px-4 py-4 text-sm text-gray-700 dark:text-gray-300">
          <p>
            {t('aeroShare.receive.offerMessage', {
              sender: senderLabel,
              name: offer.name,
              size: formatBytes(offer.size),
            })}
          </p>
          {!alias && (
            <p className="mt-2 text-xs text-amber-600 dark:text-amber-400">
              {shortAfid(offer.sender_afid)}
            </p>
          )}
        </div>
        <div className="flex justify-end gap-2 px-4 py-3 border-t border-gray-100 dark:border-gray-700">
          <button
            onClick={onDecline}
            className="px-3 py-1.5 text-sm rounded-md border border-gray-300 dark:border-gray-600 text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
          >
            {t('aeroShare.receive.decline')}
          </button>
          <button
            onClick={onAccept}
            className="px-3 py-1.5 text-sm rounded-md bg-violet-600 text-white hover:bg-violet-500 flex items-center gap-1.5"
          >
            <Check size={14} />
            {t('aeroShare.receive.accept')}
          </button>
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Send-file dialog (friend picker)
// ---------------------------------------------------------------------------

function SendFileDialog({
  filePath,
  onSent,
  onError,
  onClose,
}: {
  filePath: string;
  onSent: (name: string, friend: string) => void;
  onError: (msg: string) => void;
  onClose: () => void;
}) {
  const t = useTranslation();
  const [friends, setFriends] = useState<PeerFriend[]>([]);
  const [presence, setPresence] = useState<Record<string, boolean>>({});
  const [probing, setProbing] = useState(false);
  const [manualAfid, setManualAfid] = useState('');
  const [sending, setSending] = useState(false);
  const fileName = basenameOf(filePath);

  useEffect(() => {
    let cancelled = false;
    peerFriendsList()
      .then(async (list) => {
        if (cancelled) return;
        setFriends(list);
        if (list.length === 0) return;
        // Presence: who is online/receiving right now (best-effort).
        setProbing(true);
        try {
          const online = await peerFriendsPresence(list.map((f) => f.afid));
          if (cancelled) return;
          const map: Record<string, boolean> = {};
          list.forEach((f, i) => {
            map[f.afid] = !!online[i];
          });
          setPresence(map);
        } catch {
          /* leave presence unknown */
        } finally {
          if (!cancelled) setProbing(false);
        }
      })
      .catch(() => setFriends([]));
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !sending) onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose, sending]);

  const send = useCallback(
    async (recipientAfid: string, friendLabel: string) => {
      if (sending) return;
      setSending(true);
      try {
        await peerSendFile({ recipientAfid, filePath });
        onSent(fileName, friendLabel);
        onClose();
      } catch (e) {
        onError(String(e));
      } finally {
        setSending(false);
      }
    },
    [sending, filePath, fileName, onSent, onError, onClose],
  );

  return (
    <div
      className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50"
      onClick={() => !sending && onClose()}
    >
      <div
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[460px] max-h-[80vh] flex flex-col animate-scale-in"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-gray-100 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <Send size={18} className="text-violet-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('aeroShare.send.title')}
            </h2>
          </div>
          <button
            onClick={() => !sending && onClose()}
            className="p-1.5 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-400"
          >
            <X size={14} />
          </button>
        </div>

        <div className="px-4 py-3 overflow-y-auto">
          <p className="text-xs text-gray-500 mb-3">
            {t('aeroShare.send.file')}: <span className="font-medium text-gray-700 dark:text-gray-300">{fileName}</span>
          </p>

          <p className="text-sm font-medium text-gray-700 dark:text-gray-300 mb-2 flex items-center gap-2">
            {t('aeroShare.send.pickFriend')}
            {probing && <Loader2 size={12} className="animate-spin text-gray-400" />}
          </p>
          {friends.length === 0 ? (
            <p className="text-sm text-gray-500 mb-3">{t('aeroShare.send.noFriends')}</p>
          ) : (
            <div className="space-y-1 mb-3">
              {friends.map((f) => {
                const online = presence[f.afid];
                return (
                  <button
                    key={f.afid}
                    disabled={sending}
                    onClick={() => send(f.afid, f.alias || shortAfid(f.afid))}
                    className="w-full flex items-center gap-2 px-3 py-2 rounded-md text-left text-sm hover:bg-violet-50 dark:hover:bg-violet-900/20 disabled:opacity-50"
                  >
                    <Users size={15} className="text-violet-500 shrink-0" />
                    <span className="flex-1 truncate text-gray-800 dark:text-gray-200">
                      {f.alias || shortAfid(f.afid)}
                    </span>
                    {online !== undefined && (
                      <span
                        className={`text-[10px] flex items-center gap-1 ${online ? 'text-emerald-500' : 'text-gray-400'}`}
                      >
                        <span
                          className={`inline-block w-2 h-2 rounded-full ${online ? 'bg-emerald-500' : 'bg-gray-400'}`}
                        />
                        {online ? t('aeroShare.driveState.live') : t('aeroShare.driveState.offline')}
                      </span>
                    )}
                  </button>
                );
              })}
            </div>
          )}

          <div className="border-t border-gray-100 dark:border-gray-700 pt-3">
            <label className="block text-xs text-gray-500 mb-1">{t('aeroShare.send.orAfid')}</label>
            <div className="flex gap-2">
              <input
                value={manualAfid}
                onChange={(e) => setManualAfid(e.target.value)}
                placeholder={t('aeroShare.send.afidPlaceholder')}
                disabled={sending}
                className="flex-1 px-3 py-1.5 text-sm border border-gray-300 dark:border-gray-600 rounded-md bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100"
              />
              <button
                disabled={sending || !manualAfid.trim()}
                onClick={() => send(manualAfid.trim(), shortAfid(manualAfid.trim()))}
                className="px-3 py-1.5 text-sm rounded-md bg-violet-600 text-white hover:bg-violet-500 disabled:opacity-50 flex items-center gap-1.5"
              >
                {sending ? <Loader2 size={14} className="animate-spin" /> : <Send size={14} />}
                {sending ? t('aeroShare.send.sending') : t('aeroShare.send.sendButton')}
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Session Inbox modal (lightweight list of files received this session)
// ---------------------------------------------------------------------------

function InboxModal({
  items,
  receiving,
  onClose,
}: {
  items: InboxItem[];
  receiving: boolean;
  onClose: () => void;
}) {
  const t = useTranslation();

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={onClose}>
      <div
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[480px] max-h-[80vh] flex flex-col animate-scale-in"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
      >
        <div className="flex items-center justify-between px-4 py-3 border-b border-gray-100 dark:border-gray-700">
          <div className="flex items-center gap-2">
            <Inbox size={18} className="text-violet-500" />
            <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
              {t('aeroShare.inbox.title')}
            </h2>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-400"
          >
            <X size={14} />
          </button>
        </div>
        <div className="px-4 py-3 overflow-y-auto">
          {!receiving && (
            <p className="text-xs text-amber-600 dark:text-amber-400 mb-3">
              {t('aeroShare.inbox.receiverOff')}
            </p>
          )}
          {items.length === 0 ? (
            <p className="text-sm text-gray-500">{t('aeroShare.inbox.empty')}</p>
          ) : (
            <div className="space-y-1">
              {items.map((it, i) => (
                <div
                  key={`${it.name}-${it.atMs}-${i}`}
                  className="flex items-center gap-2 px-2 py-2 rounded-md hover:bg-gray-50 dark:hover:bg-gray-700/40"
                >
                  <Download size={15} className="text-violet-500 shrink-0" />
                  <div className="flex-1 min-w-0">
                    <p className="text-sm text-gray-800 dark:text-gray-200 truncate">{it.name}</p>
                    <p className="text-xs text-gray-400 truncate">
                      {t('aeroShare.inbox.from', {
                        sender: it.senderAlias || shortAfid(it.senderAfid),
                      })}
                      {it.path ? ` · ${it.path}` : ''}
                    </p>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export default AeroShareHub;
