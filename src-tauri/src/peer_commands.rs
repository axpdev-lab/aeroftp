//! AeroShare Phase 1, task 4: the Tauri command surface for the handshake
//! flow (design doc §6) and the Friends/Drives inventory.
//!
//! Five commands, mirroring the proven `aeroftp peer` CLI verbs over the
//! same engine (`crate::peer`) + vault (`crate::user_partitions::gui_peer_*`)
//! seams, with the long-lived tasks owned by `crate::peer::runtime::PeerRuntime`:
//! - `peer_identity_get`: my AeroFTP-ID (receiver step 1 "show my AFID/QR");
//!   mints + custodies one on first use when `auto_create`.
//! - `peer_share_start` (sharer step 2): publish the folder as an encrypted
//!   drive (or reuse the live publish of that folder), seal a capability to
//!   the recipient, save them as a friend, return the ONE share link.
//! - `peer_drive_add` (receiver step 3): paste/scan the link, import the
//!   capability, custody the key, save the friend, start replication into
//!   the chosen local folder.
//! - `peer_friends_list` / `peer_drives_list`: the saved contacts and drive
//!   inventory (with live sync/serve state) for My Servers and the dialogs.
//!
//! Progress/state flows separately over the `peer://sync-status` and
//! `peer://share-status` events emitted by the runtime; commands return only
//! the final data. All identity/key bytes stay in `Zeroizing` buffers.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::peer::runtime::PeerRuntime;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::{AppHandle, State};

/// The ONE share link of the handshake (design §6): ticket + sealed
/// capability in a single QR-friendly string. Both parts are URL-safe by
/// construction (the ticket is base32, the capability payload base64url).
pub const SHARE_LINK_PREFIX: &str = "aeroftp-share://v1/";
/// Scheme of the bare capability token produced by `peer grant` /
/// [`crate::peer::grant_capability`].
const DRIVE_TOKEN_SCHEME: &str = "aeroftp-drive://";

/// Compose the share link from the drive ticket and the sealed capability
/// token (`aeroftp-drive://...`).
pub fn build_share_link(ticket: &str, token: &str) -> Result<String, String> {
    let cap = token
        .strip_prefix(DRIVE_TOKEN_SCHEME)
        .ok_or_else(|| "capability token has an unexpected scheme".to_string())?;
    let ticket = ticket.trim();
    if ticket.is_empty() || ticket.contains('/') {
        return Err("drive ticket is empty or malformed".to_string());
    }
    Ok(format!("{SHARE_LINK_PREFIX}{ticket}/{cap}"))
}

/// Split a pasted share link back into `(ticket, aeroftp-drive:// token)`.
/// Accepts surrounding whitespace; fails closed on anything else.
pub fn parse_share_link(link: &str) -> Result<(String, String), String> {
    let body = link.trim().strip_prefix(SHARE_LINK_PREFIX).ok_or_else(|| {
        format!("not an AeroShare link (expected it to start with {SHARE_LINK_PREFIX})")
    })?;
    let (ticket, cap) = body
        .split_once('/')
        .ok_or_else(|| "AeroShare link is missing its capability part".to_string())?;
    if ticket.is_empty() || cap.is_empty() {
        return Err("AeroShare link has an empty ticket or capability part".to_string());
    }
    Ok((ticket.to_string(), format!("{DRIVE_TOKEN_SCHEME}{cap}")))
}

/// Short display form of an AeroFTP-ID (used as a default alias).
fn short_afid(afid: &str) -> String {
    if afid.len() > 13 {
        format!("{}…{}", &afid[..8], &afid[afid.len() - 4..])
    } else {
        afid.to_string()
    }
}

// ---------------------------------------------------------------------------
// peer_identity_get
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct PeerIdentityInfo {
    /// My shareable AeroFTP-ID, or `None` when no identity exists yet and
    /// `auto_create` was false.
    pub afid: Option<String>,
    /// True when this call minted (and custodied) a fresh identity.
    pub created: bool,
}

/// The active user's P2P identity. With `auto_create` (the handshake dialog
/// path) a missing identity is minted and custodied in the partition vault.
#[tauri::command]
pub async fn peer_identity_get(
    app: AppHandle,
    auto_create: Option<bool>,
) -> Result<PeerIdentityInfo, String> {
    match crate::user_partitions::gui_peer_identity_get_or_create(
        &app,
        auto_create.unwrap_or(false),
    )? {
        Some((_uid, afid, created)) => Ok(PeerIdentityInfo {
            afid: Some(afid),
            created,
        }),
        None => Ok(PeerIdentityInfo {
            afid: None,
            created: false,
        }),
    }
}

// ---------------------------------------------------------------------------
// peer_friends_list / peer_drives_list
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct PeerFriend {
    pub afid: String,
    pub alias: String,
}

/// The saved contacts ("friends") of the active user partition.
#[tauri::command]
pub async fn peer_friends_list(app: AppHandle) -> Result<Vec<PeerFriend>, String> {
    Ok(crate::user_partitions::gui_peer_contact_list(&app)?
        .into_iter()
        .map(|(afid, alias)| PeerFriend { afid, alias })
        .collect())
}

/// Add (or rename) a contact in the active user's partition (UPSERT keyed by
/// AFID, so this also edits an existing contact's alias). The AFID is validated
/// as a well-formed AeroFTP-ID; the alias falls back to a short AFID when blank.
#[tauri::command]
pub async fn peer_contact_add(
    app: AppHandle,
    contact_id: String,
    alias: String,
) -> Result<(), String> {
    let afid = contact_id.trim();
    if afid.is_empty() {
        return Err("an AeroFTP-ID is required".to_string());
    }
    aeroftp_peer_l0::IdentityPublic::from_aeroftp_id(afid)
        .map_err(|_| "that is not a valid AeroFTP-ID".to_string())?;
    let alias = {
        let a = alias.trim();
        if a.is_empty() {
            short_afid(afid)
        } else {
            a.to_string()
        }
    };
    crate::user_partitions::gui_peer_contact_add(&app, afid, &alias)
}

/// Remove a contact from the active user's partition (no-op if absent).
#[tauri::command]
pub async fn peer_contact_remove(app: AppHandle, contact_id: String) -> Result<(), String> {
    crate::user_partitions::gui_peer_contact_remove(&app, contact_id.trim())
}

#[derive(Serialize)]
pub struct PeerDriveInfo {
    pub namespace: String,
    /// `publisher` or `replicator` (my relationship to the drive).
    pub role: String,
    /// A live replication task is converging this drive right now.
    pub syncing: bool,
    /// A live publish task is serving this drive right now.
    pub serving: bool,
    /// The runtime's authoritative state (`starting|syncing|live|serving|
    /// error|stopped|standby`). The FE trusts THIS on a re-pull so the dot
    /// stays `live` (green) across a remount instead of reverting to `syncing`
    /// derived from the boolean (F3). Falls back to the booleans for a drive
    /// whose task never started this session.
    pub state: String,
}

/// The drive inventory of the active user partition, annotated with the
/// runtime's live task state.
#[tauri::command]
pub async fn peer_drives_list(
    app: AppHandle,
    peer_runtime: State<'_, PeerRuntime>,
) -> Result<Vec<PeerDriveInfo>, String> {
    let drives = crate::user_partitions::gui_peer_drive_list(&app)?;
    let syncing = peer_runtime.live_sub_namespaces().await;
    let serving = peer_runtime.live_share_namespaces().await;
    let states = peer_runtime.states_snapshot();
    Ok(drives
        .into_iter()
        .map(|(namespace, role)| {
            let is_syncing = syncing.contains(&namespace);
            let is_serving = serving.contains(&namespace);
            let state = states.get(&namespace).cloned().unwrap_or_else(|| {
                if is_serving {
                    "serving".to_string()
                } else if is_syncing {
                    "syncing".to_string()
                } else {
                    "stopped".to_string()
                }
            });
            PeerDriveInfo {
                namespace,
                role,
                syncing: is_syncing,
                serving: is_serving,
                state,
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// peer_share_start (sharer side of the handshake)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerShareStartParams {
    /// Absolute local folder to share.
    pub dir: String,
    /// The receiver's AeroFTP-ID (pasted or picked from saved friends).
    pub recipient_afid: String,
    /// Optional alias to save the receiver under (defaults to a short AFID).
    pub recipient_alias: Option<String>,
    /// Optional human label for the drive (defaults to the folder name).
    pub drive_name: Option<String>,
}

#[derive(Serialize)]
pub struct PeerShareStarted {
    pub namespace: String,
    pub ticket: String,
    pub token: String,
    /// The ONE string to send back to the receiver (also QR-encoded by the
    /// dialog): `aeroftp-share://v1/<ticket>/<capability>`.
    pub link: String,
    pub drive_name: String,
}

/// Publish `dir` as an encrypted drive (or reuse the live publish of that
/// folder), seal a read capability to the recipient, save them as a friend,
/// and return the share link. The serving task keeps running in the
/// background (D-GUI-1) and is observable on `peer://share-status`.
#[tauri::command]
pub async fn peer_share_start(
    app: AppHandle,
    peer_runtime: State<'_, PeerRuntime>,
    params: PeerShareStartParams,
) -> Result<PeerShareStarted, String> {
    let dir = Path::new(params.dir.trim());
    if !dir.is_absolute() {
        return Err("the shared folder must be an absolute path".to_string());
    }
    if !dir.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    let recipient = crate::peer::validate_aeroftp_id(params.recipient_afid.trim())
        .map_err(|e| format!("invalid recipient AeroFTP-ID: {e}"))?;
    let drive_name = params
        .drive_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| dir.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "drive".to_string());

    // My identity (minted on first share, CLI publish precedent).
    let (user_id, _afid, _created) =
        crate::user_partitions::gui_peer_identity_get_or_create(&app, true)?
            .ok_or_else(|| "could not initialize the P2P identity".to_string())?;
    let (_uid, identity_secret) = crate::user_partitions::gui_peer_identity_load_secret(&app)?
        .ok_or_else(|| "P2P identity missing right after creation".to_string())?;

    // Publish (or reuse the live/persisted publish of this folder); the
    // runtime resolves key continuity and custodies a fresh key itself, and
    // hands back the key the served drive is ACTUALLY encrypted with.
    let (namespace, ticket, content_key) = peer_runtime
        .start_share(&app, user_id, dir, &drive_name, &identity_secret)
        .await?;

    let issued_at = chrono::Utc::now().timestamp();
    let token = crate::peer::grant_capability(
        &identity_secret,
        &recipient,
        &namespace,
        &content_key,
        &drive_name,
        1,
        Vec::new(),
        issued_at,
    )
    .map_err(|e| format!("grant failed: {e}"))?;
    let link = build_share_link(&ticket, &token)?;

    // Both ends of the handshake save each other (design §6).
    let alias = params
        .recipient_alias
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| short_afid(&recipient));
    crate::user_partitions::gui_peer_contact_add(&app, &recipient, &alias)?;

    // Durable record for the "Shared by me" panel (the drive record itself
    // remembers neither folder nor recipients). Best-effort beside the publish
    // store; a failure here must not void an otherwise-successful share link.
    if let Err(e) = crate::peer::share_registry::record_share(
        &app,
        user_id,
        &namespace,
        &dir.to_string_lossy(),
        &drive_name,
        &recipient,
        &alias,
    ) {
        tracing::warn!("AeroShare: could not record share metadata for {namespace}: {e}");
    }

    Ok(PeerShareStarted {
        namespace,
        ticket,
        token,
        link,
        drive_name,
    })
}

// ---------------------------------------------------------------------------
// peer_drive_add (receiver side of the handshake)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerDriveAddParams {
    /// The pasted/scanned `aeroftp-share://v1/...` link.
    pub link: String,
    /// The sharer's AeroFTP-ID (the capability is verified against it).
    pub issuer_afid: String,
    /// Optional alias to save the sharer under.
    pub issuer_alias: Option<String>,
    /// Absolute local folder to replicate the drive into.
    pub local_folder: String,
}

#[derive(Serialize)]
pub struct PeerDriveAdded {
    pub namespace: String,
    pub drive_name: String,
    pub version: u64,
}

/// Import a share link: open the sealed capability (verifying the issuer),
/// custody the drive key, save the friend, and start replicating into
/// `local_folder`. Sync progress flows on `peer://sync-status`.
#[tauri::command]
pub async fn peer_drive_add(
    app: AppHandle,
    peer_runtime: State<'_, PeerRuntime>,
    params: PeerDriveAddParams,
) -> Result<PeerDriveAdded, String> {
    let issuer = crate::peer::validate_aeroftp_id(params.issuer_afid.trim())
        .map_err(|e| format!("invalid friend AeroFTP-ID: {e}"))?;
    let (ticket, token) = parse_share_link(&params.link)?;
    let local_folder = Path::new(params.local_folder.trim());
    if !local_folder.is_absolute() {
        return Err("the destination folder must be an absolute path".to_string());
    }

    // My identity must exist: the sharer sealed the capability to the AFID I
    // showed in step 1 of the handshake.
    let (_uid, identity_secret) = crate::user_partitions::gui_peer_identity_load_secret(&app)?
        .ok_or_else(|| {
            "no P2P identity in this partition: open \"Add friend\" to create one and share \
             your AeroFTP-ID first"
                .to_string()
        })?;

    let imported = crate::peer::import_capability(&identity_secret, &issuer, &token)
        .map_err(|e| format!("import failed: {e}"))?;

    // The link's ticket must dial the drive the capability unlocks.
    let ticket_namespace = crate::peer::namespace_from_ticket(&ticket)
        .map_err(|e| format!("invalid ticket in the link: {e}"))?;
    if ticket_namespace != imported.namespace_id {
        return Err("the link's ticket and capability describe different drives".to_string());
    }

    crate::user_partitions::gui_peer_drive_store(
        &app,
        &imported.namespace_id,
        "replicator",
        &imported.content_key,
    )?;
    let alias = params
        .issuer_alias
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| short_afid(&issuer));
    crate::user_partitions::gui_peer_contact_add(&app, &issuer, &alias)?;

    peer_runtime
        .ensure_sub(&app, &imported.namespace_id, &ticket, local_folder)
        .await?;

    Ok(PeerDriveAdded {
        namespace: imported.namespace_id.clone(),
        drive_name: imported.drive_name.clone(),
        version: imported.version,
    })
}

// ---------------------------------------------------------------------------
// peer_shares_list / peer_share_stop / peer_share_resume / peer_share_remove
// (Share surface slice 2: the "Shared by me" panel)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct PeerShareRecipient {
    pub afid: String,
    pub alias: String,
}

#[derive(Serialize)]
pub struct PeerShareInfo {
    pub namespace: String,
    /// Absolute local folder being shared.
    pub folder: String,
    pub drive_name: String,
    pub recipients: Vec<PeerShareRecipient>,
    /// A live publish task is serving this drive right now (vs idle: persisted
    /// but not serving, e.g. after a restart or an explicit Stop).
    pub serving: bool,
}

/// The folders the active user is sharing, from the durable share registry,
/// annotated with the runtime's live serving state. Empty when there is no P2P
/// identity yet (you cannot share without one).
#[tauri::command]
pub async fn peer_shares_list(
    app: AppHandle,
    peer_runtime: State<'_, PeerRuntime>,
) -> Result<Vec<PeerShareInfo>, String> {
    let user_id = match crate::user_partitions::gui_peer_identity_get_or_create(&app, false)? {
        Some((uid, _afid, _created)) => uid,
        None => return Ok(Vec::new()),
    };
    let serving = peer_runtime.live_share_namespaces().await;
    Ok(crate::peer::share_registry::list_shares(&app, user_id)?
        .into_iter()
        .map(|meta| PeerShareInfo {
            serving: serving.contains(&meta.namespace),
            recipients: meta
                .recipients
                .into_iter()
                .map(|r| PeerShareRecipient {
                    afid: r.afid,
                    alias: r.alias,
                })
                .collect(),
            namespace: meta.namespace,
            folder: meta.folder,
            drive_name: meta.drive_name,
        })
        .collect())
}

/// Stop serving a shared drive (it stays in the panel as idle, re-servable).
#[tauri::command]
pub async fn peer_share_stop(
    peer_runtime: State<'_, PeerRuntime>,
    namespace: String,
) -> Result<(), String> {
    peer_runtime.stop_share(&namespace).await;
    Ok(())
}

/// Re-serve an idle shared folder (after a restart or an explicit Stop). Reuses
/// the published drive's key + namespace via the publish-store continuity, so
/// the recipients' existing share links keep working; no new grant is issued.
#[tauri::command]
pub async fn peer_share_resume(
    app: AppHandle,
    peer_runtime: State<'_, PeerRuntime>,
    namespace: String,
) -> Result<(), String> {
    let (user_id, _afid, _created) =
        crate::user_partitions::gui_peer_identity_get_or_create(&app, true)?
            .ok_or_else(|| "could not initialize the P2P identity".to_string())?;
    let (_uid, identity_secret) = crate::user_partitions::gui_peer_identity_load_secret(&app)?
        .ok_or_else(|| "P2P identity missing right after creation".to_string())?;
    let meta = crate::peer::share_registry::get_share(&app, user_id, &namespace)?
        .ok_or_else(|| format!("no share metadata for drive {namespace}"))?;
    let dir = Path::new(meta.folder.trim());
    if !dir.is_dir() {
        return Err(format!(
            "the shared folder {} no longer exists",
            meta.folder
        ));
    }
    let (served_namespace, _ticket, _key) = peer_runtime
        .start_share(&app, user_id, dir, &meta.drive_name, &identity_secret)
        .await?;
    // Key continuity normally re-serves under the SAME namespace; if the publish
    // store was lost the engine mints a fresh one, so move the panel entry to it.
    crate::peer::share_registry::rekey_share(&app, user_id, &namespace, &served_namespace)?;
    Ok(())
}

/// Forget a share from the panel (Remove): stop serving and drop its registry
/// entry. The drive key + publish store stay intact (re-sharing the same folder
/// later keeps key continuity); this only removes the panel entry.
#[tauri::command]
pub async fn peer_share_remove(
    app: AppHandle,
    peer_runtime: State<'_, PeerRuntime>,
    namespace: String,
) -> Result<(), String> {
    peer_runtime.stop_share(&namespace).await;
    if let Some((user_id, _afid, _created)) =
        crate::user_partitions::gui_peer_identity_get_or_create(&app, false)?
    {
        crate::peer::share_registry::remove_share(&app, user_id, &namespace)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// "Send file to user" one-shot: peer_send_file + the receive-side commands
// (peer_receiver_start/stop/status, peer_incoming_respond).
// ---------------------------------------------------------------------------

/// Default inbox root: `~/AeroShare Inbox`. Received files land in a per-sender
/// subfolder beneath it. Fixed for the first cut (a Setting later).
fn default_inbox_root() -> Result<std::path::PathBuf, String> {
    dirs::home_dir()
        .map(|h| h.join("AeroShare Inbox"))
        .ok_or_else(|| "could not resolve the home directory for the AeroShare inbox".to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerSendFileParams {
    /// The recipient's AeroFTP-ID.
    pub recipient_afid: String,
    /// Absolute path of the local file to send.
    pub file_path: String,
}

/// Send a single file to a friend (one-shot, E2EE, no persistent drive). Mints
/// the active user's identity on first use (the sender must have one so the
/// recipient can authenticate them and dial-by-AFID works). Resolves once the
/// recipient ACKs a verified receipt; a recipient decline is returned as an error.
#[tauri::command]
pub async fn peer_send_file(app: AppHandle, params: PeerSendFileParams) -> Result<(), String> {
    let recipient = crate::peer::validate_aeroftp_id(params.recipient_afid.trim())
        .map_err(|e| format!("invalid recipient AeroFTP-ID: {e}"))?;
    let file_path = params.file_path.trim();
    if !Path::new(file_path).is_file() {
        return Err(format!("{file_path} is not a file"));
    }

    crate::user_partitions::gui_peer_identity_get_or_create(&app, true)?
        .ok_or_else(|| "could not initialize the P2P identity".to_string())?;
    let (_uid, identity_secret) = crate::user_partitions::gui_peer_identity_load_secret(&app)?
        .ok_or_else(|| "P2P identity missing right after creation".to_string())?;

    crate::peer::send_file_oneshot(
        &recipient,
        &identity_secret,
        file_path,
        crate::peer::runtime::relay_urls_from_env(),
    )
    .await
    .map_err(|e| format!("send failed: {e}"))
}

/// Start the standing receive loop (the Ricezione toggle = ON). Mints the
/// identity if needed (the receive endpoint is seeded from it so friends can dial
/// by AFID) and listens until [`peer_receiver_stop`]. Idempotent.
#[tauri::command]
pub async fn peer_receiver_start(
    app: AppHandle,
    peer_runtime: State<'_, PeerRuntime>,
) -> Result<(), String> {
    crate::user_partitions::gui_peer_identity_get_or_create(&app, true)?
        .ok_or_else(|| "could not initialize the P2P identity".to_string())?;
    let (_uid, identity_secret) = crate::user_partitions::gui_peer_identity_load_secret(&app)?
        .ok_or_else(|| "P2P identity missing right after creation".to_string())?;
    let inbox_root = default_inbox_root()?;
    peer_runtime
        .start_receiver(&app, &identity_secret, inbox_root)
        .await
}

/// Stop the receive loop (toggle = OFF).
#[tauri::command]
pub async fn peer_receiver_stop(
    app: AppHandle,
    peer_runtime: State<'_, PeerRuntime>,
) -> Result<(), String> {
    peer_runtime.stop_receiver(&app).await;
    Ok(())
}

/// Whether the receive loop is currently listening.
#[tauri::command]
pub async fn peer_receiver_status(peer_runtime: State<'_, PeerRuntime>) -> Result<bool, String> {
    Ok(peer_runtime.is_receiving().await)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerIncomingRespondParams {
    /// The `transferId` from the `peer://incoming-offer` event.
    pub transfer_id: String,
    /// Accept (write to the inbox) or decline.
    pub accept: bool,
    /// Optional friendly per-sender subfolder name (the friend's alias); falls
    /// back to a short AFID when absent.
    pub sender_label: Option<String>,
}

/// Answer a pending incoming offer (Accept/Decline). The FE applies the
/// auto-accept-from-known-friends opt-in by calling this with `accept = true`
/// immediately for a known sender; otherwise it shows the prompt first.
#[tauri::command]
pub async fn peer_incoming_respond(
    peer_runtime: State<'_, PeerRuntime>,
    params: PeerIncomingRespondParams,
) -> Result<(), String> {
    peer_runtime
        .respond_to_offer(&params.transfer_id, params.accept, params.sender_label)
        .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerPresenceParams {
    /// AeroFTP-IDs to probe (the friends shown in the Send dialog).
    pub afids: Vec<String>,
}

/// Probe which friends are online/receiving right now (presence). Returns one
/// bool per input AFID, in order. Best-effort: needs the active user's identity
/// (minted on first use) to seed the probing endpoint.
#[tauri::command]
pub async fn peer_friends_presence(
    app: AppHandle,
    params: PeerPresenceParams,
) -> Result<Vec<bool>, String> {
    if params.afids.is_empty() {
        return Ok(Vec::new());
    }
    crate::user_partitions::gui_peer_identity_get_or_create(&app, true)?
        .ok_or_else(|| "could not initialize the P2P identity".to_string())?;
    let (_uid, identity_secret) = crate::user_partitions::gui_peer_identity_load_secret(&app)?
        .ok_or_else(|| "P2P identity missing right after creation".to_string())?;
    crate::peer::probe_presence(
        &identity_secret,
        &params.afids,
        crate::peer::runtime::relay_urls_from_env(),
    )
    .await
    .map_err(|e| format!("presence probe failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_link_round_trips() {
        let ticket = "docaaa7uzbzx6lcccbtaqki6uynkb6cocjabkzkmpkzcz3ifyu5fgza";
        let token = "aeroftp-drive://q83vEjRWshESNFZ4kBI0VniQ";
        let link = build_share_link(ticket, token).expect("build");
        assert_eq!(
            link,
            format!("aeroftp-share://v1/{ticket}/q83vEjRWshESNFZ4kBI0VniQ")
        );
        let (t, c) = parse_share_link(&link).expect("parse");
        assert_eq!(t, ticket);
        assert_eq!(c, token);
        // Whitespace from copy/paste is tolerated.
        let (t2, c2) = parse_share_link(&format!("  {link}\n")).expect("parse trimmed");
        assert_eq!(t2, ticket);
        assert_eq!(c2, token);
    }

    #[test]
    fn share_link_fails_closed() {
        assert!(build_share_link("ticket", "not-a-drive-token").is_err());
        assert!(build_share_link("", "aeroftp-drive://abc").is_err());
        assert!(build_share_link("tick/et", "aeroftp-drive://abc").is_err());
        assert!(parse_share_link("https://example.com/x").is_err());
        assert!(parse_share_link("aeroftp-share://v1/onlyticket").is_err());
        assert!(parse_share_link("aeroftp-share://v1//cap").is_err());
        assert!(parse_share_link("aeroftp-share://v1/ticket/").is_err());
    }

    #[test]
    fn short_afid_is_compact() {
        assert_eq!(short_afid("AFID1abc"), "AFID1abc");
        let long = "AFID1Y6pmnpUPJNqmDhjN7pEuGc4xyCmzWnSWF6T9Gg6heCDUE6j86K804ZR64BEz0N";
        let s = short_afid(long);
        assert!(s.starts_with("AFID1Y6p"));
        assert!(s.len() < 20);
    }
}
