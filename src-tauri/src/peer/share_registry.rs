//! AeroShare Phase 1, Share surface slice 2: durable metadata for the folders
//! the active user is sharing (the "Shared by me" panel).
//!
//! The encrypted drive record is only `(namespace, role, content_key)`; it does
//! not remember WHICH local folder was published, under WHAT name, or to WHOM.
//! P1 also does not auto-republish on boot, so without a durable record the
//! "Shared by me" panel would be empty after every restart, defeating its whole
//! point ("so the user does not forget what they share").
//!
//! This sidecar fills that gap: a plaintext JSON beside the publish stores
//! (`<app_config>/aeroshare/<user_id>/shares.json`), keyed by drive namespace,
//! holding the local folder, the drive name, and the recipient AeroFTP-IDs. It
//! is consistent with the existing plaintext `namespace.txt` sidecar and the
//! custody rule that AFIDs/namespaces are PUBLIC material (the secret content
//! key stays DEK-sealed in the vault). The namespace key is stable per folder
//! (engine S8 key continuity), so re-sharing the same folder with a second
//! friend reuses the entry and just appends the new recipient.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tauri::AppHandle;

/// A friend a folder is shared with (public material: AFID + display alias).
#[derive(Clone, Serialize, Deserialize)]
pub struct ShareRecipient {
    pub afid: String,
    pub alias: String,
}

/// Durable record of one shared folder.
#[derive(Clone, Serialize, Deserialize)]
pub struct ShareMeta {
    pub namespace: String,
    pub folder: String,
    pub drive_name: String,
    #[serde(default)]
    pub recipients: Vec<ShareRecipient>,
}

fn registry_path(app: &AppHandle, user_id: i64) -> Result<PathBuf, String> {
    Ok(crate::portable::app_config_dir(app)?
        .join("aeroshare")
        .join(user_id.to_string())
        .join("shares.json"))
}

fn load_map(app: &AppHandle, user_id: i64) -> Result<BTreeMap<String, ShareMeta>, String> {
    let path = registry_path(app, user_id)?;
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|e| {
            format!(
                "cannot parse AeroShare share registry {}: {e}",
                path.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(format!(
            "cannot read AeroShare share registry {}: {e}",
            path.display()
        )),
    }
}

fn store_map(
    app: &AppHandle,
    user_id: i64,
    map: &BTreeMap<String, ShareMeta>,
) -> Result<(), String> {
    let path = registry_path(app, user_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "cannot create AeroShare config dir {}: {e}",
                parent.display()
            )
        })?;
    }
    let raw = serde_json::to_string_pretty(map)
        .map_err(|e| format!("cannot serialize AeroShare share registry: {e}"))?;
    std::fs::write(&path, raw).map_err(|e| {
        format!(
            "cannot write AeroShare share registry {}: {e}",
            path.display()
        )
    })
}

/// Upsert a share: set `folder` + `drive_name` for `namespace` and add the
/// recipient (by AFID) if not already present, refreshing the alias otherwise.
/// Called after a successful publish + grant in `peer_share_start`.
pub fn record_share(
    app: &AppHandle,
    user_id: i64,
    namespace: &str,
    folder: &str,
    drive_name: &str,
    recipient_afid: &str,
    recipient_alias: &str,
) -> Result<(), String> {
    let mut map = load_map(app, user_id)?;
    let entry = map
        .entry(namespace.to_string())
        .or_insert_with(|| ShareMeta {
            namespace: namespace.to_string(),
            folder: folder.to_string(),
            drive_name: drive_name.to_string(),
            recipients: Vec::new(),
        });
    entry.folder = folder.to_string();
    entry.drive_name = drive_name.to_string();
    match entry
        .recipients
        .iter_mut()
        .find(|r| r.afid == recipient_afid)
    {
        Some(r) => r.alias = recipient_alias.to_string(),
        None => entry.recipients.push(ShareRecipient {
            afid: recipient_afid.to_string(),
            alias: recipient_alias.to_string(),
        }),
    }
    store_map(app, user_id, &map)
}

/// All shares of the active user, sorted by drive name for a stable panel order.
pub fn list_shares(app: &AppHandle, user_id: i64) -> Result<Vec<ShareMeta>, String> {
    let mut shares: Vec<ShareMeta> = load_map(app, user_id)?.into_values().collect();
    shares.sort_by(|a, b| {
        a.drive_name
            .to_lowercase()
            .cmp(&b.drive_name.to_lowercase())
            .then_with(|| a.namespace.cmp(&b.namespace))
    });
    Ok(shares)
}

/// One share's metadata by namespace (the Re-serve path needs its folder/name).
pub fn get_share(
    app: &AppHandle,
    user_id: i64,
    namespace: &str,
) -> Result<Option<ShareMeta>, String> {
    Ok(load_map(app, user_id)?.remove(namespace))
}

/// Forget a share from the panel (Remove). No-op if absent. The drive's content
/// key + publish store are left intact so re-sharing the same folder later keeps
/// key continuity; this only drops the panel entry.
pub fn remove_share(app: &AppHandle, user_id: i64, namespace: &str) -> Result<(), String> {
    let mut map = load_map(app, user_id)?;
    if map.remove(namespace).is_some() {
        store_map(app, user_id, &map)?;
    }
    Ok(())
}

/// Move a share entry to a new namespace, preserving folder/name/recipients.
/// Used by Re-serve in the degraded case where the publish store was lost and
/// the engine minted a fresh namespace (old recipient tokens stop converging,
/// the WI-4e re-key semantics); keeps the panel pointing at the live drive.
pub fn rekey_share(
    app: &AppHandle,
    user_id: i64,
    old_namespace: &str,
    new_namespace: &str,
) -> Result<(), String> {
    if old_namespace == new_namespace {
        return Ok(());
    }
    let mut map = load_map(app, user_id)?;
    if let Some(mut meta) = map.remove(old_namespace) {
        meta.namespace = new_namespace.to_string();
        map.insert(new_namespace.to_string(), meta);
        store_map(app, user_id, &map)?;
    }
    Ok(())
}
