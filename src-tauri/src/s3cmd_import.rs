// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import/export server profiles from the s3cmd configuration file (`~/.s3cfg`).
//!
//! s3cmd is mono-config: a single INI file with one `[default]` section. The
//! relevant keys are `access_key`, `secret_key`, `host_base`, `host_bucket`,
//! `bucket_location` (region), and `use_https`. The S3 secret lives plain in
//! the file, so it is bridged and re-encrypted into our AES-256-GCM vault.
//!
//! Secret policy (APPENDIX-BRIDGE section 1):
//!
//! | Case                         | Example                  | Bridge policy                                            |
//! |------------------------------|--------------------------|----------------------------------------------------------|
//! | Secret in plain/obfusc. file | s3cmd `secret_key` plain | import the secret, re-encrypt into AES-256-GCM vault      |
//! | Reversible obfuscation       | (n/a for s3cmd)          | reveal on import, re-obscure on export                    |
//! | Secret in OS keychain        | (n/a for s3cmd)          | metadata only, has_stored_credential=false, log "re-enter"|
//! | OAuth token                  | (n/a for s3cmd)          | stub profile, credential=None, manual re-auth             |
//! | SSH key referenced           | (n/a for s3cmd)          | options.private_key_path, no key bytes                    |
//!
//! s3cmd stores the S3 secret in cleartext, so the only policy row that
//! applies here is the first one: the secret is populated into `credential`
//! and recrypted into the vault on import.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

/// Parse a boolean-ish s3cmd value. s3cmd writes `True`/`False`; treat an
/// empty/absent value as the s3cmd default (HTTPS on).
fn parse_use_https(value: Option<&str>) -> bool {
    match value.map(|v| v.trim().to_lowercase()) {
        Some(v) if v == "false" || v == "0" || v == "no" || v == "off" => false,
        // missing, empty, "true", "1", "yes", "on", anything else -> HTTPS
        _ => true,
    }
}

/// Split a `host[:port]` token into host and an explicit port if present.
fn split_host_port(host_base: &str) -> (String, Option<u32>) {
    let trimmed = host_base.trim();
    match trimmed.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u32>() {
            Ok(port) => (h.to_string(), Some(port)),
            // not a numeric port (e.g. an unexpected scheme remnant): keep whole
            Err(_) => (trimmed.to_string(), None),
        },
        None => (trimmed.to_string(), None),
    }
}

/// Map the single s3cmd `[default]` section to a profile.
///
/// Returns `None` (with a caller-side skip reason) only when `access_key`
/// is empty: without it the profile cannot authenticate to S3.
fn map_s3cmd(section: &HashMap<String, String>) -> Option<MappedProfile> {
    let get = |k: &str| section.get(k).map(|s| s.trim()).filter(|s| !s.is_empty());

    let access_key = get("access_key")?.to_string();

    let host_base_raw = get("host_base").unwrap_or("").to_string();
    let provider_id =
        crate::bridge_shared::map_s3_provider_from_endpoint(&host_base_raw).to_string();

    // host_base may carry an explicit `host:port`. An empty host_base means
    // the user relies on the AWS default endpoint: keep host empty and let
    // the amazon-s3 provider resolve it from the region.
    let host_base_endpoint = crate::bridge_shared::endpoint_host(&host_base_raw);
    let (host, explicit_port) = if host_base_endpoint.is_empty() {
        (String::new(), None)
    } else {
        split_host_port(&host_base_endpoint)
    };

    let use_https = parse_use_https(get("use_https"));
    // Explicit `:port` in host_base wins; otherwise derive from use_https.
    let port = explicit_port.unwrap_or(if use_https { 443 } else { 80 });

    let region = get("bucket_location").map(|s| s.to_string());
    let host_bucket = get("host_bucket").map(|s| s.to_string());

    let mut options = serde_json::Map::new();
    if let Some(r) = region {
        options.insert("region".into(), serde_json::Value::String(r));
    }
    // Preserve the host_bucket template so an export round-trips it; it is
    // metadata only (s3cmd uses it for virtual-hosted-style addressing).
    if let Some(hb) = host_bucket {
        options.insert("hostBucket".into(), serde_json::Value::String(hb));
    }
    options.insert("useHttps".into(), serde_json::Value::Bool(use_https));

    Some(MappedProfile {
        protocol: "s3".to_string(),
        provider_id: Some(provider_id),
        host,
        port,
        username: access_key,
        password: get("secret_key").map(|s| s.to_string()),
        options: if options.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(options))
        },
        initial_path: None,
    })
}

/// Stable profile display name: `host_base` host if present, else a generic
/// "s3cmd default". This keeps round-trips deterministic.
fn s3cmd_profile_name(section: &HashMap<String, String>) -> String {
    let host_base = section
        .get("host_base")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(crate::bridge_shared::endpoint_host);
    match host_base {
        Some(h) if !h.is_empty() => format!("s3cmd {h}"),
        _ => "s3cmd default".to_string(),
    }
}

// ============ Default config path detection ============

/// Returns the default `~/.s3cfg` path for the current platform.
///
/// Resolution chain:
/// 1. `S3CMD_CONFIG` env var (s3cmd itself only honors `-c <file>`, but the
///    env override is the natural bridge counterpart and the user can always
///    pass an explicit path argument to `import_s3cmd`).
/// 2. Per-OS default: `$HOME/.s3cfg` (unix/macOS),
///    `%USERPROFILE%\.s3cfg` (Windows).
pub fn default_s3cmd_config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("S3CMD_CONFIG") {
        if !path.is_empty() {
            let p = PathBuf::from(&path);
            if p.exists() {
                return Some(p);
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".s3cfg");
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(userprofile) = std::env::var("USERPROFILE") {
            let path = PathBuf::from(userprofile).join(".s3cfg");
            if path.exists() {
                return Some(path);
            }
        }
    }

    None
}

// ============ Public API ============

/// A profile that was skipped during s3cmd import.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct S3cmdSkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Result of importing an s3cmd `~/.s3cfg` file.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct S3cmdImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<S3cmdSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

/// Import the single s3cmd profile from a `~/.s3cfg` file.
pub fn import_s3cmd(path: &Path) -> Result<S3cmdImportResult, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read s3cmd config: {e}"))?;

    let sections = crate::bridge_shared::parse_ini_sections(&content);

    // s3cmd is mono-config: prefer [default], fall back to the only section
    // present so a hand-edited file with a renamed section still imports.
    let section = sections
        .get("default")
        .or_else(|| {
            if sections.len() == 1 {
                sections.values().next()
            } else {
                None
            }
        })
        .cloned();

    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    let total_remotes = if section.is_some() { 1 } else { 0 };

    if let Some(section) = section {
        let name = s3cmd_profile_name(&section);
        match map_s3cmd(&section) {
            Some(mapped) => {
                let id = format!(
                    "s3cmd-{}-{}",
                    name.to_lowercase().replace(' ', "-"),
                    &crate::bridge_shared::uuid_v4()[..8]
                );
                servers.push(ServerProfileExport {
                    id,
                    name,
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
            None => {
                skipped.push(S3cmdSkippedRemote {
                    name,
                    kind: "s3".to_string(),
                    reason: "no access_key".to_string(),
                });
            }
        }
    }

    Ok(S3cmdImportResult {
        servers,
        skipped,
        source_path: path.display().to_string(),
        total_remotes,
    })
}

// ============ Export to ~/.s3cfg ============

/// A server profile to export as an s3cmd `[default]` section.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S3cmdExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

/// Export the first S3 profile to a native s3cmd `~/.s3cfg` file.
///
/// s3cmd is mono-config: only one `[default]` section is emitted (the first
/// S3 profile in `servers`). The S3 secret is written in cleartext, exactly
/// as s3cmd does natively; the file is written `0600` via `atomic_write_600`.
pub fn export_s3cmd(
    servers: &[S3cmdExportServer],
    passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<usize, String> {
    let server = servers.iter().find(|s| s.protocol.as_deref() == Some("s3"));

    let Some(server) = server else {
        return Err("export s3cmd: no s3 profile to export".to_string());
    };

    let opts = server.options.as_ref().and_then(|v| v.as_object());
    let opt_str = |k: &str| {
        opts.and_then(|m| m.get(k))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let opt_bool = |k: &str| opts.and_then(|m| m.get(k)).and_then(|v| v.as_bool());

    let secret = passwords.get(&server.name).cloned().unwrap_or_default();
    let region = opt_str("region").unwrap_or_default();
    let host_bucket =
        opt_str("hostBucket").unwrap_or_else(|| "%(bucket)s.s3.amazonaws.com".to_string());

    // host_base: keep the explicit endpoint host[:port]. The s3cmd default
    // (empty host or AWS endpoint) is `s3.amazonaws.com`.
    let host = server.host.trim();
    let host_base = if host.is_empty() {
        "s3.amazonaws.com".to_string()
    } else if server.port == 443 || server.port == 80 {
        host.to_string()
    } else {
        format!("{host}:{}", server.port)
    };

    // use_https: prefer the preserved option, else derive from the port.
    let use_https = opt_bool("useHttps").unwrap_or(server.port != 80);

    let mut body = String::new();
    body.push_str("# Generated by AeroFTP - https://aeroftp.app\n");
    body.push_str(&format!(
        "# Exported: {}\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));
    body.push_str("[default]\n");
    body.push_str(&format!("access_key = {}\n", server.username));
    body.push_str(&format!("secret_key = {secret}\n"));
    body.push_str(&format!("host_base = {host_base}\n"));
    body.push_str(&format!("host_bucket = {host_bucket}\n"));
    body.push_str(&format!(
        "use_https = {}\n",
        if use_https { "True" } else { "False" }
    ));
    body.push_str(&format!("bucket_location = {region}\n"));

    crate::bridge_shared::atomic_write_600(out, body.as_bytes())?;
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_CUSTOM: &str = r#"
# s3cmd config
[default]
access_key = AKIAIOSFODNN7EXAMPLE
secret_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY
host_base = s3.lab.example.com:9000
host_bucket = %(bucket)s.s3.lab.example.com:9000
bucket_location = eu-west-1
use_https = False
gpg_passphrase = ignored-secret
"#;

    const FIXTURE_NO_ACCESS_KEY: &str = r#"
[default]
access_key =
secret_key = orphan-secret
host_base = s3.amazonaws.com
bucket_location = us-east-1
"#;

    #[test]
    fn test_parse_s3cmd_custom_endpoint() {
        let sections = crate::bridge_shared::parse_ini_sections(FIXTURE_CUSTOM);
        let section = sections.get("default").expect("default section present");
        let mapped = map_s3cmd(section).expect("should map s3cmd profile");

        assert_eq!(mapped.protocol, "s3");
        assert_eq!(mapped.host, "s3.lab.example.com");
        // explicit :9000 in host_base overrides the use_https default
        assert_eq!(mapped.port, 9000);
        assert_eq!(mapped.username, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(
            mapped.password.as_deref(),
            Some("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")
        );
        assert_eq!(mapped.provider_id.as_deref(), Some("custom-s3"));

        let opts = mapped.options.expect("options present");
        let obj = opts.as_object().unwrap();
        assert_eq!(
            obj.get("region").and_then(|v| v.as_str()),
            Some("eu-west-1")
        );
        assert_eq!(obj.get("useHttps").and_then(|v| v.as_bool()), Some(false));
    }

    #[test]
    fn test_import_s3cmd_skips_empty_access_key() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join("aeroftp-test-s3cmd-noaccess.cfg");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(FIXTURE_NO_ACCESS_KEY.as_bytes()).unwrap();
        }
        let result = import_s3cmd(&tmp).expect("should parse");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(result.total_remotes, 1);
        assert_eq!(result.servers.len(), 0);
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].kind, "s3");
        assert_eq!(result.skipped[0].reason, "no access_key");
    }

    #[test]
    fn test_secret_policy_file_plain_populates_credential() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join("aeroftp-test-s3cmd-secret.cfg");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(FIXTURE_CUSTOM.as_bytes()).unwrap();
        }
        let result = import_s3cmd(&tmp).expect("should parse");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(result.servers.len(), 1);
        let s = &result.servers[0];
        // s3cmd stores the secret plain in the file -> credential populated.
        assert_eq!(
            s.credential.as_deref(),
            Some("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")
        );
        // gpg_passphrase must be ignored, not surfaced as a credential.
        assert!(s.credential.as_deref() != Some("ignored-secret"));
        assert_eq!(s.has_stored_credential, None);
    }

    #[test]
    fn test_roundtrip_metadata_idempotence() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join("aeroftp-test-s3cmd-rt-in.cfg");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(FIXTURE_CUSTOM.as_bytes()).unwrap();
        }
        let first = import_s3cmd(&tmp).expect("import 1");
        std::fs::remove_file(&tmp).ok();
        assert_eq!(first.servers.len(), 1);
        let s1 = &first.servers[0];

        let export_servers = vec![S3cmdExportServer {
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
        passwords.insert(s1.name.clone(), s1.credential.clone().unwrap_or_default());

        let out = std::env::temp_dir().join("aeroftp-test-s3cmd-rt-out.cfg");
        let exported = export_s3cmd(&export_servers, &passwords, &out).expect("export");
        assert_eq!(exported, 1);

        let second = import_s3cmd(&out).expect("import 2");
        std::fs::remove_file(&out).ok();
        assert_eq!(second.servers.len(), 1);
        let s2 = &second.servers[0];

        // Metadata idempotence across import -> export -> import.
        assert_eq!(s1.host, s2.host);
        assert_eq!(s1.port, s2.port);
        assert_eq!(s1.username, s2.username);
        assert_eq!(s1.protocol, s2.protocol);
        assert_eq!(s1.provider_id, s2.provider_id);
        assert_eq!(s1.credential, s2.credential);

        let r1 = s1
            .options
            .as_ref()
            .and_then(|v| v.get("region"))
            .and_then(|v| v.as_str());
        let r2 = s2
            .options
            .as_ref()
            .and_then(|v| v.get("region"))
            .and_then(|v| v.as_str());
        assert_eq!(r1, r2);
        assert_eq!(r1, Some("eu-west-1"));
    }

    #[test]
    fn test_default_config_path_env_override() {
        use std::io::Write;

        let prev_s3cmd = std::env::var("S3CMD_CONFIG").ok();

        let dir = std::env::temp_dir().join(format!(
            "s3cmd-cfg-{}",
            &crate::bridge_shared::uuid_v4()[..8]
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("custom.s3cfg");
        {
            let mut f = std::fs::File::create(&cfg).unwrap();
            f.write_all(b"[default]\naccess_key = AK\n").unwrap();
        }

        std::env::set_var("S3CMD_CONFIG", &cfg);
        let resolved = default_s3cmd_config_path();
        assert_eq!(resolved.as_deref(), Some(cfg.as_path()));

        // restore env
        match prev_s3cmd {
            Some(v) => std::env::set_var("S3CMD_CONFIG", v),
            None => std::env::remove_var("S3CMD_CONFIG"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
