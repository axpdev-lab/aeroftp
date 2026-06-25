// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from an OpenSSH client config (`~/.ssh/config`).
//!
//! Parses `Host` blocks (HostName / Port / User / IdentityFile / ProxyJump),
//! maps each concrete alias to an AeroFTP `sftp` profile, and references the
//! private key by path only. Wildcard host patterns (`Host *`, any alias with
//! `*` or `?`) are skipped because they are match templates, not endpoints.
//!
//! Secret policy for this source:
//!
//! | Source state                | credential | has_stored_credential | note                       |
//! |-----------------------------|------------|-----------------------|----------------------------|
//! | SSH key referenced (file)   | None       | Some(false)           | options.private_key_path   |
//! | no key (agent / password)   | None       | Some(false)           | re-enter or use ssh-agent  |
//!
//! No key bytes are ever read or copied: only the (tilde-expanded) file path
//! is recorded. The ssh config never carries a secret, and the exporter never
//! writes one.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ Tilde expansion ============

/// Expand a leading `~/` against `HOME` (unix/macOS) or `USERPROFILE`
/// (Windows). Any other form (absolute path, bare `~user`) is returned
/// unchanged: ssh itself only special-cases `~/` here.
fn shellexpand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(h) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            let h = h.trim_end_matches('/');
            return format!("{h}/{rest}");
        }
    }
    p.to_string()
}

/// `true` if an alias is an ssh match pattern rather than a concrete host.
fn is_wildcard_pattern(alias: &str) -> bool {
    alias.contains('*') || alias.contains('?')
}

// ============ Default config path detection ============

/// Returns the default OpenSSH client config path for the current platform.
///
/// ssh has no standard environment variable for the user config location
/// (it is always `~/.ssh/config` unless overridden per-invocation with
/// `ssh -F`), so detection is purely the per-OS default. Callers that need
/// a different file pass it explicitly to [`import_ssh_config`].
pub fn default_ssh_config_path() -> Option<PathBuf> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".ssh").join("config");
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            let path = PathBuf::from(profile).join(".ssh").join("config");
            if path.exists() {
                return Some(path);
            }
        }
    }

    None
}

// ============ Parser ============

/// One parsed `Host` block: the alias list plus its lowercased key/value
/// directives. `Host` may name several patterns; the first concrete token
/// is used as the profile name.
struct HostBlock {
    aliases: Vec<String>,
    kv: HashMap<String, String>,
}

/// Split an ssh config directive line into `(lowercased key, value)`.
///
/// ssh accepts either `Key value` or `Key = value`; tokens are separated by
/// whitespace and the optional `=` is stripped.
fn split_directive(line: &str) -> Option<(String, String)> {
    let (k, v) = line.split_once(char::is_whitespace)?;
    let value = v.trim().trim_start_matches('=').trim().to_string();
    Some((k.to_lowercase(), value))
}

/// Parse the ssh config into ordered `Host` blocks. Comments (`#`) and blank
/// lines are ignored. Directives before the first `Host` (rare global block)
/// are discarded since they cannot produce a concrete profile.
fn parse_ssh_config(content: &str) -> Vec<HostBlock> {
    let mut blocks: Vec<HostBlock> = Vec::new();
    let mut current: Option<HostBlock> = None;

    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = split_directive(line) else {
            continue;
        };

        if key == "host" {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            let aliases: Vec<String> = value
                .split_whitespace()
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .collect();
            current = Some(HostBlock {
                aliases,
                kv: HashMap::new(),
            });
        } else if let Some(block) = current.as_mut() {
            // First occurrence wins, matching ssh's own precedence.
            block.kv.entry(key).or_insert(value);
        }
    }

    if let Some(block) = current.take() {
        blocks.push(block);
    }

    blocks
}

/// Convert one `Host` block to a profile, or `None` (with a skip reason) when
/// it is a wildcard pattern or has no usable alias.
fn map_host_block(block: &HostBlock) -> Result<ServerProfileExport, String> {
    let alias = block
        .aliases
        .iter()
        .find(|a| !a.is_empty())
        .ok_or_else(|| "empty host alias".to_string())?
        .clone();

    if block.aliases.iter().any(|a| is_wildcard_pattern(a)) {
        return Err("wildcard host pattern".to_string());
    }

    let kv = &block.kv;
    let host = kv
        .get("hostname")
        .cloned()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| alias.clone());
    let port = kv
        .get("port")
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(22);
    let username = kv.get("user").cloned().unwrap_or_default();

    let mut options = serde_json::Map::new();
    if let Some(identity) = kv.get("identityfile").filter(|s| !s.is_empty()) {
        options.insert(
            "private_key_path".into(),
            serde_json::Value::String(shellexpand_tilde(identity)),
        );
    }
    if let Some(proxy) = kv.get("proxyjump").filter(|s| !s.is_empty()) {
        options.insert("proxyJump".into(), serde_json::Value::String(proxy.clone()));
    }

    Ok(ServerProfileExport {
        id: format!(
            "ssh-{}-{}",
            alias.to_lowercase().replace(' ', "-"),
            &crate::bridge_shared::uuid_v4()[..8]
        ),
        name: alias,
        host,
        port,
        username,
        protocol: Some("sftp".to_string()),
        initial_path: None,
        local_initial_path: None,
        color: None,
        last_connected: None,
        options: if options.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(options))
        },
        provider_id: None,
        // The key lives on disk and is referenced by path only: no secret is
        // ever transported through the ssh config.
        credential: None,
        has_stored_credential: Some(false),
        public_url_base: None,
        ..Default::default()
    })
}

// ============ Public API ============

/// A `Host` block that was skipped (wildcard pattern or unusable alias).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshSkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Result of importing an ssh config.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<SshSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

/// Import all concrete `Host` blocks from an OpenSSH client config file.
pub fn import_ssh_config(path: &Path) -> Result<SshImportResult, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read ssh config: {e}"))?;

    let blocks = parse_ssh_config(&content);
    let total_remotes = blocks.len();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    for block in &blocks {
        let display_name = block.aliases.first().cloned().unwrap_or_default();
        match map_host_block(block) {
            Ok(profile) => servers.push(profile),
            Err(reason) => skipped.push(SshSkippedRemote {
                name: display_name,
                kind: "host".to_string(),
                reason,
            }),
        }
    }

    Ok(SshImportResult {
        servers,
        skipped,
        source_path: path.display().to_string(),
        total_remotes,
    })
}

// ============ Export to ssh config ============

/// A server profile to export as an ssh config `Host` block.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

/// Export `sftp` profiles as OpenSSH `Host` blocks.
///
/// Only `sftp` profiles map onto ssh; every other protocol is skipped. The
/// ssh config format has no password field, so the `passwords` map is
/// intentionally unused: secrets are never written. An `IdentityFile` line is
/// emitted only when the profile already references a key by path.
pub fn export_ssh_config(
    servers: &[SshExportServer],
    _passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<usize, String> {
    let mut output = String::new();
    output.push_str("# Generated by AeroFTP - https://aeroftp.app\n");
    output.push_str(&format!(
        "# Exported: {}\n\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));

    let mut exported = 0;

    for server in servers {
        if server.protocol.as_deref() != Some("sftp") {
            continue;
        }
        let alias = server.name.trim();
        if alias.is_empty() {
            continue;
        }
        // ssh `Host` patterns are whitespace separated, so an alias with a
        // space would silently become two patterns. Collapse to a safe token.
        let safe_alias = alias.replace(char::is_whitespace, "-");

        output.push_str(&format!("Host {}\n", safe_alias));
        if !server.host.trim().is_empty() {
            output.push_str(&format!("  HostName {}\n", server.host.trim()));
        }
        output.push_str(&format!("  Port {}\n", server.port));
        if !server.username.trim().is_empty() {
            output.push_str(&format!("  User {}\n", server.username.trim()));
        }
        if let Some(opts) = server.options.as_ref() {
            if let Some(key_file) = opts
                .get("private_key_path")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                output.push_str(&format!("  IdentityFile {}\n", key_file));
            }
            if let Some(proxy) = opts
                .get("proxyJump")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                output.push_str(&format!("  ProxyJump {}\n", proxy));
            }
        }
        output.push('\n');
        exported += 1;
    }

    crate::bridge_shared::atomic_write_600(out, output.as_bytes())?;
    Ok(exported)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
# personal hosts
Host basic
    HostName 192.168.1.50

Host nas
    HostName nas.example.com
    Port 2222
    User admin
    IdentityFile ~/.ssh/id_ed25519_nas

Host *
    ServerAliveInterval 60

Host web-?
    HostName web.example.com
    User deploy
"#;

    #[test]
    fn test_parse_ssh_config_fixture() {
        let blocks = parse_ssh_config(FIXTURE);
        // basic, nas, Host *, web-?
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0].aliases, vec!["basic".to_string()]);
        assert_eq!(blocks[1].kv.get("port").map(String::as_str), Some("2222"));
        assert_eq!(blocks[1].kv.get("user").map(String::as_str), Some("admin"));
        assert_eq!(
            blocks[1].kv.get("identityfile").map(String::as_str),
            Some("~/.ssh/id_ed25519_nas")
        );
        assert_eq!(blocks[2].aliases, vec!["*".to_string()]);
    }

    #[test]
    fn test_import_skips_wildcards() {
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-ssh-{}.cfg",
            crate::bridge_shared::uuid_v4()
        ));
        std::fs::write(&tmp, FIXTURE).unwrap();
        let result = import_ssh_config(&tmp).expect("should parse");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(result.total_remotes, 4);
        // `Host *` and `web-?` are skipped wildcard patterns.
        assert_eq!(result.servers.len(), 2);
        assert_eq!(result.skipped.len(), 2);
        for s in &result.skipped {
            assert_eq!(s.reason, "wildcard host pattern");
            assert_eq!(s.kind, "host");
        }

        let basic = result.servers.iter().find(|s| s.name == "basic").unwrap();
        assert_eq!(basic.host, "192.168.1.50");
        assert_eq!(basic.port, 22); // default
        assert_eq!(basic.protocol.as_deref(), Some("sftp"));

        let nas = result.servers.iter().find(|s| s.name == "nas").unwrap();
        assert_eq!(nas.host, "nas.example.com");
        assert_eq!(nas.port, 2222);
        assert_eq!(nas.username, "admin");
    }

    #[test]
    fn test_host_without_hostname_uses_alias() {
        let blocks = parse_ssh_config("Host fallbackhost\n  User bob\n");
        let profile = map_host_block(&blocks[0]).expect("should map");
        assert_eq!(profile.host, "fallbackhost");
        assert_eq!(profile.username, "bob");
    }

    #[test]
    fn test_secret_policy_never_credential() {
        let blocks = parse_ssh_config(
            "Host keyed\n  HostName h.example.com\n  IdentityFile ~/.ssh/id_rsa\n",
        );
        let profile = map_host_block(&blocks[0]).expect("should map");
        // Key referenced by path only: never a credential.
        assert!(profile.credential.is_none());
        assert_eq!(profile.has_stored_credential, Some(false));
        let opts = profile.options.expect("options should exist");
        let key = opts
            .get("private_key_path")
            .and_then(|v| v.as_str())
            .unwrap();
        // Tilde-expanded, no key bytes.
        assert!(!key.starts_with("~/"));
        assert!(key.ends_with("/.ssh/id_rsa"));
    }

    #[test]
    fn test_roundtrip_metadata_idempotence() {
        let original = r#"
Host roundtrip
    HostName rt.example.com
    Port 2200
    User carol
    IdentityFile /home/carol/.ssh/id_rt
"#;
        let tmp_in = std::env::temp_dir().join(format!(
            "aeroftp-ssh-rt-{}.cfg",
            crate::bridge_shared::uuid_v4()
        ));
        std::fs::write(&tmp_in, original).unwrap();
        let imported = import_ssh_config(&tmp_in).expect("import");
        std::fs::remove_file(&tmp_in).ok();
        assert_eq!(imported.servers.len(), 1);

        let s = &imported.servers[0];
        let export_servers = vec![SshExportServer {
            name: s.name.clone(),
            host: s.host.clone(),
            port: s.port,
            username: s.username.clone(),
            protocol: s.protocol.clone(),
            options: s.options.clone(),
            provider_id: s.provider_id.clone(),
            initial_path: s.initial_path.clone(),
        }];

        let tmp_out = std::env::temp_dir().join(format!(
            "aeroftp-ssh-rt-out-{}.cfg",
            crate::bridge_shared::uuid_v4()
        ));
        let exported =
            export_ssh_config(&export_servers, &HashMap::new(), &tmp_out).expect("export");
        assert_eq!(exported, 1);

        let reimported = import_ssh_config(&tmp_out).expect("reimport");
        std::fs::remove_file(&tmp_out).ok();
        assert_eq!(reimported.servers.len(), 1);

        let r = &reimported.servers[0];
        assert_eq!(r.name, "roundtrip");
        assert_eq!(r.host, "rt.example.com");
        assert_eq!(r.port, 2200);
        assert_eq!(r.username, "carol");
        assert_eq!(r.protocol.as_deref(), Some("sftp"));
        // private_key_path metadata preserved across the round trip.
        let key = r
            .options
            .as_ref()
            .and_then(|o| o.get("private_key_path"))
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(key, "/home/carol/.ssh/id_rt");
        // Still no secret.
        assert!(r.credential.is_none());
        assert_eq!(r.has_stored_credential, Some(false));
    }

    #[test]
    fn test_default_path_env_override() {
        // ssh has no dedicated env var, so the override surface is HOME
        // (unix/macOS) pointing at a temp dir that contains .ssh/config.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let prev = std::env::var("HOME").ok();
            let dir = std::env::temp_dir().join(format!(
                "aeroftp-ssh-home-{}",
                crate::bridge_shared::uuid_v4()
            ));
            std::fs::create_dir_all(dir.join(".ssh")).unwrap();
            std::fs::write(dir.join(".ssh").join("config"), "Host x\n").unwrap();

            std::env::set_var("HOME", &dir);
            let detected = default_ssh_config_path();
            assert_eq!(detected, Some(dir.join(".ssh").join("config")));

            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn test_export_skips_non_sftp() {
        let servers = vec![
            SshExportServer {
                name: "ftp-box".to_string(),
                host: "ftp.example.com".to_string(),
                port: 21,
                username: "u".to_string(),
                protocol: Some("ftp".to_string()),
                options: None,
                provider_id: None,
                initial_path: None,
            },
            SshExportServer {
                name: "ssh-box".to_string(),
                host: "ssh.example.com".to_string(),
                port: 22,
                username: "u".to_string(),
                protocol: Some("sftp".to_string()),
                options: None,
                provider_id: None,
                initial_path: None,
            },
        ];
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-ssh-exp-{}.cfg",
            crate::bridge_shared::uuid_v4()
        ));
        let n = export_ssh_config(&servers, &HashMap::new(), &tmp).expect("export");
        let conf = std::fs::read_to_string(&tmp).unwrap();
        std::fs::remove_file(&tmp).ok();

        assert_eq!(n, 1);
        assert!(conf.contains("Host ssh-box"));
        assert!(!conf.contains("ftp-box"));
        // No secret material ever written.
        assert!(!conf.to_lowercase().contains("password"));
    }
}
