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
