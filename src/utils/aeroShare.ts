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

// ---------------------------------------------------------------------------
// Command wrappers (Tauri v2: JS arg names are camelCase)
// ---------------------------------------------------------------------------

/** Receiver step 1: the active partition's AeroFTP-ID (minted on first use
 *  when `autoCreate`). */
export const peerIdentityGet = (autoCreate: boolean): Promise<PeerIdentityInfo> =>
  invoke<PeerIdentityInfo>('peer_identity_get', { autoCreate });

export const peerFriendsList = (): Promise<PeerFriend[]> =>
  invoke<PeerFriend[]>('peer_friends_list');

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
}

/** Ask the (always-mounted) MyServersPanel to open the AeroShare dialog.
 *  No-op surface for callers: if AeroShare is off the listener ignores it. */
export const openAeroShareDialog = (detail: AeroShareOpenDetail = { mode: 'receive' }): void => {
  window.dispatchEvent(new CustomEvent<AeroShareOpenDetail>(AERO_SHARE_OPEN_EVENT, { detail }));
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
  return next;
};
