// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroShare (P2P "peer" channel) frontend surface.
 *
 * Thin typed wrappers over the five `peer_*` Tauri commands shipped in
 * `src-tauri/src/peer_commands.rs`, plus the friend <-> saved-profile mapping
 * that makes a friend appear in My Servers as a `protocol: "peer"` profile
 * (design doc P2P-GUI-DESIGN.md; Phase 1 tasks 6-11).
 *
 * Persistence model (task 11): a friend IS a saved server profile. The local
 * binding the engine does not track (`ns -> { friend_afid, local_folder,
 * role }`) rides in the profile's free-form `options` (peerNamespace /
 * peerTicket / peerLocalFolder / peerRole). No separate backend table for
 * Phase 1. The connect path (App.buildProviderParams) forwards those four
 * straight to provider_connect.
 */

import { invoke } from '@tauri-apps/api/core';
import type { ProviderOptions, ServerProfile } from '../types';
import { loadSavedServerProfiles, storeSavedServerProfiles } from './serverProfileStore';
import { secureGetWithFallback, secureStoreAndClean } from './secureStorage';

// ---------------------------------------------------------------------------
// Backend command result shapes (snake_case keys, repo convention)
// ---------------------------------------------------------------------------

export interface PeerIdentityInfo {
  afid: string | null;
  created: boolean;
}

export interface PeerFriend {
  afid: string;
  alias: string;
}

export interface PeerDriveInfo {
  namespace: string;
  /** "publisher" | "replicator" */
  role: string;
  /** A live replication task is converging this drive right now. */
  syncing: boolean;
  /** A live publish task is serving this drive right now. */
  serving: boolean;
  /** Runtime's authoritative state (starting|syncing|live|serving|error|
   *  stopped|standby). Trusted by usePeerDriveStates on a re-pull so the dot
   *  stays green across a remount instead of reverting to syncing (F3). */
  state?: string;
}

export interface PeerShareStarted {
  namespace: string;
  ticket: string;
  token: string;
  /** The ONE string to send back to the receiver. */
  link: string;
  drive_name: string;
}

export interface PeerDriveAdded {
  namespace: string;
  drive_name: string;
  version: number;
}

export interface PeerShareRecipient {
  afid: string;
  alias: string;
}

/** A folder the active user is sharing (the "Shared by me" panel). */
export interface PeerShareInfo {
  namespace: string;
  /** Absolute local folder being shared. */
  folder: string;
  drive_name: string;
  recipients: PeerShareRecipient[];
  /** A live publish task is serving this drive right now (vs idle/persisted). */
  serving: boolean;
}

// ---------------------------------------------------------------------------
// Command wrappers (Tauri v2: JS arg names are camelCase)
// ---------------------------------------------------------------------------

/** Receiver step 1: the active partition's AeroFTP-ID (minted on first use
 *  when `autoCreate`). */
export const peerIdentityGet = (autoCreate: boolean): Promise<PeerIdentityInfo> =>
  invoke<PeerIdentityInfo>('peer_identity_get', { autoCreate });

export const peerFriendsList = (): Promise<PeerFriend[]> =>
  invoke<PeerFriend[]>('peer_friends_list');

/** Add (or rename) a saved contact in the active partition's address book
 *  (UPSERT keyed by AFID, so it also edits an existing contact's alias). The
 *  backend validates the AFID and defaults a blank alias to a short AFID. */
export const peerContactAdd = (contactId: string, alias: string): Promise<void> =>
  invoke('peer_contact_add', { contactId, alias });

/** Remove a saved contact from the active partition's address book (no-op if
 *  absent). */
export const peerContactRemove = (contactId: string): Promise<void> =>
  invoke('peer_contact_remove', { contactId });

/** Payload of the `peer://knock` event (an incoming predefined-code ping). */
export interface PeerKnockEvent {
  senderAfid: string;
  code: string;
  inReplyTo: string | null;
  atMs: number;
}

/** Payload of the `peer://action` event (an incoming structured agent-to-agent
 *  message: a verb + an optional small JSON payload + an optional correlation id
 *  tying a reply back to a request). The extensible generalization of a knock. */
export interface PeerActionEvent {
  senderAfid: string;
  verb: string;
  payload: unknown | null;
  correlationId: string | null;
  atMs: number;
}

/** Send a knock (predefined-code ping, no file) to a friend. `inReplyTo`, when
 *  set, is the code of the knock being answered (bounded predefined Q/A). */
export const peerSendKnock = (
  recipientAfid: string,
  code: string,
  inReplyTo?: string,
): Promise<void> =>
  invoke('peer_send_knock', { params: { recipientAfid, code, inReplyTo: inReplyTo ?? null } });

/** The absolute AeroShare inbox root (`~/AeroShare Inbox`), created if absent.
 *  Pair with `open_in_file_manager` to open it in the OS file manager. */
export const aeroShareInboxRoot = (): Promise<string> => invoke<string>('aeroshare_inbox_root');

/** Open a folder (or reveal a file) in the OS file manager. Reuses the app-wide
 *  `open_in_file_manager` command. Used to open the AeroShare inbox / a received
 *  file's containing folder. */
export const openInFileManager = (path: string): Promise<void> =>
  invoke('open_in_file_manager', { path });

/** Fire a NATIVE OS notification through the Rust notification plugin. Used
 *  instead of the plugin's JS `sendNotification`, which builds a web
 *  `window.Notification` that WebKitGTK silently drops (no OS notification on
 *  Linux). The caller owns the opt-in gating and passes localized text. */
export const aeroShareNotify = (title: string, body: string): Promise<void> =>
  invoke('aeroshare_notify', { title, body });

export const peerDrivesList = (): Promise<PeerDriveInfo[]> =>
  invoke<PeerDriveInfo[]>('peer_drives_list');

export interface PeerShareStartParams {
  dir: string;
  recipientAfid: string;
  recipientAlias?: string;
  driveName?: string;
}

/** Sharer step 2: publish (or reuse the live publish of) `dir`, seal a
 *  capability to the recipient, and return the share link. Long call (first
 *  publish encrypts the folder); progress arrives on `peer://share-status`. */
export const peerShareStart = (params: PeerShareStartParams): Promise<PeerShareStarted> =>
  invoke<PeerShareStarted>('peer_share_start', { params });

export interface PeerDriveAddParams {
  link: string;
  issuerAfid: string;
  issuerAlias?: string;
  localFolder: string;
}

/** Receiver step 3: import the share link (verified against `issuerAfid`),
 *  custody the key, and start replication into `localFolder`. Sync progress
 *  flows on `peer://sync-status`. */
export const peerDriveAdd = (params: PeerDriveAddParams): Promise<PeerDriveAdded> =>
  invoke<PeerDriveAdded>('peer_drive_add', { params });

// --- Share surface slice 2: the "Shared by me" panel ---

/** The folders the active user is sharing, with live serving state. */
export const peerSharesList = (): Promise<PeerShareInfo[]> =>
  invoke<PeerShareInfo[]>('peer_shares_list');

/** Stop serving a shared drive (stays in the panel as idle, re-servable). */
export const peerShareStop = (namespace: string): Promise<void> =>
  invoke('peer_share_stop', { namespace });

/** Re-serve an idle shared folder (reuses the drive key; no new grant). */
export const peerShareResume = (namespace: string): Promise<void> =>
  invoke('peer_share_resume', { namespace });

/** Forget a share from the panel (stops serving + drops the registry entry). */
export const peerShareRemove = (namespace: string): Promise<void> =>
  invoke('peer_share_remove', { namespace });

// ---------------------------------------------------------------------------
// "Send file to user" one-shot (AirDrop)
// ---------------------------------------------------------------------------

export interface PeerSendFileParams {
  recipientAfid: string;
  filePath: string;
}

/** Send a single file to a friend (one-shot, E2EE, no persistent drive).
 *  Resolves once the recipient ACKs a verified receipt; rejects on an explicit
 *  recipient decline or any transport error. */
export const peerSendFile = (params: PeerSendFileParams): Promise<void> =>
  invoke('peer_send_file', { params });

/** Start the standing receive loop (the Ricezione toggle = ON). Idempotent. */
export const peerReceiverStart = (): Promise<void> => invoke('peer_receiver_start');

/** Stop the receive loop (toggle = OFF). */
export const peerReceiverStop = (): Promise<void> => invoke('peer_receiver_stop');

/** Whether the receive loop is currently listening. */
export const peerReceiverStatus = (): Promise<boolean> =>
  invoke<boolean>('peer_receiver_status');

export interface PeerIncomingRespondParams {
  transferId: string;
  accept: boolean;
  /** Friendly per-sender subfolder name (the friend alias); falls back to a
   *  short AFID when omitted. */
  senderLabel?: string;
}

/** Answer a pending incoming offer (Accept/Decline). */
export const peerIncomingRespond = (params: PeerIncomingRespondParams): Promise<void> =>
  invoke('peer_incoming_respond', { params });

/** Probe which friends are online/receiving right now. Returns one bool per
 *  input AFID, in order. */
export const peerFriendsPresence = (afids: string[]): Promise<boolean[]> =>
  invoke<boolean[]>('peer_friends_presence', { params: { afids } });

// ---------------------------------------------------------------------------
// v4.1.0 security follow-ups (#370): anti-flood gate + discovery opt-out
// ---------------------------------------------------------------------------

/** Mute a sender AFID: its inbound knocks/actions/offers are dropped by the
 *  backend before they ever surface a modal or notification. Idempotent. The
 *  AFID need not be a saved contact. */
export const peerContactMute = (contactId: string): Promise<void> =>
  invoke('peer_contact_mute', { contactId });

/** Unmute a sender AFID (no-op if it was not muted). */
export const peerContactUnmute = (contactId: string): Promise<void> =>
  invoke('peer_contact_unmute', { contactId });

/** The active partition's muted AFIDs. */
export const peerMutesList = (): Promise<string[]> => invoke<string[]>('peer_mutes_list');

/** Discovery backend for the receive endpoint:
 *  - `both`: n0 DNS + Mainline DHT (default, most reachable);
 *  - `dht`: Mainline DHT only;
 *  - `n0`: n0 DNS only;
 *  - `none`: publish to neither, so the long-term AFID is NOT enumerable on the
 *    public DHT while receiving (privacy opt-out). Tradeoff: you are reachable
 *    only by a peer who already holds your full address (e.g. via a ticket). */
export type PeerDiscoveryMode = 'both' | 'dht' | 'n0' | 'none';

/** The active partition's AeroShare preferences (anti-flood gate + discovery). */
export interface PeerSettings {
  /** Accept inbound knock/action/offer only from saved contacts. */
  friendsOnly: boolean;
  discoveryMode: PeerDiscoveryMode;
  /** Max inbound signals per sender per minute (0 = no limit). */
  rateLimitPerMin: number;
}

/** Read the active partition's AeroShare settings (defaults when unset). */
export const peerSettingsGet = (): Promise<PeerSettings> =>
  invoke<PeerSettings>('peer_settings_get');

/** Persist the active partition's AeroShare settings. A discovery-mode change
 *  while receiving rebinds the receive endpoint so it takes effect at once. */
export const peerSettingsSet = (settings: PeerSettings): Promise<void> =>
  invoke('peer_settings_set', { settings });

/** Rotate the active partition's P2P identity, minting a fresh AFID. DESTRUCTIVE:
 *  the old AFID and every share link / served drive that encoded it become
 *  unreachable, so the new AFID must be re-shared with friends. Returns the new
 *  AeroFTP-ID. Confirm with the user before calling. */
export const peerIdentityRotate = (): Promise<string> =>
  invoke<string>('peer_identity_rotate');

/** Payload of the `peer://incoming-offer` event (an incoming send awaiting
 *  the user's Accept/Decline). */
export interface PeerIncomingOfferEvent {
  transfer_id: string;
  sender_afid: string;
  name: string;
  size: number;
  at_ms: number;
}

/** Payload of the `peer://incoming-status` event (outcome of an incoming
 *  transfer): state is `completed` | `declined` | `failed`. */
export interface PeerIncomingStatusEvent {
  state: 'completed' | 'declined' | 'failed';
  sender_afid: string;
  name: string;
  path: string | null;
  error: string | null;
  at_ms: number;
}

// --- Global "open the Send-file dialog" event (mirrors AERO_SHARE_OPEN_EVENT) ---

export const AERO_SHARE_SEND_EVENT = 'aeroftp-open-aeroshare-send';

export interface AeroShareSendDetail {
  /** Absolute path of the local file to send. */
  filePath: string;
}

/** Ask the (always-mounted) AeroShare hub to open the Send-file dialog for a
 *  file. No-op if AeroShare is off (the listener is only mounted when on). */
export const openAeroShareSend = (detail: AeroShareSendDetail): void => {
  window.dispatchEvent(new CustomEvent<AeroShareSendDetail>(AERO_SHARE_SEND_EVENT, { detail }));
};

/** Global "open the received-files Inbox" event. */
export const AERO_SHARE_INBOX_EVENT = 'aeroftp-open-aeroshare-inbox';

export const openAeroShareInbox = (): void => {
  window.dispatchEvent(new CustomEvent(AERO_SHARE_INBOX_EVENT));
};

/** Last path component of an absolute path (handles both `/` and `\`). */
export const basenameOf = (p: string): string => {
  const trimmed = p.replace(/[/\\]+$/, '');
  const idx = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'));
  return idx >= 0 ? trimmed.slice(idx + 1) : trimmed;
};

/** Human-readable byte size for offer prompts/toasts (e.g. "2.3 MB"). */
export const formatBytes = (bytes: number): string => {
  if (!Number.isFinite(bytes) || bytes < 0) return '';
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = bytes / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
};

// ---------------------------------------------------------------------------
// Share-link helpers (LOCKED format: aeroftp-share://v1/<ticket>/<cap>)
// ---------------------------------------------------------------------------

export const SHARE_LINK_PREFIX = 'aeroftp-share://v1/';

/** True when `link` looks like an AeroShare link (cheap, for input hints). */
export const looksLikeShareLink = (link: string): boolean =>
  link.trim().startsWith(SHARE_LINK_PREFIX);

/**
 * Pull the drive ticket out of a share link. provider_connect needs the
 * ticket to (re)establish the replication sub, but `peer_drive_add` returns
 * only the namespace; the ticket lives in the link the receiver pasted. The
 * link format is LOCKED in `peer_commands.rs`, so this split is safe.
 * Returns null when the string is not a well-formed share link.
 */
export const extractTicketFromLink = (link: string): string | null => {
  const trimmed = link.trim();
  if (!trimmed.startsWith(SHARE_LINK_PREFIX)) return null;
  const rest = trimmed.slice(SHARE_LINK_PREFIX.length);
  const slash = rest.indexOf('/');
  if (slash <= 0 || slash >= rest.length - 1) return null;
  return rest.slice(0, slash);
};

/** Short, human-scannable AeroFTP-ID for labels: `AFID1Y6p…UE6j`. */
export const shortAfid = (afid: string): string =>
  afid.length > 13 ? `${afid.slice(0, 8)}…${afid.slice(-4)}` : afid;

/** Stable read-only-error marker the backend emits (`providers/peer.rs`). */
export const READ_ONLY_ERROR_PREFIX = 'Read-only endpoint:';

// ---------------------------------------------------------------------------
// Global "open the AeroShare dialog" event.
//
// The dialog lives in MyServersPanel (always mounted via display:none, so it
// keeps its connect/refresh callbacks). Every other entry point - the Discover
// tile, the titlebar +friend icon and File-menu item, onboarding - is decoupled
// from that mount: it just dispatches this event, and MyServersPanel listens
// and opens the dialog. Keeps a single dialog instance without prop-drilling
// through App.tsx / IntroHub.
// ---------------------------------------------------------------------------

export const AERO_SHARE_OPEN_EVENT = 'aeroftp-open-aeroshare';

export interface AeroShareOpenDetail {
  mode: 'receive' | 'share';
  prefillAfid?: string;
  prefillAlias?: string;
  /** SHARE flow: pre-fill the folder to share (folder-context "Share via
   *  AeroShare" entry point). Implies opening on the share tab. */
  prefillShareFolder?: string;
}

/** Ask the (always-mounted) MyServersPanel to open the AeroShare dialog.
 *  The listener is always registered (AeroShare is always-on at launch). */
export const openAeroShareDialog = (detail: AeroShareOpenDetail = { mode: 'receive' }): void => {
  window.dispatchEvent(new CustomEvent<AeroShareOpenDetail>(AERO_SHARE_OPEN_EVENT, { detail }));
};

// ---------------------------------------------------------------------------
// Auto-activation (always-on at launch). The Discover tile and the titlebar
// +friend icon are visible before the user opts in; performing a real action
// (adding a friend or sharing a folder) flips `aeroShareEnabled` on so every
// richer surface (peer filter, friend cards, notification bell) lights up. The
// manual Settings toggle stays as an override. The receiver (the standing P2P
// listener) is NEVER auto-started: on the first activation we fire
// AERO_SHARE_ACTIVATED_EVENT so a one-time prompt can offer opt-in / opt-out.
// ---------------------------------------------------------------------------

const SETTINGS_VAULT_KEY = 'app_settings';
const SETTINGS_LOCAL_KEY = 'aeroftp_settings';

/** Fired ONCE, on the false->true transition of `aeroShareEnabled`. */
export const AERO_SHARE_ACTIVATED_EVENT = 'aeroftp-aeroshare-activated';

/** Merge a patch into `aeroftp_settings` (vault + localStorage write-through)
 *  and broadcast it on `aeroftp-settings-changed` so the read-only settings
 *  hooks (useAeroShareEnabled, useAeroShareReceiveSettings) update instantly. */
export const patchAeroShareSettings = async (
  patch: Record<string, unknown>,
): Promise<void> => {
  let current: Record<string, unknown> = {};
  try {
    const parsed = await secureGetWithFallback<Record<string, unknown>>(
      SETTINGS_VAULT_KEY,
      SETTINGS_LOCAL_KEY,
    );
    if (parsed) current = parsed;
  } catch {
    /* fall back to empty; we still persist the patch */
  }
  await secureStoreAndClean(SETTINGS_VAULT_KEY, SETTINGS_LOCAL_KEY, { ...current, ...patch });
  window.dispatchEvent(new CustomEvent('aeroftp-settings-changed', { detail: patch }));
};

/** Ensure AeroShare is enabled. Idempotent: a no-op when already on. On the
 *  first activation it persists the flag and fires AERO_SHARE_ACTIVATED_EVENT
 *  (the receiver opt-in/opt-out prompt). Call after a friend is added or a
 *  folder is shared. */
export const ensureAeroShareActivated = async (): Promise<void> => {
  try {
    const parsed = await secureGetWithFallback<Record<string, unknown>>(
      SETTINGS_VAULT_KEY,
      SETTINGS_LOCAL_KEY,
    );
    if (parsed?.aeroShareEnabled === true) return; // already active
  } catch {
    /* treat as not-active and enable below */
  }
  await patchAeroShareSettings({ aeroShareEnabled: true });
  window.dispatchEvent(new CustomEvent(AERO_SHARE_ACTIVATED_EVENT));
};

// ---------------------------------------------------------------------------
// Friend <-> saved-profile mapping (task 11 persistence)
// ---------------------------------------------------------------------------

/** Deterministic profile id keyed by AFID so receiving a drive from a friend
 *  and later re-sharing both converge on ONE friend card. */
export const peerProfileId = (afid: string): string => `peer_${afid}`;

/** A saved profile is an AeroShare friend. */
export const isFriendProfile = (p: { protocol?: string }): boolean => p.protocol === 'peer';

/** The friend has a received drive bound to it, so clicking the card can
 *  connect straight to the replica (task 9). Without all three, clicking
 *  opens the handshake dialog instead. */
export const friendCanConnect = (p: { options?: ProviderOptions }): boolean =>
  !!(p.options?.peerNamespace && p.options?.peerTicket && p.options?.peerLocalFolder);

export interface FriendBinding {
  afid: string;
  alias?: string;
  /** Received-drive binding (omit for a contact-only friend). */
  namespace?: string;
  ticket?: string;
  localFolder?: string;
  role?: string;
  driveName?: string;
  /** Custom avatar/icon (data URL) for the friend card. */
  customIconUrl?: string;
}

/**
 * Create or update the saved profile for a friend, merging any new drive
 * binding onto the existing card. Returns the persisted profile. Written by
 * the handshake dialog on a successful receive/share (task 11).
 */
export const upsertFriendProfile = async (b: FriendBinding): Promise<ServerProfile> => {
  const profiles = await loadSavedServerProfiles();
  const id = peerProfileId(b.afid);
  const existing = profiles.find((p) => p.id === id);

  const options: ProviderOptions = {
    ...(existing?.options ?? {}),
    ...(b.namespace ? { peerNamespace: b.namespace } : {}),
    ...(b.ticket ? { peerTicket: b.ticket } : {}),
    ...(b.localFolder ? { peerLocalFolder: b.localFolder } : {}),
    ...(b.role ? { peerRole: b.role } : {}),
    ...(b.driveName ? { peerDriveName: b.driveName } : {}),
  };

  const alias = (b.alias && b.alias.trim()) || existing?.username || shortAfid(b.afid);
  const next: ServerProfile = {
    ...(existing ?? {}),
    id,
    name: existing?.name?.trim() || alias,
    host: b.afid,
    port: 0,
    username: alias,
    protocol: 'peer',
    customIconUrl: b.customIconUrl ?? existing?.customIconUrl,
    options,
  };

  const others = profiles.filter((p) => p.id !== id);
  await storeSavedServerProfiles([...others, next]);
  // Saving a friend (received-folder or share flow) auto-activates AeroShare.
  await ensureAeroShareActivated();
  return next;
};
