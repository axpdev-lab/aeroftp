// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from MinIO Client (`mc`) configuration.
//!
//! Parses `~/.mc/config.json` (JSON, `{ "version":"10", "aliases": { ... } }`),
//! maps each alias to an S3 profile, and carries the plain `accessKey` /
//! `secretKey` straight across. `mc` stores credentials in clear text in this
//! file, so there is nothing to de-obfuscate: the imported secret is re-stored
//! in our AES-256-GCM vault, which is a security upgrade over the plain file.
//!
//! Secret policy (APPENDIX-BRIDGE, section 1):
//!
//! | Case                         | mc reality            | Bridge policy        |
//! |------------------------------|-----------------------|----------------------|
//! | secret in plain file         | accessKey/secretKey   | populate `credential`|
//! | OS keychain                  | n/a (mc has none)     | n/a                  |
//! | OAuth token                  | n/a (mc is S3 only)   | n/a                  |
//! | SSH key referenced           | n/a                   | n/a                  |
//! | reversible obfuscation       | n/a (mc is plain)     | n/a                  |
//!
//! Only the plain-file row applies to `mc`.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ JSON model (~/.mc/config.json) ============

/// One `aliases.<name>` entry. `mc` writes `url`, `accessKey`, `secretKey`,
/// `api`, `path`; tolerate any missing field and ignore extras.
#[derive(serde::Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct McAlias {
    url: String,
    access_key: String,
    secret_key: String,
    api: String,
    path: String,
}

/// Top-level `~/.mc/config.json`. `version` is informational; extra fields
/// (e.g. cosmetic `colorTheme`) are ignored.
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct McConfig {
    aliases: HashMap<String, McAlias>,
}

// Provider tables / uuid live in `crate::bridge_shared` (Refactor 6).

// ============ Type Mapping ============

struct MappedProfile {
    protocol: String,
    provider_id: Option<String>,
    host: String,
    port: u32,
    username: String,
    password: Option<String>,
    options: Option<serde_json::Value>,
    initial_path: Option<String>,
}

/// Resolve an `mc` alias `url` to `host`, `port`. `endpoint_host` strips the
/// scheme and path and keeps an explicit `:port`; when no explicit port is
/// present the scheme decides (`https` -> 443, `http` -> 80, default 443).
fn host_port_from_url(url: &str) -> (String, u32) {
    let host = crate::bridge_shared::endpoint_host(url);
    if let Some((h, p)) = host.rsplit_once(':') {
        // Only treat the trailing token as a port when it is numeric;
        // otherwise it is part of the host (defensive, mc always emits a URL).
        if let Ok(port) = p.parse::<u32>() {
            return (h.to_string(), port);
        }
    }
    let scheme = url.split_once("://").map(|(s, _)| s).unwrap_or("");
    let port = match scheme.to_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => 443,
    };
    (host, port)
}

/// Decide whether an alias is bridgeable, and how. Returns `Err(reason)` for a
/// skip (the reason is surfaced verbatim in the result), `Ok(profile)`
/// otherwise. The local `play` alias (MinIO's public playground) carries real
/// keys and is therefore a normal import.
fn map_alias(alias: &McAlias) -> Result<MappedProfile, String> {
    if alias.access_key.trim().is_empty() {
        return Err("no accessKey".to_string());
    }

    let host_lower = crate::bridge_shared::endpoint_host(&alias.url).to_lowercase();
    if host_lower.contains("storage.googleapis.com")
        && (alias.access_key.trim().is_empty() || alias.secret_key.trim().is_empty())
    {
        // GCS only speaks S3 with explicit HMAC keys; without them the alias
        // cannot authenticate against an S3 endpoint.
        return Err("GCS without HMAC".to_string());
    }

    let (host, port) = host_port_from_url(&alias.url);
    let provider_id = crate::bridge_shared::map_s3_provider_from_endpoint(&alias.url).to_string();

    let mut options = serde_json::Map::new();
    if !alias.api.trim().is_empty() {
        options.insert(
            "api".to_string(),
            serde_json::Value::String(alias.api.clone()),
        );
    }
    // mc's `path` field is the S3 bucket-lookup style ("auto" | "dns" |
    // "on" | "off"), NOT a remote directory. Mapping it to `initial_path`
    // made the imported profile try to `cd /auto`, breaking `ls /`. Keep
    // it as informational metadata and leave the remote path unset (mc
    // aliases have no remote-dir concept; the bucket is chosen at use).
    if !alias.path.trim().is_empty() {
        options.insert(
            "bucketLookup".to_string(),
            serde_json::Value::String(alias.path.trim().to_string()),
        );
    }
    let initial_path = None;

    Ok(MappedProfile {
        protocol: "s3".to_string(),
        provider_id: Some(provider_id),
        host,
        port,
        username: alias.access_key.clone(),
        password: {
            // mc keeps secrets plain: carry it straight through (the vault
            // re-encrypts on import). Empty string means "no secret stored".
            if alias.secret_key.is_empty() {
                None
            } else {
                Some(alias.secret_key.clone())
            }
        },
        options: if options.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(options))
        },
        initial_path,
    })
}

// ============ Default config path ============

/// Resolve `~/.mc/config.json`.
///
/// Chain: `MC_CONFIG_DIR` env (join `config.json`) -> `$HOME/.mc/config.json`
/// -> Windows `%USERPROFILE%\.mc\config.json`. `mc` exposes no command that
/// prints its config path, so there is no tool-output step (unlike rclone).
pub fn default_mc_config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("MC_CONFIG_DIR") {
        if !dir.is_empty() {
            let p = PathBuf::from(dir).join("config.json");
            if p.exists() {
                return Some(p);
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(home).join(".mc").join("config.json");
            if p.exists() {
                return Some(p);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            let p = PathBuf::from(userprofile).join(".mc").join("config.json");
            if p.exists() {
                return Some(p);
            }
        }
    }

    None
}

// ============ Public API ============

/// An alias that was skipped (no usable credential / unsupported endpoint).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McSkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Result of importing an `mc` config.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<McSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

/// Import every bridgeable alias from an `mc` `config.json`.
pub fn import_mc(path: &Path) -> Result<McImportResult, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read mc config: {}", e))?;
    let config: McConfig =
        serde_json::from_str(&content).map_err(|e| format!("parse mc config json: {}", e))?;

    let total_remotes = config.aliases.len();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    // Stable ordering so import output is deterministic across runs.
    let mut names: Vec<&String> = config.aliases.keys().collect();
    names.sort();

    for name in names {
        let alias = &config.aliases[name];
        match map_alias(alias) {
            Ok(mapped) => {
                let id = format!(
                    "mc-{}-{}",
                    name.to_lowercase().replace(' ', "-"),
                    &crate::bridge_shared::uuid_v4()[..8]
                );
                servers.push(ServerProfileExport {
                    id,
                    name: name.clone(),
                    host: mapped.host,
                    port: mapped.port,
                    username: mapped.username,
                    protocol: Some(mapped.protocol),
                    initial_path: mapped.initial_path,
                    local_initial_path: None,
                    color: None,
                    last_connected: None,
                    options: mapped.options,
                    provider_id: mapped.provider_id,
                    credential: mapped.password,
                    has_stored_credential: None,
                    public_url_base: None,
                });
            }
            Err(reason) => {
                skipped.push(McSkippedRemote {
                    name: name.clone(),
                    kind: "s3".to_string(),
                    reason,
                });
            }
        }
    }

    Ok(McImportResult {
        servers,
        skipped,
        source_path: path.display().to_string(),
        total_remotes,
    })
}

// ============ Export to ~/.mc/config.json ============

/// A server profile to export as an `mc` alias. Mirrors
/// `rclone_import::RcloneExportServer`; the secret is fetched from the vault
/// separately and passed via the `passwords` map keyed by `name`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

/// Export S3 profiles as a native `~/.mc/config.json`.
///
/// Only `s3` profiles are emitted (mc is an S3 client). The secret is written
/// in plain, exactly as `mc` itself stores it; the file is written atomically
/// with `0600` on unix via `atomic_write_600`. Returns the number of aliases
/// written.
pub fn export_mc(
    servers: &[McExportServer],
    passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<usize, String> {
    let mut aliases = serde_json::Map::new();
    let mut exported = 0usize;

    for server in servers {
        if server.protocol.as_deref() != Some("s3") {
            continue;
        }

        // Reconstruct a scheme-qualified URL from host[:port]. mc expects a
        // full URL; default to https unless the port is the plain-HTTP 80.
        let scheme = if server.port == 80 { "http" } else { "https" };
        let host_has_port = server.host.contains(':');
        let url = if host_has_port
            || (scheme == "https" && server.port == 443)
            || (scheme == "http" && server.port == 80)
        {
            format!("{}://{}", scheme, server.host)
        } else {
            format!("{}://{}:{}", scheme, server.host, server.port)
        };

        let api = server
            .options
            .as_ref()
            .and_then(|v| v.as_object())
            .and_then(|m| m.get("api"))
            .and_then(|v| v.as_str())
            .unwrap_or("S3v4")
            .to_string();

        // mc's `path` is the bucket-lookup style, round-tripped from
        // `options.bucketLookup` (default "auto"), never the remote dir.
        let path = server
            .options
            .as_ref()
            .and_then(|v| v.as_object())
            .and_then(|m| m.get("bucketLookup"))
            .and_then(|v| v.as_str())
            .filter(|p| !p.is_empty())
            .unwrap_or("auto")
            .to_string();

        let secret = passwords.get(&server.name).cloned().unwrap_or_default();

        let entry = serde_json::json!({
            "url": url,
            "accessKey": server.username,
            "secretKey": secret,
            "api": api,
            "path": path,
        });
        aliases.insert(server.name.clone(), entry);
        exported += 1;
    }

    let doc = serde_json::json!({
        "version": "10",
        "aliases": serde_json::Value::Object(aliases),
    });
    let body = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    crate::bridge_shared::atomic_write_600(out, &body)?;
    Ok(exported)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
        "version": "10",
        "aliases": {
            "lab": {
                "url": "https://s3.lab.example.com:9000",
                "accessKey": "AKIALAB",
                "secretKey": "labsecret",
                "api": "S3v4",
                "path": "auto"
            },
            "noauth": {
                "url": "https://s3.example.com",
                "accessKey": "",
                "secretKey": "",
                "api": "S3v4",
                "path": "auto"
            }
        }
    }"#;

    fn write_tmp(name: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "mc-test-{}-{}.json",
            name,
            crate::bridge_shared::uuid_v4()
        ));
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn test_parse_fixture() {
        let p = write_tmp("parse", FIXTURE);
        let result = import_mc(&p).expect("import should succeed");
        std::fs::remove_file(&p).ok();

        assert_eq!(result.total_remotes, 2);
        assert_eq!(result.servers.len(), 1);
        assert_eq!(result.skipped.len(), 1);

        let lab = &result.servers[0];
        assert_eq!(lab.name, "lab");
        assert_eq!(lab.host, "s3.lab.example.com");
        assert_eq!(lab.port, 9000);
        assert_eq!(lab.protocol.as_deref(), Some("s3"));
        assert_eq!(lab.username, "AKIALAB");
        assert_eq!(lab.credential.as_deref(), Some("labsecret"));
        // mc `path` is the bucket-lookup style, not a remote dir: it must
        // NOT leak into initial_path (that would break `ls /`).
        assert_eq!(lab.initial_path, None);
        assert_eq!(
            lab.options
                .as_ref()
                .and_then(|o| o.get("bucketLookup"))
                .and_then(|v| v.as_str()),
            Some("auto")
        );
        assert_eq!(lab.provider_id.as_deref(), Some("custom-s3"));

        let skipped = &result.skipped[0];
        assert_eq!(skipped.name, "noauth");
        assert_eq!(skipped.kind, "s3");
        assert_eq!(skipped.reason, "no accessKey");
    }

    #[test]
    fn test_gcs_without_hmac_skipped() {
        let body = r#"{
            "version": "10",
            "aliases": {
                "gcs": {
                    "url": "https://storage.googleapis.com",
                    "accessKey": "GOOG1",
                    "secretKey": "",
                    "api": "S3v4",
                    "path": "auto"
                }
            }
        }"#;
        let p = write_tmp("gcs", body);
        let result = import_mc(&p).expect("import should succeed");
        std::fs::remove_file(&p).ok();

        assert_eq!(result.servers.len(), 0);
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].reason, "GCS without HMAC");
    }

    #[test]
    fn test_secret_policy_plain_populated() {
        // mc stores secrets in plain: a present secretKey must become a
        // populated `credential` (no keychain / OAuth path exists for mc).
        let p = write_tmp("secret", FIXTURE);
        let result = import_mc(&p).expect("import should succeed");
        std::fs::remove_file(&p).ok();

        let lab = &result.servers[0];
        assert_eq!(lab.credential.as_deref(), Some("labsecret"));
        assert!(lab.has_stored_credential.is_none());
    }

    #[test]
    fn test_roundtrip_metadata_idempotence() {
        let p = write_tmp("rt-src", FIXTURE);
        let first = import_mc(&p).expect("first import");
        std::fs::remove_file(&p).ok();

        let export_servers: Vec<McExportServer> = first
            .servers
            .iter()
            .map(|s| McExportServer {
                name: s.name.clone(),
                host: s.host.clone(),
                port: s.port,
                username: s.username.clone(),
                protocol: s.protocol.clone(),
                options: s.options.clone(),
                provider_id: s.provider_id.clone(),
                initial_path: s.initial_path.clone(),
            })
            .collect();

        let mut passwords = HashMap::new();
        for s in &first.servers {
            if let Some(c) = &s.credential {
                passwords.insert(s.name.clone(), c.clone());
            }
        }

        let out = write_tmp("rt-out", "{}");
        let n = export_mc(&export_servers, &passwords, &out).expect("export");
        assert_eq!(n, 1);

        let second = import_mc(&out).expect("re-import");
        std::fs::remove_file(&out).ok();

        assert_eq!(second.servers.len(), first.servers.len());
        let a = &first.servers[0];
        let b = &second.servers[0];
        assert_eq!(a.name, b.name);
        assert_eq!(a.host, b.host);
        assert_eq!(a.port, b.port);
        assert_eq!(a.username, b.username);
        assert_eq!(a.protocol, b.protocol);
        assert_eq!(a.provider_id, b.provider_id);
        assert_eq!(a.initial_path, b.initial_path);
        assert_eq!(a.credential, b.credential);
        // bucket-lookup style survives the export -> re-import round-trip
        let lookup = |s: &ServerProfileExport| {
            s.options
                .as_ref()
                .and_then(|o| o.get("bucketLookup"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        assert_eq!(lookup(a), lookup(b));
        assert_eq!(lookup(a).as_deref(), Some("auto"));
    }

    #[test]
    fn test_default_path_env_override() {
        let dir =
            std::env::temp_dir().join(format!("mc-cfgdir-{}", crate::bridge_shared::uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.json");
        std::fs::write(&cfg, FIXTURE).unwrap();

        let prev = std::env::var("MC_CONFIG_DIR").ok();
        std::env::set_var("MC_CONFIG_DIR", &dir);

        let resolved = default_mc_config_path();
        assert_eq!(resolved.as_deref(), Some(cfg.as_path()));

        match prev {
            Some(v) => std::env::set_var("MC_CONFIG_DIR", v),
            None => std::env::remove_var("MC_CONFIG_DIR"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
