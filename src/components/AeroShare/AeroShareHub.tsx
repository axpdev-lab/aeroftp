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
import { Send, Download, X, Check, Inbox, Users, Loader2, FolderOpen, Hand } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { ToastContainer, useToast } from '../Toast';
import { useActivityLog } from '../../hooks/useActivityLog';
import { useNotificationCenter } from '../../hooks/useNotificationCenter';
import { loadSavedServerProfiles } from '../../utils/serverProfileStore';
import { useAeroShareReceiveSettings } from '../../hooks/useAeroShareReceiveSettings';
import {
  peerSendFile,
  peerReceiverStart,
  peerReceiverStop,
  peerFriendsList,
  peerFriendsPresence,
  peerIncomingRespond,
  peerSendKnock,
  peerContactMute,
  aeroShareNotify,
  aeroShareInboxRoot,
  openInFileManager,
  shortAfid,
  formatBytes,
  basenameOf,
  AERO_SHARE_SEND_EVENT,
  AERO_SHARE_INBOX_EVENT,
  type AeroShareSendDetail,
  type PeerFriend,
  type PeerIncomingOfferEvent,
  type PeerIncomingStatusEvent,
  type PeerKnockEvent,
  type PeerActionEvent,
} from '../../utils/aeroShare';
import { knockLabelKey, knockReplies } from '../../utils/aeroShareKnock';

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

/**
 * Resolve a sender AFID to a saved-friend alias (or undefined when unknown).
 *
 * Looks in the authoritative P2P contact store FIRST (`peer_contacts`, via
 * `peerFriendsList` - the same source the Send dialog lists and where both the
 * CLI `peer contact add` and the GUI add-friend write), then falls back to
 * Phase-1 folder-share friends, which are persisted as `protocol: 'peer'`
 * server profiles (`upsertFriendProfile`) rather than in `peer_contacts`.
 *
 * Before this, the lookup checked ONLY the server profiles, so a contact that
 * lived only in `peer_contacts` (e.g. added via the CLI, or any non-folder-share
 * friend) was treated as unknown: auto-accept never fired and the inbox
 * subfolder fell back to the abbreviated AFID instead of the saved alias.
 */
async function resolveFriendAlias(afid: string): Promise<string | undefined> {
  // Primary: the P2P contact store.
  try {
    const friends = await peerFriendsList();
    const alias = friends.find((f) => f.afid === afid)?.alias?.trim();
    if (alias) return alias;
  } catch {
    /* fall through to the profile lookup */
  }
  // Fallback: folder-share friends saved as peer server profiles.
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
  const { receiving, autoAcceptFriends, notifyOnReceive } = useAeroShareReceiveSettings();

  // Component-local toasts (useToast is not a shared context).
  const { toasts, removeToast, success, error, info } = useToast();

  // Durable surfaces behind the ephemeral toast: the Activity log (Finding 7)
  // and the titlebar notification center (Finding 8). Both are fed from the
  // same receive/send outcomes so a silent auto-accepted receive still leaves a
  // readable, persistent trace.
  const { log } = useActivityLog();
  const { notify } = useNotificationCenter();

  const [incoming, setIncoming] = useState<IncomingPrompt | null>(null);
  const [sendTarget, setSendTarget] = useState<AeroShareSendDetail | null>(null);
  const [inboxOpen, setInboxOpen] = useState(false);
  const [inbox, setInbox] = useState<InboxItem[]>([]);
  // A received QUESTION knock awaiting a one-tap predefined reply.
  const [incomingKnock, setIncomingKnock] = useState<{
    senderAfid: string;
    senderLabel: string;
    code: string;
    replies: string[];
  } | null>(null);

  // autoAccept read at event time without re-subscribing the listener.
  const autoAcceptRef = useRef(autoAcceptFriends);
  useEffect(() => {
    autoAcceptRef.current = autoAcceptFriends;
  }, [autoAcceptFriends]);

  // OS-notify-on-receive opt-in, read at event time (same pattern as autoAccept):
  // the incoming-status listener stays subscribed across toggles.
  const notifyOnReceiveRef = useRef(notifyOnReceive);
  useEffect(() => {
    notifyOnReceiveRef.current = notifyOnReceive;
  }, [notifyOnReceive]);

  // ---- Receive loop lifecycle (the Ricezione toggle) ----
  // ONE effect owns both start and stop (its cleanup): a separate unmount-only
  // stop effect used to race a `peerReceiverStop()` against this start and, under
  // React StrictMode's dev double-invoke (mount->unmount->mount), could leave the
  // receiver down. When `receiving` is true we start, retrying a few times because
  // at cold boot the peer identity / vault may not be unlocked yet the first time
  // `receiving` reads true - without the retry that first failure left the receiver
  // permanently off until a manual re-toggle (the OFF/Save/ON/Save workaround). The
  // cleanup stops on toggle-off, on flag-off (Hub unmount), and on app teardown.
  useEffect(() => {
    if (!receiving) return;
    let cancelled = false;
    const start = (attemptsLeft: number) => {
      peerReceiverStart().catch((e) => {
        if (cancelled) return;
        if (attemptsLeft > 0) {
          window.setTimeout(() => start(attemptsLeft - 1), 800);
        } else {
          error(t('aeroShare.receive.startError'), String(e));
        }
      });
    };
    start(4);
    return () => {
      cancelled = true;
      peerReceiverStop().catch(() => {
        /* best-effort */
      });
    };
  }, [receiving, error, t]);

  // ---- peer://incoming-offer: prompt or auto-accept ----
  // The `disposed` guard tears the listener down even if `listen()` resolves
  // AFTER the cleanup ran (React StrictMode's dev mount->unmount->mount races the
  // async subscribe). Without it the first listener leaked and a second one
  // registered, so one incoming offer was answered TWICE -> the second
  // peerIncomingRespond hit "no longer pending" -> a FALSE red error toast on an
  // otherwise successful receive.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let disposed = false;
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
      if (disposed) {
        unlisten();
        unlisten = undefined;
      }
    })();
    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [error, info, t]);

  // ---- peer://incoming-status: outcome toasts + session inbox ----
  // Same `disposed` guard as the incoming-offer listener: a leaked duplicate here
  // would double every completed/failed toast.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let disposed = false;
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
          const completedTitle = t('aeroShare.receive.completedTitle');
          const completedDesc = t('aeroShare.receive.completedDesc', {
            name: ev.name,
            sender: senderLabel,
          });
          success(completedTitle, completedDesc);
          // Durable trace (Finding 7 + 8): the toast is ephemeral, so record the
          // receive in the Activity log AND push it to the titlebar notification
          // center, which keeps the file path for an "open folder" action.
          log('DOWNLOAD', completedDesc, 'success', ev.path ?? undefined);
          notify({
            kind: 'receive',
            title: completedTitle,
            body: completedDesc,
            filePath: ev.path,
            ts: ev.at_ms,
          });
          // OS system notification (opt-in): the in-app toast is missed when the
          // window is unfocused, and auto-accepted receives are otherwise SILENT.
          // Routed through the NATIVE plugin (aeroShareNotify) because the JS
          // sendNotification is a silent no-op under WebKitGTK. Best-effort: a
          // failed notification must never break the receive flow.
          if (notifyOnReceiveRef.current) {
            aeroShareNotify(completedTitle, completedDesc).catch(() => {
              /* notification plugin unavailable */
            });
          }
        } else if (ev.state === 'failed') {
          const failedTitle = t('aeroShare.receive.failedTitle');
          const failedBody = `${ev.name}${ev.error ? ` · ${ev.error}` : ''}`;
          error(failedTitle, failedBody);
          log('DOWNLOAD', failedBody, 'error', ev.error ?? undefined);
          notify({ kind: 'error', title: failedTitle, body: failedBody, ts: ev.at_ms });
        }
        // 'declined' is the user's own choice: no toast.
      });
      if (disposed) {
        unlisten();
        unlisten = undefined;
      }
    })();
    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [success, error, t, log, notify]);

  // ---- peer://knock: predefined-code ping (no file) ----
  // Surfaced as a toast + a notification-center entry; a QUESTION knock also
  // opens a small prompt offering the predefined one-tap replies (bounded Q/A).
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let disposed = false;
    (async () => {
      unlisten = await listen<PeerKnockEvent>('peer://knock', async (e) => {
        const ev = e.payload;
        const alias = await resolveFriendAlias(ev.senderAfid);
        const senderLabel = alias || shortAfid(ev.senderAfid);
        const msg = t(knockLabelKey(ev.code));
        // The modal is the most visible surface, so EVERY knock opens it (owner
        // call): a statement shows just the message + Dismiss; a fresh question
        // also shows the one-tap replies. The bell keeps the durable history; no
        // separate toast (the modal already fronts it). A reply (in_reply_to set)
        // carries no replies of its own, so its modal is informational only -
        // which also bounds the exchange (no infinite ping-pong).
        notify({ kind: 'knock', title: senderLabel, body: msg, ts: ev.atMs });
        setIncomingKnock({
          senderAfid: ev.senderAfid,
          senderLabel,
          code: ev.code,
          replies: ev.inReplyTo ? [] : knockReplies(ev.code),
        });
      });
      if (disposed) {
        unlisten();
        unlisten = undefined;
      }
    })();
    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [info, notify, t]);

  // ---- peer://action: structured agent-to-agent message (verb + payload) ----
  // Actions are the extensible generalization of a knock, meant for automation
  // (the action bus), so they are NOT prompted with a modal like a human knock;
  // they are SURFACED durably in the notification center + the activity log so the
  // user can see what arrived while the app is open (D5). A correlated reply and a
  // small payload preview are folded into the body.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let disposed = false;
    (async () => {
      unlisten = await listen<PeerActionEvent>('peer://action', async (e) => {
        const ev = e.payload;
        const alias = await resolveFriendAlias(ev.senderAfid);
        const senderLabel = alias || shortAfid(ev.senderAfid);
        const corr = ev.correlationId ? ` (reply ${ev.correlationId})` : '';
        let preview = '';
        if (ev.payload != null) {
          try {
            const json = JSON.stringify(ev.payload);
            preview = json.length > 120 ? ` ${json.slice(0, 117)}...` : ` ${json}`;
          } catch {
            /* unserializable payload: show the verb alone */
          }
        }
        // The verb is free-form agent data (an extensible catalog), not a fixed
        // translatable string, so it is surfaced verbatim - no i18n key needed.
        const body = `${ev.verb}${corr}${preview}`;
        notify({ kind: 'knock', title: senderLabel, body, ts: ev.atMs });
        log('INFO', `${senderLabel} - ${body}`, 'success');
      });
      if (disposed) {
        unlisten();
        unlisten = undefined;
      }
    })();
    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [notify, log]);

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

  // Answer a question knock with a predefined reply code (in_reply_to = the
  // original code), then dismiss the prompt.
  const replyToKnock = useCallback(
    (replyCode: string) => {
      if (!incomingKnock) return;
      peerSendKnock(incomingKnock.senderAfid, replyCode, incomingKnock.code).catch(() => {
        /* fire-and-forget */
      });
      setIncomingKnock(null);
    },
    [incomingKnock],
  );

  // Mute the sender of the current knock: its future knocks/actions/offers are
  // dropped by the backend gate. Fire-and-forget + dismiss the prompt.
  const muteKnockSender = useCallback(() => {
    if (!incomingKnock) return;
    const { senderAfid, senderLabel } = incomingKnock;
    peerContactMute(senderAfid).catch(() => {
      /* fire-and-forget */
    });
    notify({ kind: 'knock', title: senderLabel, body: t('aeroShare.knock.muted') });
    setIncomingKnock(null);
  }, [incomingKnock, notify, t]);

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

      {incomingKnock && (
        <KnockPrompt
          senderLabel={incomingKnock.senderLabel}
          code={incomingKnock.code}
          replies={incomingKnock.replies}
          onReply={replyToKnock}
          onMute={muteKnockSender}
          onClose={() => setIncomingKnock(null)}
        />
      )}

      {sendTarget && (
        <SendFileDialog
          filePath={sendTarget.filePath}
          onSent={(name, friend) => {
            const sentTitle = t('aeroShare.send.sent');
            const sentDesc = t('aeroShare.send.sentDesc', { name, friend });
            success(sentTitle, sentDesc);
            // Delivery confirmation in the durable surfaces (Finding 7 + 8).
            log('UPLOAD', sentDesc, 'success');
            notify({ kind: 'send', title: sentTitle, body: sentDesc });
          }}
          onError={(msg) => {
            const failedTitle = t('aeroShare.send.failed');
            error(failedTitle, msg);
            log('UPLOAD', msg, 'error');
            notify({ kind: 'error', title: failedTitle, body: msg });
          }}
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
// Knock prompt: a received QUESTION knock with predefined one-tap replies
// (bounded Q/A; no free text). Plain "X: <message>" + a button per reply code.
// ---------------------------------------------------------------------------

function KnockPrompt({
  senderLabel,
  code,
  replies,
  onReply,
  onMute,
  onClose,
}: {
  senderLabel: string;
  code: string;
  replies: string[];
  onReply: (replyCode: string) => void;
  onMute: () => void;
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
    <div className="fixed inset-0 z-[10000] flex items-center justify-center bg-black/50">
      <div
        className="bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-[400px] animate-scale-in"
        role="dialog"
        aria-modal="true"
      >
        <div className="flex items-center gap-2 px-4 py-3 border-b border-gray-100 dark:border-gray-700">
          <Hand size={18} className="text-amber-500" />
          <h2 className="text-sm font-semibold text-gray-900 dark:text-gray-100">{senderLabel}</h2>
        </div>
        <div className="px-4 py-4 text-sm text-gray-700 dark:text-gray-300">
          {t(knockLabelKey(code))}
        </div>
        <div className="flex flex-wrap justify-end gap-2 px-4 py-3 border-t border-gray-100 dark:border-gray-700">
          {/* Mute this sender: drops all its future knocks/actions/offers before
              they ever surface a modal (the per-sender anti-flood lever, #370). */}
          <button
            onClick={onMute}
            className="mr-auto px-3 py-1.5 text-sm rounded-md text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/20"
            title={t('aeroShare.knock.mute')}
          >
            {t('aeroShare.knock.mute')}
          </button>
          <button
            onClick={onClose}
            className="px-3 py-1.5 text-sm rounded-md border border-gray-300 dark:border-gray-600 text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
          >
            {t('aeroShare.knock.dismiss')}
          </button>
          {replies.map((rc) => (
            <button
              key={rc}
              onClick={() => onReply(rc)}
              className="px-3 py-1.5 text-sm rounded-md bg-violet-600 text-white hover:bg-violet-500"
            >
              {t(knockLabelKey(rc))}
            </button>
          ))}
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
  const [progress, setProgress] = useState<{ percent: number; speed: number } | null>(null);
  const lastTickRef = useRef<{ t: number; bytes: number } | null>(null);
  const fileName = basenameOf(filePath);

  // Live send progress: the backend streams `peer://send-status` byte ticks while
  // the (post-accept) blob transfers. Match on the file name (the dialog sends one
  // file at a time) and derive an instantaneous throughput from successive ticks.
  useEffect(() => {
    let un: UnlistenFn | undefined;
    let cancelled = false;
    listen<{ name: string; sent: number; total: number; percent: number; phase: string }>(
      'peer://send-status',
      (e) => {
        const p = e.payload;
        if (p.name !== fileName) return;
        const now = Date.now();
        const last = lastTickRef.current;
        const speed = last && now > last.t ? ((p.sent - last.bytes) * 1000) / (now - last.t) : 0;
        lastTickRef.current = { t: now, bytes: p.sent };
        setProgress({ percent: p.percent, speed });
      },
    ).then((u) => {
      if (cancelled) u();
      else un = u;
    });
    return () => {
      cancelled = true;
      if (un) un();
    };
  }, [fileName]);

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
      lastTickRef.current = null;
      setProgress(null);
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

          {sending && progress && (
            <div className="mb-3">
              <div className="flex items-center justify-between text-[11px] text-gray-500 mb-1">
                <span>{t('aeroShare.send.sending')}</span>
                <span className="tabular-nums">
                  {progress.percent}%
                  {progress.speed > 0 && ` · ${formatBytes(progress.speed)}/s`}
                </span>
              </div>
              <div className="h-2 rounded-full bg-gray-200 dark:bg-gray-700 overflow-hidden">
                <div
                  className="h-full bg-violet-500 transition-[width] duration-150"
                  style={{ width: `${progress.percent}%` }}
                />
              </div>
            </div>
          )}

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

  // Open the on-disk inbox root (~/AeroShare Inbox) in the OS file manager.
  const openInboxFolder = useCallback(() => {
    aeroShareInboxRoot()
      .then(openInFileManager)
      .catch(() => {
        /* best-effort: nothing actionable if the file manager refuses */
      });
  }, []);

  // Reveal a specific received file in the OS file manager (selects it on
  // Windows/macOS, and on Linux via the file manager's D-Bus ShowItems, with a
  // fallback to opening the parent folder when that service is unavailable).
  const revealFile = useCallback((path: string) => {
    openInFileManager(path).catch(() => {
      /* best-effort */
    });
  }, []);

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
          <div className="flex items-center gap-1">
            <button
              onClick={openInboxFolder}
              className="flex items-center gap-1.5 px-2 py-1 rounded text-xs text-violet-600 dark:text-violet-400 hover:bg-violet-50 dark:hover:bg-violet-500/10"
              title={t('aeroShare.inbox.openFolder')}
            >
              <FolderOpen size={14} />
              {t('aeroShare.inbox.openFolder')}
            </button>
            <button
              onClick={onClose}
              className="p-1.5 rounded hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-600 dark:text-gray-400"
            >
              <X size={14} />
            </button>
          </div>
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
                  className="group flex items-center gap-2 px-2 py-2 rounded-md hover:bg-gray-50 dark:hover:bg-gray-700/40"
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
                  {it.path && (
                    <button
                      onClick={() => revealFile(it.path as string)}
                      className="shrink-0 p-1.5 rounded text-gray-400 hover:text-violet-500 hover:bg-violet-50 dark:hover:bg-violet-500/10 opacity-0 group-hover:opacity-100 transition-opacity"
                      title={t('aeroShare.inbox.revealFile')}
                    >
                      <FolderOpen size={14} />
                    </button>
                  )}
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
