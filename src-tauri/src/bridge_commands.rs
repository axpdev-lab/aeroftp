// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Generic GUI bridge commands for the 12 expansion sources.
//!
//! The three legacy importers (rclone / WinSCP / FileZilla) keep their
//! own dedicated `*_config` Tauri commands. The 12 newer sources are
//! dispatched generically here, mirroring the CLI's
//! `cmd_import_bridge` / `cmd_export_bridge` design so the GUI and CLI
//! never diverge. Per-source protocol filtering, the export file
//! format and the secret policy all come from `bridge_shared` (single
//! source of truth, shared with the CLI).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

use crate::bridge_shared::{
    bridge_export_format, bridge_import_filter, bridge_secret_policy,
    bridge_supported_protocols,
};
use crate::credential_store::CredentialStore;
use crate::{
    aws_credentials_import, cyberduck_import, dreamweaver_import, duplicacy_import,
    kopia_import, lftp_import, mc_import, mobaxterm_import, putty_import, restic_import,
    s3cmd_import, ssh_config_import,
};

fn known(source: &str) -> Result<(), String> {
    if bridge_supported_protocols(source).is_empty() {
        return Err(format!("unknown bridge source: {source}"));
    }
    Ok(())
}

/// Default config path for a generic bridge source (mirrors the CLI's
/// `default_*_config_path` dispatch). `Ok(None)` means "no conventional
/// path on this OS, ask the user to browse".
#[tauri::command]
pub async fn detect_bridge_config(source: String) -> Result<Option<String>, String> {
    known(&source)?;
    let p = match source.as_str() {
        "aws" => aws_credentials_import::default_aws_credentials_config_path(),
        "ssh" => ssh_config_import::default_ssh_config_path(),
        "mc" => mc_import::default_mc_config_path(),
        "cyberduck" => cyberduck_import::default_cyberduck_config_path(),
        "s3cmd" => s3cmd_import::default_s3cmd_config_path(),
        "lftp" => lftp_import::default_lftp_config_path(),
        "putty" => putty_import::default_putty_config_path(),
        "mobaxterm" => mobaxterm_import::default_mobaxterm_config_path(),
        "dreamweaver" => dreamweaver_import::default_dreamweaver_config_path(),
        "kopia" => kopia_import::default_kopia_config_path(),
        "duplicacy" => duplicacy_import::default_duplicacy_config_path(),
        "restic" => restic_import::default_restic_config_path(),
        other => return Err(format!("unknown bridge source: {other}")),
    };
    Ok(p.map(|p| p.display().to_string()))
}

/// UI metadata for a generic bridge source: protocol filter, file
/// picker hints and the secret-recovery policy class.
#[tauri::command]
pub async fn bridge_source_meta(source: String) -> Result<Value, String> {
    known(&source)?;
    let (filter_name, exts) = bridge_import_filter(&source);
    let (export_ext, export_label) =
        bridge_export_format(&source).unwrap_or(("txt", ""));
    Ok(json!({
        "source": source,
        "supportedProtocols": bridge_supported_protocols(&source),
        "importFilterName": filter_name,
        "importExtensions": exts,
        "exportExt": export_ext,
        "exportLabel": export_label,
        "secretPolicy": bridge_secret_policy(&source),
    }))
}

/// Dispatch the typed `import_<src>` and collapse the result to the
/// shared camelCase JSON shape (servers / skipped / sourcePath /
/// totalRemotes), identical to the CLI's `cmd_import_bridge`.
fn to_v<T: serde::Serialize>(r: Result<T, String>) -> Result<Value, String> {
    r.and_then(|r| serde_json::to_value(r).map_err(|e| e.to_string()))
}

fn dispatch_import(source: &str, path: &Path) -> Result<Value, String> {
    match source {
        "aws" => to_v(aws_credentials_import::import_aws_credentials(path)),
        "ssh" => to_v(ssh_config_import::import_ssh_config(path)),
        "mc" => to_v(mc_import::import_mc(path)),
        "cyberduck" => to_v(cyberduck_import::import_cyberduck(path)),
        "s3cmd" => to_v(s3cmd_import::import_s3cmd(path)),
        "lftp" => to_v(lftp_import::import_lftp(path)),
        "putty" => to_v(putty_import::import_putty(path)),
        "mobaxterm" => to_v(mobaxterm_import::import_mobaxterm(path)),
        "dreamweaver" => to_v(dreamweaver_import::import_dreamweaver(path)),
        "kopia" => to_v(kopia_import::import_kopia(path)),
        "duplicacy" => to_v(duplicacy_import::import_duplicacy(path)),
        "restic" => to_v(restic_import::import_restic(path)),
        other => Err(format!("unknown import source: {other}")),
    }
}

/// Import profiles from a third-party config file. Validates the path
/// (traversal reject + canonicalize + regular-file + 10 MB cap),
/// upgrades any recovered secret into the AES-256-GCM vault and returns
/// credential-redacted profiles, exactly like `import_rclone_config`.
#[tauri::command]
pub async fn import_bridge_config(
    source: String,
    file_path: String,
) -> Result<Value, String> {
    known(&source)?;

    let path = Path::new(&file_path);
    if path.to_string_lossy().contains("..") {
        return Err("Invalid path: directory traversal not allowed".to_string());
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| "File not found or inaccessible".to_string())?;
    let metadata = std::fs::metadata(&canonical)
        .map_err(|_| "Cannot read file metadata".to_string())?;
    if !metadata.is_file() {
        return Err("Not a regular file".to_string());
    }
    if metadata.len() > 10 * 1024 * 1024 {
        return Err("File too large (max 10 MB)".to_string());
    }

    let value = dispatch_import(&source, &canonical)?;
    let servers = value
        .get("servers")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();

    // Upgrade recovered secrets into the vault (server_<id> key, same as
    // the connect path and the legacy importers).
    let mut cred_errors: Vec<String> = Vec::new();
    match CredentialStore::from_cache() {
        Some(store) => {
            for s in &servers {
                let id = s.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let cred = s.get("credential").and_then(|v| v.as_str()).unwrap_or("");
                if !id.is_empty() && !cred.is_empty() {
                    if let Err(e) = store.store(&format!("server_{}", id), cred) {
                        cred_errors.push(format!("{id}: {e}"));
                    }
                }
            }
        }
        None => {
            let n = servers
                .iter()
                .filter(|s| {
                    s.get("credential")
                        .and_then(|v| v.as_str())
                        .map(|c| !c.is_empty())
                        .unwrap_or(false)
                })
                .count();
            if n > 0 {
                cred_errors.push(format!("Vault not ready, {n} credentials not stored"));
            }
        }
    }
    if !cred_errors.is_empty() {
        log::warn!("{} import credential issues: {:?}", source, cred_errors);
    }

    // Redact credentials before crossing back to the renderer.
    let redacted: Vec<Value> = servers
        .iter()
        .map(|s| {
            let has_cred = s
                .get("credential")
                .map(|c| {
                    !c.is_null()
                        && c.as_str().map(|x| !x.is_empty()).unwrap_or(true)
                })
                .unwrap_or(false);
            json!({
                "id": s.get("id").cloned().unwrap_or(Value::Null),
                "name": s.get("name").cloned().unwrap_or(Value::Null),
                "host": s.get("host").cloned().unwrap_or(Value::Null),
                "port": s.get("port").cloned().unwrap_or(Value::Null),
                "username": s.get("username").cloned().unwrap_or(Value::Null),
                "protocol": s.get("protocol").cloned().unwrap_or(Value::Null),
                "initialPath": s.get("initialPath").cloned().unwrap_or(Value::Null),
                "options": s.get("options").cloned().unwrap_or(Value::Null),
                "providerId": s.get("providerId").cloned().unwrap_or(Value::Null),
                "hasStoredCredential": has_cred,
            })
        })
        .collect();

    Ok(json!({
        "servers": redacted,
        "skipped": value.get("skipped").cloned().unwrap_or_else(|| json!([])),
        "sourcePath": value.get("sourcePath").cloned().unwrap_or(Value::Null),
        "totalRemotes": value
            .get("totalRemotes")
            .and_then(|v| v.as_u64())
            .unwrap_or(servers.len() as u64),
    }))
}

/// Export the GUI's selected profiles to a third-party config file.
/// Profiles whose protocol the target tool cannot carry are filtered
/// out and reported in `skipped` (the "filter by support" contract);
/// secrets are pulled from the vault only when `include_credentials`.
#[tauri::command]
pub async fn export_bridge_config(
    source: String,
    servers_json: String,
    include_credentials: bool,
    file_path: String,
) -> Result<Value, String> {
    known(&source)?;
    let supported = bridge_supported_protocols(&source);
    let (ext, _) = bridge_export_format(&source).unwrap_or(("txt", ""));

    let path = Path::new(&file_path);
    if path.to_string_lossy().contains("..") {
        return Err("Invalid path: directory traversal not allowed".to_string());
    }
    let got_ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if got_ext != ext {
        return Err(format!("Invalid file extension. Expected .{ext}"));
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err("Destination directory does not exist".to_string());
        }
    }

    let raw: Vec<Value> = serde_json::from_str(&servers_json)
        .map_err(|e| format!("Invalid server data: {e}"))?;
    let store = CredentialStore::from_cache();

    // Each arm builds the typed Vec for that source, filtering by the
    // supported-protocol set and resolving `server_<id>` secrets.
    macro_rules! run_export {
        ($ty:path, $f:path) => {{
            let mut typed: Vec<$ty> = Vec::new();
            let mut passwords: HashMap<String, String> = HashMap::new();
            let mut skipped: Vec<Value> = Vec::new();
            for entry in &raw {
                let name = entry
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                let proto = entry
                    .get("protocol")
                    .and_then(|v| v.as_str())
                    .unwrap_or("ftp")
                    .to_string();
                if !supported.contains(&proto.as_str()) {
                    skipped.push(json!({
                        "name": name,
                        "reason": format!("protocol {} not exportable to {}", proto, source),
                    }));
                    continue;
                }
                let one: $ty = serde_json::from_value(entry.clone())
                    .map_err(|e| format!("Invalid server data: {e}"))?;
                if include_credentials {
                    if let (Some(id), Some(st)) = (
                        entry.get("id").and_then(|v| v.as_str()),
                        store.as_ref(),
                    ) {
                        if let Ok(stored) = st.get(&format!("server_{}", id)) {
                            // Stored value may be a JSON credential blob
                            // ({"password": "..."}) or a bare string.
                            let pw = serde_json::from_str::<Value>(&stored)
                                .ok()
                                .and_then(|v| {
                                    v.get("password")
                                        .and_then(|x| x.as_str())
                                        .map(str::to_string)
                                })
                                .unwrap_or_else(|| {
                                    stored.trim_matches('"').to_string()
                                });
                            if !pw.is_empty() {
                                passwords.insert(name.clone(), pw);
                            }
                        }
                    }
                }
                typed.push(one);
            }
            if typed.is_empty() {
                return Ok(json!({
                    "exported": 0,
                    "total": 0,
                    "skipped": skipped,
                    "filePath": file_path,
                    "includesCredentials": false,
                }));
            }
            let total = typed.len();
            let exported = $f(&typed, &passwords, path).map_err(|e| e.to_string())?;
            json!({
                "exported": exported,
                "total": total,
                "skipped": skipped,
                "filePath": file_path,
                "includesCredentials": !passwords.is_empty(),
            })
        }};
    }

    let out = match source.as_str() {
        "aws" => run_export!(
            aws_credentials_import::AwsExportServer,
            aws_credentials_import::export_aws_credentials
        ),
        "ssh" => run_export!(
            ssh_config_import::SshExportServer,
            ssh_config_import::export_ssh_config
        ),
        "mc" => run_export!(mc_import::McExportServer, mc_import::export_mc),
        "cyberduck" => run_export!(
            cyberduck_import::CyberduckExportServer,
            cyberduck_import::export_cyberduck
        ),
        "s3cmd" => run_export!(
            s3cmd_import::S3cmdExportServer,
            s3cmd_import::export_s3cmd
        ),
        "lftp" => run_export!(lftp_import::LftpExportServer, lftp_import::export_lftp),
        "putty" => run_export!(
            putty_import::PuttyExportServer,
            putty_import::export_putty
        ),
        "mobaxterm" => run_export!(
            mobaxterm_import::MobaxtermExportServer,
            mobaxterm_import::export_mobaxterm
        ),
        "dreamweaver" => run_export!(
            dreamweaver_import::DreamweaverExportServer,
            dreamweaver_import::export_dreamweaver
        ),
        "kopia" => run_export!(
            kopia_import::KopiaExportServer,
            kopia_import::export_kopia
        ),
        "duplicacy" => run_export!(
            duplicacy_import::DuplicacyExportServer,
            duplicacy_import::export_duplicacy
        ),
        "restic" => run_export!(
            restic_import::ResticExportServer,
            restic_import::export_restic
        ),
        other => return Err(format!("unknown export source: {other}")),
    };
    Ok(out)
}
