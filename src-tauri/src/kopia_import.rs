// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import/export server profiles from a Kopia `repository.config`.
//!
//! Fase 3 (APPENDIX-BRIDGE, backup-repo track). A Kopia repository is an
//! encrypted backup repository, not a plain transfer endpoint. The bridge
//! value here is **backend reuse**: we extract the underlying storage
//! (S3/B2/SFTP/WebDAV) the repo lives on and create a normal AeroFTP
//! `ServerProfileExport`, so AeroVault/AeroSync can point at the same
//! storage the user already backs up to.
//!
//! Source JSON shape (Kopia `repository.config`):
//! ```json
//! { "storage": { "type": "s3",
//!   "config": { "bucket":"b","endpoint":"s3.x.com","accessKeyID":"AK",
//!               "secretAccessKey":"SK","sessionToken":"","region":"us-east-1",
//!               "prefix":"kopia/" } } }
//! ```
//! `storage.type` is one of `s3 | b2 | gcs | azureBlob | sftp | webdav |
//! filesystem`.
//!
//! ## Secret policy (this module)
//! | source                     | handling                                              |
//! |----------------------------|-------------------------------------------------------|
//! | s3 secretAccessKey (plain) | populate `credential`                                 |
//! | b2 key (plain)             | populate `credential`                                 |
//! | webdav password (plain)    | populate `credential`                                 |
//! | sftp keyfile (referenced)  | `credential=None`, `options.private_key_path = path`, |
//! |                            | `has_stored_credential=Some(false)`                   |
//! | gcs / azureBlob            | skipped, reason "OAuth/SAS backend not bridgeable"    |
//! | filesystem                 | skipped, reason "local filesystem, not a server"      |
//!
//! Imported plaintext secrets are re-stored in the AeroFTP AES-256-GCM
//! vault, upgrading them from a plaintext config file to authenticated
//! encryption.
//!
//! ## Honest caveat about export
//! `export_kopia` emits ONLY the storage-connection block consumable by
//! `kopia repository connect from-config --file <out>`. It is NOT a full
//! Kopia `repository.config`: a real repo also carries crypto/format
//! parameters (encryption algorithm, splitter, hashing, ECC) that AeroFTP
//! does not possess and cannot reconstruct. This is the storage handle,
//! not the repository. The secret is written in plaintext inside the
//! `config` block, so the file is created with `atomic_write_600`.
//!
//! Kopia connects to exactly ONE repository, so `export_kopia` emits ONE
//! `repository.config`. If multiple profiles are supplied, only the first
//! is exported (documented; zero profiles is an error).

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ Source JSON model ============

#[derive(serde::Deserialize)]
struct KopiaConfig {
    storage: KopiaStorage,
}

#[derive(serde::Deserialize)]
struct KopiaStorage {
    #[serde(rename = "type")]
    kind: String,
    config: serde_json::Value,
}

// ============ Public result types ============

/// A Kopia storage backend that could not be bridged.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KopiaSkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Result of importing a Kopia `repository.config`.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KopiaImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<KopiaSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

// ============ Default config path detection ============

/// Returns the default Kopia `repository.config` path for the platform.
///
/// Mirrors `rclone_import::default_rclone_config_path`: env override first
/// (`KOPIA_CONFIG_PATH`), then the per-OS default. Kopia has no stable
/// "print config path" subcommand, so there is no tool-output probe step.
pub fn default_kopia_config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("KOPIA_CONFIG_PATH") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return Some(p);
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".config/kopia/repository.config");
            if path.exists() {
                return Some(path);
            }
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let path = PathBuf::from(xdg).join("kopia/repository.config");
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home)
                .join("Library/Application Support/kopia/repository.config");
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let path = PathBuf::from(local).join("kopia\\repository.config");
            if path.exists() {
                return Some(path);
            }
        }
    }

    None
}

// ============ Import ============

/// Outcome of mapping one Kopia storage block.
enum Mapped {
    /// A usable profile.
    Server(Box<ServerProfileExport>),
    /// A backend we intentionally do not bridge, with a reason.
    Skipped(String),
}

/// Map a parsed Kopia config into either a profile or a skip reason.
fn map_storage(kind: &str, config: &serde_json::Value) -> Mapped {
    let g = |k: &str| {
        config
            .get(k)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    use crate::bridge_shared::{
        default_port_for, endpoint_host, json_map, map_s3_provider_from_endpoint, uuid_v4,
    };

    let (protocol, provider_id, host, username, credential, key_ref, mut opts) = match kind {
        "s3" => {
            let ep = g("endpoint").unwrap_or_default();
            (
                "s3",
                Some(map_s3_provider_from_endpoint(&ep).to_string()),
                endpoint_host(&ep),
                g("accessKeyID").unwrap_or_default(),
                g("secretAccessKey"),
                None,
                json_map(&[
                    ("bucket", g("bucket")),
                    ("prefix", g("prefix")),
                    ("region", g("region")),
                    ("sessionToken", g("sessionToken")),
                ]),
            )
        }
        "b2" => (
            "s3",
            Some("backblaze-b2".to_string()),
            String::new(),
            g("keyID").unwrap_or_default(),
            g("key"),
            None,
            json_map(&[("bucket", g("bucket"))]),
        ),
        "sftp" => {
            // SSH key is referenced by path only; no key bytes are copied.
            let keyfile = g("keyfile").filter(|s| !s.is_empty());
            (
                "sftp",
                None,
                g("host").unwrap_or_default(),
                g("username").unwrap_or_default(),
                None,
                keyfile.clone(),
                json_map(&[("path", g("path")), ("private_key_path", keyfile)]),
            )
        }
        "webdav" => (
            "webdav",
            Some("custom-webdav".to_string()),
            endpoint_host(&g("url").unwrap_or_default()),
            g("username").unwrap_or_default(),
            g("password"),
            None,
            json_map(&[]),
        ),
        "gcs" | "azureblob" => {
            return Mapped::Skipped(
                "OAuth/SAS backend not bridgeable".to_string(),
            );
        }
        "filesystem" => {
            return Mapped::Skipped("local filesystem, not a server".to_string());
        }
        other => {
            return Mapped::Skipped(format!(
                "unsupported kopia storage type: {other}"
            ));
        }
    };

    opts.insert(
        "repo_kind".to_string(),
        serde_json::Value::String("kopia".to_string()),
    );

    // SSH key referenced -> mark credential absent but recoverable via key.
    let has_stored_credential = if protocol == "sftp" && key_ref.is_some() {
        Some(false)
    } else {
        None
    };

    Mapped::Server(Box::new(ServerProfileExport {
        id: format!("kopia-{}-{}", kind, &uuid_v4()[..8]),
        name: format!("Kopia ({kind})"),
        host,
        port: default_port_for(protocol),
        username,
        protocol: Some(protocol.to_string()),
        initial_path: None,
        local_initial_path: None,
        color: None,
        last_connected: None,
        options: Some(serde_json::Value::Object(opts)),
        provider_id,
        credential,
        has_stored_credential,
        public_url_base: None,
    }))
}

/// Import the backend of a Kopia `repository.config`.
///
/// A `repository.config` describes exactly one repository, hence at most
/// one profile (`total_remotes` is always 1). `gcs`/`azureBlob`/
/// `filesystem` backends are reported in `skipped` rather than failing the
/// whole import.
pub fn import_kopia(path: &Path) -> Result<KopiaImportResult, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("read kopia config: {e}"))?;
    let kc: KopiaConfig =
        serde_json::from_str(&raw).map_err(|e| format!("parse kopia json: {e}"))?;

    let kind = kc.storage.kind.to_lowercase();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    match map_storage(&kind, &kc.storage.config) {
        Mapped::Server(s) => servers.push(*s),
        Mapped::Skipped(reason) => skipped.push(KopiaSkippedRemote {
            name: format!("Kopia ({})", kc.storage.kind),
            kind: kc.storage.kind.clone(),
            reason,
        }),
    }

    Ok(KopiaImportResult {
        servers,
        skipped,
        source_path: path.display().to_string(),
        total_remotes: 1,
    })
}

// ============ Export ============

/// A server profile to export as a Kopia storage block.
///
/// Mirrors `rclone_import::RcloneExportServer` plus `initial_path` (the
/// sftp `path` lives there). The password is fetched from the vault by the
/// caller and passed in `passwords` keyed by `name`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KopiaExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

/// Build the Kopia storage block for a single profile.
fn storage_block(
    server: &KopiaExportServer,
    secret: Option<&str>,
) -> Result<serde_json::Value, String> {
    let opts = server.options.as_ref().and_then(|v| v.as_object());
    let get = |k: &str| {
        opts.and_then(|m| m.get(k))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    let storage = match server.protocol.as_deref() {
        Some("s3") if server.provider_id.as_deref() == Some("backblaze-b2") => {
            serde_json::json!({
                "type": "b2",
                "config": {
                    "bucket": get("bucket"),
                    "keyID": server.username,
                    "key": secret.unwrap_or(""),
                }
            })
        }
        Some("s3") => serde_json::json!({
            "type": "s3",
            "config": {
                "bucket": get("bucket"),
                "endpoint": server.host,
                "accessKeyID": server.username,
                "secretAccessKey": secret.unwrap_or(""),
                "region": get("region"),
                "prefix": get("prefix"),
            }
        }),
        Some("sftp") => serde_json::json!({
            "type": "sftp",
            "config": {
                "host": server.host,
                "username": server.username,
                "path": server.initial_path.clone().unwrap_or_default(),
                "keyfile": get("private_key_path"),
            }
        }),
        Some("webdav") => serde_json::json!({
            "type": "webdav",
            "config": {
                "url": format!("https://{}", server.host),
                "username": server.username,
                "password": secret.unwrap_or(""),
            }
        }),
        other => {
            return Err(format!(
                "kopia export: protocol {other:?} not a kopia storage type"
            ))
        }
    };

    Ok(serde_json::json!({ "storage": storage }))
}

/// Export ONE profile as a Kopia storage-connection JSON file.
///
/// Kopia connects to a single repository, so a `repository.config` carries
/// exactly one storage block. When `servers` holds more than one profile
/// only the first is exported; an empty slice is an error. Returns the
/// number of profiles written (0 or 1) to match the contract surface of
/// the other bridge `export_*` functions.
///
/// The emitted file is the storage-connection block only (consumable by
/// `kopia repository connect from-config --file <out>`), NOT a full Kopia
/// repository config. The secret is written in plaintext inside `config`,
/// so the file is created via `atomic_write_600` (temp + rename, unix
/// 0600).
pub fn export_kopia(
    servers: &[KopiaExportServer],
    passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<usize, String> {
    let server = servers
        .first()
        .ok_or_else(|| "kopia export: no profile supplied".to_string())?;

    let secret = passwords.get(&server.name).map(|s| s.as_str());
    let doc = storage_block(server, secret)?;
    let body = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    crate::bridge_shared::atomic_write_600(out, &body)?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_str(json: &str) -> KopiaImportResult {
        let tmp = std::env::temp_dir()
            .join(format!("aeroftp-kopia-{}.config", crate::bridge_shared::uuid_v4()));
        std::fs::write(&tmp, json).unwrap();
        let r = import_kopia(&tmp).expect("import");
        std::fs::remove_file(&tmp).ok();
        r
    }

    #[test]
    fn test_parse_s3_full() {
        let cfg = r#"{ "storage": { "type": "s3", "config": {
            "bucket": "backups",
            "endpoint": "https://s3.wasabisys.com",
            "accessKeyID": "AKIAEXAMPLE",
            "secretAccessKey": "SECRETKEY",
            "sessionToken": "tok123",
            "region": "us-east-1",
            "prefix": "kopia/" } } }"#;
        let r = import_str(cfg);
        assert_eq!(r.total_remotes, 1);
        assert_eq!(r.servers.len(), 1);
        assert!(r.skipped.is_empty());
        let s = &r.servers[0];
        assert_eq!(s.protocol.as_deref(), Some("s3"));
        assert_eq!(s.provider_id.as_deref(), Some("wasabi"));
        assert_eq!(s.host, "s3.wasabisys.com");
        assert_eq!(s.port, 443);
        assert_eq!(s.username, "AKIAEXAMPLE");
        assert_eq!(s.credential.as_deref(), Some("SECRETKEY"));
        let o = s.options.as_ref().unwrap().as_object().unwrap();
        assert_eq!(o.get("bucket").unwrap(), "backups");
        assert_eq!(o.get("prefix").unwrap(), "kopia/");
        assert_eq!(o.get("region").unwrap(), "us-east-1");
        assert_eq!(o.get("sessionToken").unwrap(), "tok123");
        assert_eq!(o.get("repo_kind").unwrap(), "kopia");
    }

    #[test]
    fn test_parse_b2() {
        let cfg = r#"{ "storage": { "type": "b2", "config": {
            "bucket": "my-b2-bucket", "keyID": "002abc", "key": "K00xyz" } } }"#;
        let r = import_str(cfg);
        let s = &r.servers[0];
        assert_eq!(s.protocol.as_deref(), Some("s3"));
        assert_eq!(s.provider_id.as_deref(), Some("backblaze-b2"));
        assert_eq!(s.host, "");
        assert_eq!(s.username, "002abc");
        assert_eq!(s.credential.as_deref(), Some("K00xyz"));
        let o = s.options.as_ref().unwrap().as_object().unwrap();
        assert_eq!(o.get("bucket").unwrap(), "my-b2-bucket");
        assert_eq!(o.get("repo_kind").unwrap(), "kopia");
    }

    #[test]
    fn test_parse_sftp_with_keyfile() {
        let cfg = r#"{ "storage": { "type": "sftp", "config": {
            "host": "nas.example.com", "username": "backup",
            "path": "/volume1/kopia", "keyfile": "/home/u/.ssh/id_ed25519" } } }"#;
        let r = import_str(cfg);
        let s = &r.servers[0];
        assert_eq!(s.protocol.as_deref(), Some("sftp"));
        assert_eq!(s.host, "nas.example.com");
        assert_eq!(s.port, 22);
        assert_eq!(s.username, "backup");
        // Secret policy: SSH key referenced -> no credential, key by path.
        assert!(s.credential.is_none());
        assert_eq!(s.has_stored_credential, Some(false));
        let o = s.options.as_ref().unwrap().as_object().unwrap();
        assert_eq!(o.get("path").unwrap(), "/volume1/kopia");
        assert_eq!(
            o.get("private_key_path").unwrap(),
            "/home/u/.ssh/id_ed25519"
        );
    }

    #[test]
    fn test_parse_webdav() {
        let cfg = r#"{ "storage": { "type": "webdav", "config": {
            "url": "https://dav.example.com/remote.php/dav",
            "username": "alice", "password": "davpass" } } }"#;
        let r = import_str(cfg);
        let s = &r.servers[0];
        assert_eq!(s.protocol.as_deref(), Some("webdav"));
        assert_eq!(s.provider_id.as_deref(), Some("custom-webdav"));
        assert_eq!(s.host, "dav.example.com");
        assert_eq!(s.username, "alice");
        assert_eq!(s.credential.as_deref(), Some("davpass"));
    }

    #[test]
    fn test_gcs_skipped() {
        let cfg = r#"{ "storage": { "type": "gcs", "config": {
            "bucket": "gcs-bucket" } } }"#;
        let r = import_str(cfg);
        assert!(r.servers.is_empty());
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(r.skipped[0].kind, "gcs");
        assert_eq!(r.skipped[0].reason, "OAuth/SAS backend not bridgeable");
    }

    #[test]
    fn test_filesystem_skipped() {
        let cfg = r#"{ "storage": { "type": "filesystem", "config": {
            "path": "/mnt/backup" } } }"#;
        let r = import_str(cfg);
        assert!(r.servers.is_empty());
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(r.skipped[0].kind, "filesystem");
        assert_eq!(r.skipped[0].reason, "local filesystem, not a server");
    }

    #[test]
    fn test_roundtrip_s3() {
        let cfg = r#"{ "storage": { "type": "s3", "config": {
            "bucket": "rt-bucket",
            "endpoint": "https://s3.us-east-1.amazonaws.com",
            "accessKeyID": "AKIART",
            "secretAccessKey": "RTSECRET",
            "region": "us-east-1",
            "prefix": "rt/" } } }"#;
        let r1 = import_str(cfg);
        let s1 = &r1.servers[0];

        let export = vec![KopiaExportServer {
            name: s1.name.clone(),
            host: s1.host.clone(),
            port: s1.port,
            username: s1.username.clone(),
            protocol: s1.protocol.clone(),
            options: s1.options.clone(),
            provider_id: s1.provider_id.clone(),
            initial_path: s1.initial_path.clone(),
        }];
        let mut passwords = HashMap::new();
        passwords.insert(
            s1.name.clone(),
            s1.credential.clone().unwrap_or_default(),
        );

        let out = std::env::temp_dir()
            .join(format!("aeroftp-kopia-rt-{}.config", crate::bridge_shared::uuid_v4()));
        let n = export_kopia(&export, &passwords, &out).expect("export");
        assert_eq!(n, 1);

        let r2 = import_kopia(&out).expect("reimport");
        std::fs::remove_file(&out).ok();
        let s2 = &r2.servers[0];

        // Metadata + secret idempotence.
        assert_eq!(s2.protocol.as_deref(), Some("s3"));
        assert_eq!(s2.provider_id.as_deref(), Some("amazon-s3"));
        assert_eq!(s2.host, s1.host);
        assert_eq!(s2.username, s1.username);
        assert_eq!(s2.credential.as_deref(), Some("RTSECRET"));
        let o2 = s2.options.as_ref().unwrap().as_object().unwrap();
        assert_eq!(o2.get("bucket").unwrap(), "rt-bucket");
        assert_eq!(o2.get("region").unwrap(), "us-east-1");
        assert_eq!(o2.get("prefix").unwrap(), "rt/");
    }

    #[test]
    fn test_roundtrip_b2() {
        let cfg = r#"{ "storage": { "type": "b2", "config": {
            "bucket": "b2-rt", "keyID": "002rt", "key": "K00rt" } } }"#;
        let r1 = import_str(cfg);
        let s1 = &r1.servers[0];

        let export = vec![KopiaExportServer {
            name: s1.name.clone(),
            host: s1.host.clone(),
            port: s1.port,
            username: s1.username.clone(),
            protocol: s1.protocol.clone(),
            options: s1.options.clone(),
            provider_id: s1.provider_id.clone(),
            initial_path: s1.initial_path.clone(),
        }];
        let mut passwords = HashMap::new();
        passwords.insert(s1.name.clone(), "K00rt".to_string());

        let out = std::env::temp_dir()
            .join(format!("aeroftp-kopia-b2rt-{}.config", crate::bridge_shared::uuid_v4()));
        export_kopia(&export, &passwords, &out).expect("export");
        let r2 = import_kopia(&out).expect("reimport");
        std::fs::remove_file(&out).ok();
        let s2 = &r2.servers[0];

        assert_eq!(s2.provider_id.as_deref(), Some("backblaze-b2"));
        assert_eq!(s2.username, "002rt");
        assert_eq!(s2.credential.as_deref(), Some("K00rt"));
        let o2 = s2.options.as_ref().unwrap().as_object().unwrap();
        assert_eq!(o2.get("bucket").unwrap(), "b2-rt");
    }

    #[test]
    fn test_secret_policy() {
        // s3 / b2 / webdav: credential populated from plaintext config.
        let s3 = import_str(
            r#"{"storage":{"type":"s3","config":{"endpoint":"https://s3.x.com","accessKeyID":"A","secretAccessKey":"S"}}}"#,
        );
        assert_eq!(s3.servers[0].credential.as_deref(), Some("S"));
        let b2 = import_str(
            r#"{"storage":{"type":"b2","config":{"keyID":"K","key":"KK"}}}"#,
        );
        assert_eq!(b2.servers[0].credential.as_deref(), Some("KK"));
        let dav = import_str(
            r#"{"storage":{"type":"webdav","config":{"url":"https://d.x.com","username":"u","password":"p"}}}"#,
        );
        assert_eq!(dav.servers[0].credential.as_deref(), Some("p"));

        // sftp keyfile: credential None + private_key_path set.
        let sftp = import_str(
            r#"{"storage":{"type":"sftp","config":{"host":"h","username":"u","keyfile":"/k"}}}"#,
        );
        assert!(sftp.servers[0].credential.is_none());
        assert_eq!(sftp.servers[0].has_stored_credential, Some(false));
        let o = sftp.servers[0]
            .options
            .as_ref()
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(o.get("private_key_path").unwrap(), "/k");
    }

    #[test]
    fn test_export_empty_is_error() {
        let out = std::env::temp_dir().join("aeroftp-kopia-empty.config");
        let r = export_kopia(&[], &HashMap::new(), &out);
        assert!(r.is_err());
    }

    #[test]
    fn test_default_path_env_override() {
        let dir = std::env::temp_dir()
            .join(format!("aeroftp-kopia-defpath-{}", crate::bridge_shared::uuid_v4()));
        let cfgdir = dir.join("kopia");
        std::fs::create_dir_all(&cfgdir).unwrap();
        let cfg = cfgdir.join("repository.config");
        std::fs::write(&cfg, b"{}").unwrap();

        let prev_xdg = std::env::var("XDG_CONFIG_HOME").ok();
        let prev_home = std::env::var("HOME").ok();
        let prev_kopia = std::env::var("KOPIA_CONFIG_PATH").ok();
        std::env::remove_var("KOPIA_CONFIG_PATH");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::set_var("HOME", &dir);

        let resolved = default_kopia_config_path();

        // restore
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        if let Some(v) = prev_kopia {
            std::env::set_var("KOPIA_CONFIG_PATH", v);
        }

        #[cfg(target_os = "linux")]
        assert_eq!(resolved.as_deref(), Some(cfg.as_path()));
        #[cfg(not(target_os = "linux"))]
        let _ = resolved;

        std::fs::remove_dir_all(&dir).ok();
    }
}
