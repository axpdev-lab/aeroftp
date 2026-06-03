// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Generic GUI bridge commands for every bridge source.
//!
//! Originally only the 12 expansion sources were dispatched here while
//! rclone / WinSCP / FileZilla kept their own dedicated `*_config` Tauri
//! commands. APPENDIX-BRIDGE-CONVERGENCE folds those three onto this same
//! generic path: WinSCP/FileZilla dispatch like any other source, and
//! rclone is handled natively in `import_bridge_config`/`export_bridge_config`
//! because it carries per-profile OAuth/Jotta tokens (#214) that the generic
//! `Value` cannot transport. The design still mirrors the CLI's
//! `cmd_import_bridge` / `cmd_export_bridge` so GUI and CLI never diverge.
//! Per-source protocol filtering, the export file format and the secret
//! policy all come from `bridge_shared` (single source of truth).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

use crate::bridge_shared::{
    bridge_export_format, bridge_import_filter, bridge_secret_policy, bridge_supported_protocols,
};
use crate::credential_store::CredentialStore;
use crate::{
    aws_credentials_import, cyberduck_import, dreamweaver_import, duplicacy_import,
    filezilla_import, kopia_import, lftp_import, mc_import, mobaxterm_import, putty_import,
    rclone_import, restic_import, s3cmd_import, ssh_config_import, winscp_import,
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
    let (export_ext, export_label) = bridge_export_format(&source).unwrap_or(("txt", ""));
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

/// Content + filename sniffer for a dropped profile file. Returns the
/// candidate bridge source ids, most-specific first. Pure and cheap: it
/// only looks at the lowercased file name and a bounded text head, never
/// the secrets. Legacy ids (filezilla / rclone / winscp) are included so
/// the frontend can route them to their dedicated panels; the frontend
/// owns the id -> descriptor mapping (bridgeSources.ts), so this stays a
/// thin classifier and does not duplicate metadata.
fn identify_sources(file_name: &str, head: &str) -> Vec<&'static str> {
    let name = file_name.to_ascii_lowercase();
    let h = head; // case-sensitive markers kept where the format is
    let hl = head.to_ascii_lowercase();
    let mut hits: Vec<&'static str> = Vec::new();
    let push = |id: &'static str, hits: &mut Vec<&'static str>| {
        if !hits.contains(&id) {
            hits.push(id);
        }
    };

    // Strong, format-unique markers first.
    if h.contains("<FileZilla3") || (hl.contains("<servers") && name.contains("sitemanager")) {
        push("filezilla", &mut hits);
    }
    if name.ends_with(".ste") || (hl.contains("<site") && hl.contains("<remoteinfo")) {
        push("dreamweaver", &mut hits);
    }
    if name.ends_with(".duck")
        || (hl.contains("<plist") && hl.contains("protocol") && hl.contains("hostname"))
    {
        push("cyberduck", &mut hits);
    }
    if name.ends_with(".reg") && h.contains("SimonTatham\\PuTTY\\Sessions") {
        push("putty", &mut hits);
    }
    if hl.contains("restic_repository") {
        push("restic", &mut hits);
    }
    if (name == "rclone.conf" || name.ends_with("rclone.conf"))
        || (hl.contains("type = ") && hl.contains('[') && hl.contains(']'))
    {
        push("rclone", &mut hits);
    }
    if name.contains("winscp") || hl.contains("[sessions\\") || hl.contains("[configuration]") {
        push("winscp", &mut hits);
    }
    // INI credential families. aws and s3cmd both use [default] + a key;
    // disambiguate on their unique keys but keep both when ambiguous.
    if hl.contains("aws_access_key_id") {
        push("aws", &mut hits);
    }
    if name.contains("s3cfg")
        || (hl.contains("access_key") && hl.contains("secret_key") && hl.contains("host_base"))
    {
        push("s3cmd", &mut hits);
    }
    if (name == "config" && hl.contains("host ") && hl.contains("hostname"))
        || (hl.contains("\nhost ") && hl.contains("hostname "))
    {
        push("ssh", &mut hits);
    }
    if hl.contains("[bookmarks") && hl.contains("mobaxterm") {
        push("mobaxterm", &mut hits);
    }
    if name.ends_with(".rc") || (hl.contains("bookmark ") && hl.contains("set ")) {
        push("lftp", &mut hits);
    }
    // JSON families: mc / kopia / duplicacy.
    if hl.contains("\"aliases\"") || (hl.contains("\"url\"") && hl.contains("\"accesskey\"")) {
        push("mc", &mut hits);
    }
    if hl.contains("\"storage\"") && hl.contains("\"type\"") && hl.contains("\"hostname\"") {
        push("kopia", &mut hits);
    }
    if hl.contains("\"storage_backend\"") || hl.contains("\"snapshot_id\"") {
        push("duplicacy", &mut hits);
    }
    hits
}

/// Identify which bridge source a dropped profile file belongs to so the
/// UI can route a drag-and-drop straight to that source's import form
/// without the user picking from the list. Same path validation as
/// `import_bridge_config` (traversal reject + canonicalize + regular
/// file + 10 MB cap). Never returns file contents: profile files carry
/// credentials, so only the file name and the candidate ids are exposed;
/// the redacted preview happens later in `import_bridge_config`.
#[tauri::command]
pub async fn bridge_identify(file_path: String) -> Result<Value, String> {
    let path = Path::new(&file_path);
    if path.to_string_lossy().contains("..") {
        return Err("Invalid path: directory traversal not allowed".to_string());
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| "File not found or inaccessible".to_string())?;
    let metadata =
        std::fs::metadata(&canonical).map_err(|_| "Cannot read file metadata".to_string())?;
    if !metadata.is_file() {
        return Err("Not a regular file".to_string());
    }
    if metadata.len() > 10 * 1024 * 1024 {
        return Err("File too large (max 10 MB)".to_string());
    }

    let file_name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // Read a bounded head only; enough for every format's markers and
    // cheap. Binary/garbled files just yield zero candidates.
    let bytes = std::fs::read(&canonical).map_err(|_| "Cannot read file".to_string())?;
    let head_len = bytes.len().min(64 * 1024);
    let head = String::from_utf8_lossy(&bytes[..head_len]);

    let candidates = identify_sources(&file_name, &head);
    Ok(json!({
        "fileName": file_name,
        "candidates": candidates,
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
        "winscp" => to_v(winscp_import::import_winscp(path)),
        "filezilla" => to_v(filezilla_import::import_filezilla(path)),
        // rclone is intercepted in import_bridge_config (it needs the
        // CredentialStore for its #214 per-profile OAuth/Jotta tokens) and
        // never reaches this pure dispatcher.
        other => Err(format!("unknown import source: {other}")),
    }
}

/// Persist rclone's per-profile OAuth/Jotta tokens (#214) into the vault with
/// the same keys the saved-server reconnect path expects
/// (`oauth_<slug>_<id>`, `jottacloud_refresh_<id>`). No-op when the vault is
/// locked; the server-credential loop in `import_bridge_config` already
/// surfaces that case to the caller.
fn store_rclone_provider_secrets(result: &rclone_import::RcloneImportResult) {
    let store = match CredentialStore::from_cache() {
        Some(s) => s,
        None => return,
    };
    let protocol_by_id: HashMap<&str, String> = result
        .servers
        .iter()
        .map(|s| {
            (
                s.id.as_str(),
                s.protocol.clone().unwrap_or_default().to_lowercase(),
            )
        })
        .collect();
    for (profile_id, secrets) in &result.provider_secrets {
        let protocol = match protocol_by_id.get(profile_id.as_str()) {
            Some(p) => p,
            None => continue,
        };
        if let Some(ref oauth_json) = secrets.oauth {
            if let Some(slug) = crate::oauth_vault_slug_for_protocol(protocol) {
                if let Err(e) = store.store(&format!("oauth_{}_{}", slug, profile_id), oauth_json) {
                    log::warn!("rclone bridge: oauth token store failed for {profile_id}: {e}");
                }
            }
        }
        if let Some(ref jotta_json) = secrets.jotta_refresh {
            if protocol == "jottacloud" {
                if let Err(e) =
                    store.store(&format!("jottacloud_refresh_{}", profile_id), jotta_json)
                {
                    log::warn!("rclone bridge: jotta token store failed for {profile_id}: {e}");
                }
            }
        }
    }
}

/// Import profiles from a third-party config file. Validates the path
/// (traversal reject + canonicalize + regular-file + 10 MB cap),
/// upgrades any recovered secret into the AES-256-GCM vault and returns
/// credential-redacted profiles, exactly like `import_rclone_config`.
#[tauri::command]
pub async fn import_bridge_config(source: String, file_path: String) -> Result<Value, String> {
    known(&source)?;

    let path = Path::new(&file_path);
    if path.to_string_lossy().contains("..") {
        return Err("Invalid path: directory traversal not allowed".to_string());
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| "File not found or inaccessible".to_string())?;
    let metadata =
        std::fs::metadata(&canonical).map_err(|_| "Cannot read file metadata".to_string())?;
    if !metadata.is_file() {
        return Err("Not a regular file".to_string());
    }
    if metadata.len() > 10 * 1024 * 1024 {
        return Err("File too large (max 10 MB)".to_string());
    }

    // rclone carries per-profile OAuth/Jotta tokens (#214) in a
    // `#[serde(skip)]` field that a generic `Value` cannot transport, so we
    // import it natively here and persist those tokens before serialising the
    // server list back into the shared post-processing below.
    let value = if source == "rclone" {
        let result = rclone_import::import_rclone(&canonical).map_err(|e| e.to_string())?;
        store_rclone_provider_secrets(&result);
        serde_json::to_value(&result).map_err(|e| e.to_string())?
    } else {
        dispatch_import(&source, &canonical)?
    };
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
                .map(|c| !c.is_null() && c.as_str().map(|x| !x.is_empty()).unwrap_or(true))
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

    let raw: Vec<Value> =
        serde_json::from_str(&servers_json).map_err(|e| format!("Invalid server data: {e}"))?;
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
        "s3cmd" => run_export!(s3cmd_import::S3cmdExportServer, s3cmd_import::export_s3cmd),
        "lftp" => run_export!(lftp_import::LftpExportServer, lftp_import::export_lftp),
        "putty" => run_export!(putty_import::PuttyExportServer, putty_import::export_putty),
        "mobaxterm" => run_export!(
            mobaxterm_import::MobaxtermExportServer,
            mobaxterm_import::export_mobaxterm
        ),
        "dreamweaver" => run_export!(
            dreamweaver_import::DreamweaverExportServer,
            dreamweaver_import::export_dreamweaver
        ),
        "kopia" => run_export!(kopia_import::KopiaExportServer, kopia_import::export_kopia),
        "duplicacy" => run_export!(
            duplicacy_import::DuplicacyExportServer,
            duplicacy_import::export_duplicacy
        ),
        "restic" => run_export!(
            restic_import::ResticExportServer,
            restic_import::export_restic
        ),
        "rclone" => run_export!(
            rclone_import::RcloneExportServer,
            rclone_import::export_rclone
        ),
        "winscp" => run_export!(
            winscp_import::WinScpExportServer,
            winscp_import::export_winscp
        ),
        "filezilla" => run_export!(
            filezilla_import::FileZillaExportServer,
            filezilla_import::export_filezilla
        ),
        other => return Err(format!("unknown export source: {other}")),
    };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(name: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aeroftp_bridge_test_{}_{}",
            std::process::id(),
            name
        ));
        std::fs::write(&path, contents).expect("write temp fixture");
        path
    }

    fn server_count(v: &Value) -> usize {
        v.get("servers")
            .and_then(|s| s.as_array())
            .map(|a| a.len())
            .unwrap_or(0)
    }

    #[test]
    fn dispatch_import_handles_winscp() {
        let ini = "[Sessions\\My%20Server]\nHostName=example.com\nPortNumber=22\nUserName=admin\nFSProtocol=2\nFtps=0\n";
        let path = write_temp("winscp.ini", ini);
        let v = dispatch_import("winscp", &path).expect("winscp dispatch");
        assert_eq!(server_count(&v), 1, "winscp server parsed");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dispatch_import_handles_filezilla() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<FileZilla3>
  <Servers>
    <Server>
      <Host>ftp.example.com</Host>
      <Port>21</Port>
      <Protocol>0</Protocol>
      <User>admin</User>
      <Name>My FTP Server</Name>
    </Server>
  </Servers>
</FileZilla3>"#;
        let path = write_temp("sitemanager.xml", xml);
        let v = dispatch_import("filezilla", &path).expect("filezilla dispatch");
        assert_eq!(server_count(&v), 1, "filezilla server parsed");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dispatch_import_rejects_unknown() {
        let path = write_temp("x.txt", "nothing");
        assert!(dispatch_import("totalcommander", &path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_export_servers_deserialize_from_profile_json() {
        // The run_export! macro deserializes each profile entry into the
        // source's *ExportServer type; verify the three migrated sources
        // accept the standard saved-server JSON shape.
        let entry = json!({
            "id": "abc", "name": "S", "host": "h.example.com", "port": 21,
            "username": "u", "protocol": "ftp", "options": {}
        });
        assert!(
            serde_json::from_value::<rclone_import::RcloneExportServer>(entry.clone()).is_ok()
        );
        assert!(
            serde_json::from_value::<winscp_import::WinScpExportServer>(entry.clone()).is_ok()
        );
        assert!(
            serde_json::from_value::<filezilla_import::FileZillaExportServer>(entry).is_ok()
        );
    }
}
