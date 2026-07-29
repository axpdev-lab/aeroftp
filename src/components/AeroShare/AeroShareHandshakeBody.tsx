// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroShare handshake body (P2P-GUI-DESIGN.md §6; Phase 1 task 8).
 *
 * The reusable RECEIVE/SHARE handshake form, extracted from AeroShareDialog so
 * the modal (+friend quick-add) and the ConnectionScreen peer-ADD path render
 * ONE body, no duplication. This component owns the mode tabs, all form state,
 * the submit logic (peer_drive_add / peer_share_start / upsertFriendProfile)
 * and the live share-status listener. It deliberately carries NO modal chrome
 * (overlay, header, close button, Escape handling) so each host frames it:
 *  - AeroShareDialog wraps it in the modal chrome.
 *  - ConnectionScreen renders it inline in the form area (peer-add).
 *
 * Two modes:
 *  - RECEIVE: show MY AeroFTP-ID (copy) so a friend can share with me, then
 *    paste their share link + their AFID + pick a local folder -> peer_drive_add
 *    -> save the friend profile (task 11) -> offer "Connect now".
 *  - SHARE: pick a folder + a recipient (saved friend or pasted AFID) ->
 *    peer_share_start (spinner; peer://share-status events) -> show the ONE
 *    share link (copy) to send back.
 *
 * The link is an opaque string to the frontend; QR pairing is P4 polish, so a
 * copy button is enough for Phase 1 (no QR dependency added without approval).
 */

import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { pickFile } from '../../utils/pickPath';
import { safePickerStartDir } from '../../utils/safePickerDir';
import {
  Copy, Check, Loader2, AlertTriangle, FolderOpen, Download, Upload,
  IdCard, Link2, Plug, Image, X, Save, Users,
} from 'lucide-react';
import { useTranslation } from '../../i18n';
import { IconPickerDialog } from '../IconPickerDialog';
import { AeroShareContacts } from './AeroShareContacts';
import type { ServerProfile } from '../../types';
import {
  peerIdentityGet, peerFriendsList, peerShareStart, peerDriveAdd,
  extractTicketFromLink, shortAfid, upsertFriendProfile, looksLikeShareLink,
  type PeerFriend, type PeerShareStarted, type PeerDriveAdded,
} from '../../utils/aeroShare';

export type AeroShareMode = 'receive' | 'share' | 'contacts';

export interface AeroShareHandshakeBodyProps {
  /** Which tab to open on. */
  initialMode?: AeroShareMode;
  /** Pre-fill the friend's AFID/alias (launched from a friend card). */
  prefillAfid?: string;
  prefillAlias?: string;
  /** Default destination folder for the RECEIVE flow. */
  defaultLocalPath?: string;
  /** SHARE flow: pre-fill the folder to share (folder-context entry point). */
  prefillShareFolder?: string;
  /** Re-pull friends/drives after a successful handshake. */
  onFriendSaved?: () => void;
  /** "Connect now" after receiving a drive: open the dual panel on it. */
  onConnectFriend?: (profile: ServerProfile) => void;
  /** Dismiss the host (modal close, or ConnectionScreen back). */
  onClose: () => void;
  /** Visual density: 'modal' (compact, default) or 'page' (the wider form-tab
   *  styling used when embedded in ConnectionScreen, matching other providers). */
  variant?: 'modal' | 'page';
  /** Receive-only: hide the Share tab and lock the body to the RECEIVE flow.
   *  Used by the My Servers add (a My Servers card is a received drive, settled
   *  model), so the page-add cannot publish. Sharing stays reachable from the
   *  modal (titlebar / File menu) until it gets its own surface. The modal
   *  leaves this false and keeps both tabs. */
  receiveOnly?: boolean;
  /** Edit an existing received connection (My Servers peer-card edit). When set,
   *  the body renders a receive-only EDIT form: no mode tabs, no "Your
   *  AeroFTP-ID" card, no share-link/import step. The friend's AeroFTP-ID is
   *  fixed (shown read-only with Copy) and Save persists the updated friend
   *  name / icon / local folder via onSaveEdit, instead of importing a new
   *  drive. This is what keeps the edit form identical to the add form. */
  editConnection?: {
    afid: string;
    alias: string;
    localFolder: string;
    customIconUrl?: string;
  };
  /** Persist a peer-card edit. Required when editConnection is set; the host
   *  (ConnectionScreen) owns the saved-profile write. */
  onSaveEdit?: (values: { alias: string; localFolder: string; customIconUrl?: string }) => void | Promise<void>;
}

interface PeerShareEvent {
  dir: string;
  namespace?: string | null;
  state: string;
  detail?: string | null;
  at_ms: number;
}

type Phase = 'form' | 'working' | 'done' | 'error';

async function copyText(text: string): Promise<void> {
  try {
    await invoke('copy_to_clipboard', { text });
  } catch {
    try { await navigator.clipboard.writeText(text); } catch { /* ignore */ }
  }
}

function CopyField({ value, label, page = false }: { value: string; label: string; page?: boolean }) {
  const t = useTranslation();
  const [copied, setCopied] = useState(false);
  return (
    <div>
      <label className={labelClsFor(page)}>{label}</label>
      <div className="flex items-center gap-2">
        <input
          type="text"
          readOnly
          value={value}
          className={`flex-1 ${page ? 'text-sm px-4 py-2.5' : 'text-xs px-3 py-2'} bg-gray-50 dark:bg-gray-700 border border-gray-200 dark:border-gray-600 rounded-lg text-gray-900 dark:text-gray-100 font-mono select-all cursor-text truncate`}
          onClick={(e) => (e.target as HTMLInputElement).select()}
        />
        <button
          onClick={() => { void copyText(value); setCopied(true); setTimeout(() => setCopied(false), 2000); }}
          className={`flex items-center gap-1.5 px-3 py-2 text-xs rounded-lg transition-all ${
            copied
              ? 'bg-green-100 dark:bg-green-900/30 text-green-700 dark:text-green-400'
              : 'bg-violet-600 text-white hover:bg-violet-700'
          }`}
        >
          {copied ? <Check size={12} /> : <Copy size={12} />}
          {copied ? t('aeroShare.dialog.copied') : t('aeroShare.dialog.copy')}
        </button>
      </div>
    </div>
  );
}

// Field/label classes scale with the host: compact in the modal, wider in the
// ConnectionScreen form-tab (page) so peer-add matches every other provider form.
const inputClsFor = (page: boolean) =>
  `w-full ${page ? 'text-sm px-4 py-2.5' : 'text-xs px-3 py-2'} bg-gray-50 dark:bg-gray-700 border border-gray-200 dark:border-gray-600 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 dark:placeholder-gray-500`;
const labelClsFor = (page: boolean) =>
  `${page ? 'text-sm' : 'text-xs'} font-medium text-gray-700 dark:text-gray-300 ${page ? 'mb-1.5' : 'mb-1'} block`;

// Hoisted out of the body ON PURPOSE: defining it inline made it a new component
// type on every render, so React remounted the tab buttons. With the body now
// portaled under the frequently re-rendering MyServersPanel, those remounts
// swallowed clicks (the tab needed 2-3 taps to switch). As a stable top-level
// component it just re-renders in place.
function ModeTab({ active, onClick, icon, label }: { active: boolean; onClick: () => void; icon: React.ReactNode; label: string }) {
  return (
    <button
      onClick={onClick}
      className={`flex items-center gap-1.5 pb-2 text-xs font-medium border-b-2 transition-colors ${
        active
          ? 'border-violet-500 text-violet-600 dark:text-violet-400'
          : 'border-transparent text-gray-500 dark:text-gray-400 hover:text-gray-700 dark:hover:text-gray-300'
      }`}
    >
      {icon}{label}
    </button>
  );
}

export function AeroShareHandshakeBody({
  initialMode = 'receive',
  prefillAfid,
  prefillAlias,
  defaultLocalPath,
  onFriendSaved,
  onConnectFriend,
  onClose,
  variant = 'modal',
  receiveOnly = false,
  prefillShareFolder,
  editConnection,
  onSaveEdit,
}: AeroShareHandshakeBodyProps) {
  const t = useTranslation();
  const isPage = variant === 'page';
  // Edit mode (My Servers peer-card edit): a receive-only, link-less form.
  const isEdit = !!editConnection;
  // No mode tabs when editing, or when the host pins the body to receive
  // (My Servers add): only the RECEIVE flow is reachable then.
  const showTabs = !isEdit && !receiveOnly;
  const inputCls = inputClsFor(isPage);
  const labelCls = labelClsFor(isPage);
  const [mode, setMode] = useState<AeroShareMode>(receiveOnly ? 'receive' : initialMode);

  // Optional friend avatar (data URL), saved as the card's customIconUrl.
  // Shown only in the page (form-tab) host, matching the other providers'
  // Server Icon field; the compact modal quick-add stays icon-free. In edit
  // mode it is seeded from the existing card's icon.
  const [customIcon, setCustomIcon] = useState<string | undefined>(editConnection?.customIconUrl);
  const [showIconPicker, setShowIconPicker] = useState(false);

  // My identity (shown in RECEIVE; minted on first open).
  const [myAfid, setMyAfid] = useState<string | null>(null);
  const [friends, setFriends] = useState<PeerFriend[]>([]);

  // RECEIVE form (seeded from editConnection in edit mode; the edit form tab
  // pre-populates the profile, so these initial values are first-render-correct)
  const [link, setLink] = useState('');
  const [issuerAfid, setIssuerAfid] = useState(editConnection?.afid ?? prefillAfid ?? '');
  const [issuerAlias, setIssuerAlias] = useState(editConnection?.alias ?? prefillAlias ?? '');
  const [localFolder, setLocalFolder] = useState(editConnection?.localFolder ?? defaultLocalPath ?? '');
  const [recvPhase, setRecvPhase] = useState<Phase>('form');
  const [recvError, setRecvError] = useState('');
  const [recvResult, setRecvResult] = useState<PeerDriveAdded | null>(null);
  const [recvProfile, setRecvProfile] = useState<ServerProfile | null>(null);

  // SHARE form
  const [recipientMode, setRecipientMode] = useState<'saved' | 'paste'>('paste');
  const [selectedFriend, setSelectedFriend] = useState('');
  const [recipientAfid, setRecipientAfid] = useState(prefillAfid ?? '');
  const [recipientAlias, setRecipientAlias] = useState(prefillAlias ?? '');
  const [shareFolder, setShareFolder] = useState(prefillShareFolder ?? '');
  const [driveName, setDriveName] = useState('');
  const [sharePhase, setSharePhase] = useState<Phase>('form');
  const [shareError, setShareError] = useState('');
  const [shareResult, setShareResult] = useState<PeerShareStarted | null>(null);
  const [liveStatus, setLiveStatus] = useState('');

  // Load my AFID + saved friends on open. autoCreate so the receiver always
  // has an ID to show (handoff §2 step 1). Skipped in edit mode: it shows the
  // friend's AFID (already known) and needs neither my identity nor the roster.
  useEffect(() => {
    if (isEdit) return;
    let cancelled = false;
    peerIdentityGet(true)
      .then((info) => { if (!cancelled) setMyAfid(info.afid); })
      .catch(() => { /* leave null; surfaced inline if needed */ });
    peerFriendsList()
      .then((list) => {
        if (cancelled) return;
        setFriends(list);
        if (list.length > 0 && !prefillAfid) {
          setRecipientMode('saved');
          setSelectedFriend(list[0].afid);
        }
      })
      .catch(() => { /* none yet */ });
    return () => { cancelled = true; };
  }, [prefillAfid, isEdit]);

  const pickFolder = useCallback(async (setter: (p: string) => void, current: string) => {
    try {
      // Sanitize the starting dir so a stale/non-existent path cannot crash the
      // native folder chooser (Fix G).
      const selected = await pickFile({ directory: true, multiple: false, defaultPath: await safePickerStartDir(current) });
      if (typeof selected === 'string') setter(selected);
    } catch { /* user cancelled */ }
  }, []);

  // Server Icon field (page host only), mirroring ConnectionScreen's picker so
  // an AeroShare friend gets the same avatar affordance as every other server.
  const renderIconField = (nameForLetter: string) => {
    const letter = (nameForLetter || '?').trim().charAt(0).toUpperCase() || '?';
    return (
      <div>
        <label className={labelCls}>{t('settings.serverIcon')}</label>
        <div className="flex items-center gap-3">
          <div className={`w-10 h-10 shrink-0 rounded-lg flex items-center justify-center ${customIcon ? 'bg-white dark:bg-gray-600 border border-gray-200 dark:border-gray-500' : 'bg-gradient-to-br from-violet-500 to-violet-600 text-white'}`}>
            {customIcon ? (
              <img src={customIcon} alt="" className="w-6 h-6 rounded object-contain" />
            ) : (
              <span className="font-bold text-sm">{letter}</span>
            )}
          </div>
          <button type="button" onClick={() => setShowIconPicker(true)} className="px-3 py-1.5 text-xs font-medium rounded-lg bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 border border-gray-300 dark:border-gray-600 transition-colors flex items-center gap-1.5">
            <Image size={12} />
            {t('settings.chooseIcon')}
          </button>
          {customIcon && (
            <button type="button" onClick={() => setCustomIcon(undefined)} className="p-1.5 text-xs rounded-lg hover:bg-red-100 dark:hover:bg-red-900/30 text-red-500 transition-colors" title={t('settings.removeIcon')}>
              <X size={14} />
            </button>
          )}
        </div>
      </div>
    );
  };

  // ---- RECEIVE submit -----------------------------------------------------
  const submitReceive = useCallback(async () => {
    setRecvError('');
    // Edit mode: no link, no drive import. Just hand the updated friend name /
    // local folder / icon back to the host to persist on the saved profile.
    if (isEdit) {
      if (!localFolder.trim()) { setRecvError(t('aeroShare.dialog.missingFolder')); return; }
      setRecvPhase('working');
      try {
        await onSaveEdit?.({
          alias: issuerAlias.trim(),
          localFolder: localFolder.trim(),
          customIconUrl: customIcon || undefined,
        });
        onClose();
      } catch (e) {
        setRecvError(String(e));
        setRecvPhase('error');
      }
      return;
    }
    if (!looksLikeShareLink(link)) { setRecvError(t('aeroShare.dialog.missingLink')); return; }
    if (!issuerAfid.trim()) { setRecvError(t('aeroShare.dialog.missingIssuer')); return; }
    if (!localFolder.trim()) { setRecvError(t('aeroShare.dialog.missingFolder')); return; }

    setRecvPhase('working');
    try {
      const added = await peerDriveAdd({
        link: link.trim(),
        issuerAfid: issuerAfid.trim(),
        issuerAlias: issuerAlias.trim() || undefined,
        localFolder: localFolder.trim(),
      });
      // Persist the friend + the receive binding (task 11). The ticket lives
      // in the pasted link; provider_connect needs it to (re)start the sub.
      const ticket = extractTicketFromLink(link) ?? undefined;
      const profile = await upsertFriendProfile({
        afid: issuerAfid.trim(),
        alias: issuerAlias.trim() || undefined,
        namespace: added.namespace,
        ticket,
        localFolder: localFolder.trim(),
        role: 'replicator',
        driveName: added.drive_name,
        customIconUrl: customIcon || undefined,
      });
      setRecvResult(added);
      setRecvProfile(profile);
      setRecvPhase('done');
      onFriendSaved?.();
    } catch (e) {
      setRecvError(String(e));
      setRecvPhase('error');
    }
  }, [isEdit, onSaveEdit, onClose, link, issuerAfid, issuerAlias, localFolder, customIcon, t, onFriendSaved]);

  // ---- SHARE submit -------------------------------------------------------
  const effectiveRecipientAfid = recipientMode === 'saved' ? selectedFriend : recipientAfid.trim();
  const effectiveRecipientAlias = recipientMode === 'saved'
    ? (friends.find((f) => f.afid === selectedFriend)?.alias)
    : (recipientAlias.trim() || undefined);

  const submitShare = useCallback(async () => {
    setShareError('');
    if (!shareFolder.trim()) { setShareError(t('aeroShare.dialog.missingFolder')); return; }
    if (!effectiveRecipientAfid) { setShareError(t('aeroShare.dialog.missingRecipient')); return; }

    setSharePhase('working');
    setLiveStatus(t('aeroShare.status.starting'));
    try {
      const started = await peerShareStart({
        dir: shareFolder.trim(),
        recipientAfid: effectiveRecipientAfid,
        recipientAlias: effectiveRecipientAlias,
        driveName: driveName.trim() || undefined,
      });
      // Save the recipient as a contact card (no received-drive binding: I am
      // the publisher; in Phase 1 I do not browse my own published drive).
      await upsertFriendProfile({
        afid: effectiveRecipientAfid,
        alias: effectiveRecipientAlias,
        driveName: started.drive_name,
        customIconUrl: customIcon || undefined,
      });
      setShareResult(started);
      setSharePhase('done');
      onFriendSaved?.();
    } catch (e) {
      setShareError(String(e));
      setSharePhase('error');
    }
  }, [shareFolder, effectiveRecipientAfid, effectiveRecipientAlias, driveName, customIcon, t, onFriendSaved]);

  // Live share-status while a publish is being prepared.
  useEffect(() => {
    if (sharePhase !== 'working') return;
    let active = true;
    const p = listen<PeerShareEvent>('peer://share-status', (e) => {
      if (!active) return;
      const s = e.payload.state;
      const label =
        s === 'serving' ? t('aeroShare.status.serving')
        : s === 'error' ? (e.payload.detail || t('aeroShare.status.error'))
        : s === 'stopped' ? t('aeroShare.status.stopped')
        : t('aeroShare.status.starting');
      setLiveStatus(label);
    });
    let un: (() => void) | undefined;
    void p.then((u) => { if (active) un = u; else u(); });
    return () => { active = false; un?.(); };
  }, [sharePhase, t]);

  return (
    <>
      {/* Mode tabs: hidden in edit (a My Servers card edits its receive side
          only) and in receive-only add (My Servers cannot publish). */}
      {showTabs && (
      <div className="flex px-4 gap-5 border-b border-gray-200 dark:border-gray-700">
        <ModeTab active={mode === 'receive'} onClick={() => setMode('receive')} icon={<Download size={13} />} label={t('aeroShare.dialog.receiveTab')} />
        <ModeTab active={mode === 'share'} onClick={() => setMode('share')} icon={<Upload size={13} />} label={t('aeroShare.dialog.shareTab')} />
        <ModeTab active={mode === 'contacts'} onClick={() => setMode('contacts')} icon={<Users size={13} />} label={t('aeroShare.dialog.contactsTab')} />
      </div>
      )}

      {/* Content */}
      <div className="px-4 py-4 overflow-y-auto">
        {/* ============================== RECEIVE ============================== */}
        {mode === 'receive' && (
          <div className="space-y-4">
            {/* Identity card: in EDIT the friend's AeroFTP-ID (fixed, read-only +
                Copy); otherwise MY AeroFTP-ID to hand to the friend so they can
                share with me. Same violet card either way, for visual parity. */}
            {isEdit ? (
              <div className="rounded-lg border border-violet-200 dark:border-violet-900/50 bg-violet-50/60 dark:bg-violet-900/10 p-3">
                <div className="flex items-center gap-1.5 mb-1.5 text-xs font-medium text-violet-700 dark:text-violet-300">
                  <IdCard size={13} />
                  {t('aeroShare.dialog.issuerLabel')}
                </div>
                <CopyField value={issuerAfid} label={t('aeroShare.dialog.issuerPlaceholder')} page={isPage} />
              </div>
            ) : (
              <div className="rounded-lg border border-violet-200 dark:border-violet-900/50 bg-violet-50/60 dark:bg-violet-900/10 p-3">
                <div className="flex items-center gap-1.5 mb-1.5 text-xs font-medium text-violet-700 dark:text-violet-300">
                  <IdCard size={13} />
                  {t('aeroShare.dialog.myId')}
                </div>
                {myAfid ? (
                  <CopyField value={myAfid} label={t('aeroShare.dialog.myIdHint')} page={isPage} />
                ) : (
                  <div className="flex items-center gap-2 text-xs text-gray-500 dark:text-gray-400">
                    <Loader2 size={13} className="animate-spin" />
                    {t('aeroShare.dialog.needIdentityFirst')}
                  </div>
                )}
              </div>
            )}

            {recvPhase === 'done' && recvResult ? (
              <div className="space-y-3">
                <div className="flex items-start gap-2 p-3 bg-green-50 dark:bg-green-900/20 border border-green-200 dark:border-green-800 rounded-lg">
                  <Check size={16} className="text-green-600 shrink-0 mt-0.5" />
                  <div className="text-xs">
                    <p className="font-medium text-green-700 dark:text-green-400">{t('aeroShare.dialog.receiveSuccess')}</p>
                    <p className="text-green-600 dark:text-green-500 mt-0.5 break-all">{recvResult.drive_name}</p>
                  </div>
                </div>
                <div className="flex justify-end gap-2">
                  <button onClick={onClose} className="px-3 py-1.5 text-xs text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg">
                    {t('aeroShare.dialog.close')}
                  </button>
                  {recvProfile && onConnectFriend && (
                    <button
                      onClick={() => { onConnectFriend(recvProfile); onClose(); }}
                      className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-violet-600 text-white rounded-lg hover:bg-violet-700"
                    >
                      <Plug size={12} />
                      {t('aeroShare.dialog.connectNow')}
                    </button>
                  )}
                </div>
              </div>
            ) : (
              <>
                {!isEdit && <p className="text-xs text-gray-500 dark:text-gray-400">{t('aeroShare.dialog.receiveStep')}</p>}
                {!isEdit && (
                <div>
                  <label className={labelCls}>{t('aeroShare.dialog.linkLabel')}</label>
                  <input value={link} onChange={(e) => setLink(e.target.value)} placeholder={t('aeroShare.dialog.linkPlaceholder')} className={`${inputCls} font-mono`} />
                </div>
                )}
                {!isEdit && (
                <div>
                  <label className={labelCls}>{t('aeroShare.dialog.issuerLabel')}</label>
                  <input value={issuerAfid} onChange={(e) => setIssuerAfid(e.target.value)} placeholder={t('aeroShare.dialog.issuerPlaceholder')} className={`${inputCls} font-mono`} />
                </div>
                )}
                <div>
                  <label className={labelCls}>{t('aeroShare.dialog.aliasLabel')}</label>
                  <input value={issuerAlias} onChange={(e) => setIssuerAlias(e.target.value)} className={inputCls} />
                </div>
                {isPage && renderIconField(issuerAlias)}
                <div>
                  <label className={labelCls}>{t('aeroShare.dialog.localFolderLabel')}</label>
                  <div className="flex items-center gap-2">
                    <input value={localFolder} onChange={(e) => setLocalFolder(e.target.value)} className={`${inputCls} flex-1`} />
                    <button onClick={() => pickFolder(setLocalFolder, localFolder)} className="flex items-center gap-1.5 px-3 py-2 text-xs bg-gray-100 dark:bg-gray-700 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600">
                      <FolderOpen size={12} />
                      {t('aeroShare.dialog.chooseFolder')}
                    </button>
                  </div>
                </div>

                {recvError && (
                  <div className="flex items-start gap-2 p-2.5 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg text-xs text-red-600 dark:text-red-400">
                    <AlertTriangle size={13} className="shrink-0 mt-0.5" />
                    <span className="break-words">{recvError}</span>
                  </div>
                )}

                {isEdit ? (
                  <div className="flex gap-2 pt-1">
                    <button
                      onClick={onClose}
                      className="px-4 py-2.5 text-sm font-medium rounded-lg bg-gray-100 dark:bg-gray-700 text-gray-700 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors"
                    >
                      {t('common.cancel')}
                    </button>
                    <button
                      onClick={submitReceive}
                      disabled={recvPhase === 'working'}
                      className="flex-1 flex items-center justify-center gap-2 px-4 py-2.5 text-sm bg-violet-600 text-white rounded-lg hover:bg-violet-700 transition-colors disabled:opacity-60"
                    >
                      {recvPhase === 'working' ? <Loader2 size={14} className="animate-spin" /> : <Save size={14} />}
                      {t('common.save')}
                    </button>
                  </div>
                ) : (
                  <button
                    onClick={submitReceive}
                    disabled={recvPhase === 'working'}
                    className="w-full flex items-center justify-center gap-2 px-4 py-2.5 text-sm bg-violet-600 text-white rounded-lg hover:bg-violet-700 transition-colors disabled:opacity-60"
                  >
                    {recvPhase === 'working' ? <Loader2 size={14} className="animate-spin" /> : <Download size={14} />}
                    {recvPhase === 'working' ? t('aeroShare.dialog.receiving') : t('aeroShare.dialog.receiveSubmit')}
                  </button>
                )}
              </>
            )}
          </div>
        )}

        {/* ============================== SHARE ============================== */}
        {mode === 'share' && (
          <div className="space-y-4">
            {sharePhase === 'done' && shareResult ? (
              <div className="space-y-3">
                <div className="flex items-start gap-2 p-3 bg-green-50 dark:bg-green-900/20 border border-green-200 dark:border-green-800 rounded-lg">
                  <Check size={16} className="text-green-600 shrink-0 mt-0.5" />
                  <div className="text-xs">
                    <p className="font-medium text-green-700 dark:text-green-400">{t('aeroShare.dialog.shareReadyTitle')}</p>
                    <p className="text-green-600 dark:text-green-500 mt-0.5">{t('aeroShare.dialog.shareReadyHint')}</p>
                  </div>
                </div>
                <CopyField value={shareResult.link} label={t('aeroShare.dialog.shareLinkLabel')} />
                <div className="flex justify-end">
                  <button onClick={onClose} className="px-3 py-1.5 text-xs text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg">
                    {t('aeroShare.dialog.close')}
                  </button>
                </div>
              </div>
            ) : sharePhase === 'working' ? (
              <div className="flex flex-col items-center justify-center py-10 text-gray-600 dark:text-gray-400">
                <Loader2 size={28} className="animate-spin mb-3 text-violet-500" />
                <p className="text-sm font-medium">{t('aeroShare.dialog.sharing')}</p>
                {liveStatus && <p className="text-xs text-gray-500 mt-1">{liveStatus}</p>}
              </div>
            ) : (
              <>
                <p className="text-xs text-gray-500 dark:text-gray-400">{t('aeroShare.dialog.shareStep')}</p>
                <div>
                  <label className={labelCls}>{t('aeroShare.dialog.shareFolderLabel')}</label>
                  <div className="flex items-center gap-2">
                    <input value={shareFolder} onChange={(e) => setShareFolder(e.target.value)} className={`${inputCls} flex-1`} />
                    <button onClick={() => pickFolder(setShareFolder, shareFolder)} className="flex items-center gap-1.5 px-3 py-2 text-xs bg-gray-100 dark:bg-gray-700 text-gray-700 dark:text-gray-300 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600">
                      <FolderOpen size={12} />
                      {t('aeroShare.dialog.chooseFolder')}
                    </button>
                  </div>
                </div>

                {/* Recipient */}
                <div>
                  <label className={labelCls}>{t('aeroShare.dialog.recipientLabel')}</label>
                  <div className="flex gap-1 mb-2 rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800 p-1 w-fit">
                    <button
                      onClick={() => setRecipientMode('saved')}
                      disabled={friends.length === 0}
                      className={`px-2.5 py-1 rounded-md text-[11px] font-medium ${recipientMode === 'saved' ? 'bg-white dark:bg-gray-700 text-violet-600 dark:text-violet-300 shadow-sm' : 'text-gray-500 dark:text-gray-400'} disabled:opacity-40`}
                    >
                      {t('aeroShare.dialog.recipientSaved')}
                    </button>
                    <button
                      onClick={() => setRecipientMode('paste')}
                      className={`px-2.5 py-1 rounded-md text-[11px] font-medium ${recipientMode === 'paste' ? 'bg-white dark:bg-gray-700 text-violet-600 dark:text-violet-300 shadow-sm' : 'text-gray-500 dark:text-gray-400'}`}
                    >
                      {t('aeroShare.dialog.recipientPaste')}
                    </button>
                  </div>
                  {recipientMode === 'saved' ? (
                    friends.length > 0 ? (
                      <select value={selectedFriend} onChange={(e) => setSelectedFriend(e.target.value)} className={inputCls}>
                        {friends.map((f) => (
                          <option key={f.afid} value={f.afid}>{f.alias} ({shortAfid(f.afid)})</option>
                        ))}
                      </select>
                    ) : (
                      <p className="text-xs text-gray-400 dark:text-gray-500">{t('aeroShare.dialog.noFriendsYet')}</p>
                    )
                  ) : (
                    <div className="space-y-2">
                      <input value={recipientAfid} onChange={(e) => setRecipientAfid(e.target.value)} placeholder={t('aeroShare.dialog.recipientAfidLabel')} className={`${inputCls} font-mono`} />
                      <input value={recipientAlias} onChange={(e) => setRecipientAlias(e.target.value)} placeholder={t('aeroShare.dialog.recipientAliasLabel')} className={inputCls} />
                    </div>
                  )}
                </div>

                {isPage && renderIconField(effectiveRecipientAlias || recipientAfid)}
                <div>
                  <label className={labelCls}>{t('aeroShare.dialog.driveNameLabel')}</label>
                  <input value={driveName} onChange={(e) => setDriveName(e.target.value)} className={inputCls} />
                </div>

                {shareError && (
                  <div className="flex items-start gap-2 p-2.5 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg text-xs text-red-600 dark:text-red-400">
                    <AlertTriangle size={13} className="shrink-0 mt-0.5" />
                    <span className="break-words">{shareError}</span>
                  </div>
                )}

                <button
                  onClick={submitShare}
                  className="w-full flex items-center justify-center gap-2 px-4 py-2.5 text-sm bg-violet-600 text-white rounded-lg hover:bg-violet-700 transition-colors"
                >
                  <Link2 size={14} />
                  {t('aeroShare.dialog.shareSubmit')}
                </button>
              </>
            )}
          </div>
        )}
        {mode === 'contacts' && <AeroShareContacts />}
      </div>

      {isPage && showIconPicker && (
        <IconPickerDialog
          onSelect={(dataUrl) => setCustomIcon(dataUrl)}
          onClose={() => setShowIconPicker(false)}
          currentIcon={customIcon}
        />
      )}
    </>
  );
}

export default AeroShareHandshakeBody;
