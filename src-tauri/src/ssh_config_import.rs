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
    let value = v.trim().trim_start_matches('=').trim();
    // A value in double quotes, as ssh reads it and as the export writes a
    // key file path with spaces: the quotes are not part of it.
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    Some((k.to_lowercase(), value.to_string()))
}

/// Maximum number of `Host` blocks to parse, matching the other importers:
/// each imported profile with a credential costs a whole-vault rewrite, so an
/// uncapped parse of a large config is a self-inflicted write storm.
const MAX_HOSTS: usize = 10_000;

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
            if blocks.len() >= MAX_HOSTS {
                log::warn!("ssh config import: stopped at the {MAX_HOSTS}-host cap");
                return blocks;
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
    /// The profile's id: the key of its secret in the passwords map
    /// ([`crate::bridge_shared::export_secret`]).
    #[serde(default)]
    pub id: String,
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
/// ssh `Host` patterns are whitespace separated, so an alias with a space
/// would silently become two patterns. Collapse to a safe token.
fn ssh_host_alias(name: &str) -> String {
    name.trim().replace(char::is_whitespace, "-")
}

/// One word of an ssh_config line (`HostName`, `User`, `ProxyJump`): a line
/// break would start keywords of its own, a `ProxyCommand` or a
/// `LocalCommand` ssh runs among them, and whitespace would end the value
/// early, so the rest is read as more arguments.
fn ssh_word<'a>(field: &str, value: &'a str) -> Result<&'a str, String> {
    crate::bridge_shared::export_line_value(field, value)?;
    if value.contains(char::is_whitespace) || value.contains('"') {
        return Err(format!(
            "its {field} holds whitespace or a double quote, which ssh would read as more than one value; correct the profile and export again"
        ));
    }
    Ok(value)
}

/// An `IdentityFile` path as ssh_config reads it: one with whitespace is
/// written in double quotes (a path may hold spaces), and one with a double
/// quote cannot be written at all, since the format has no escape for it.
fn ssh_path(field: &str, value: &str) -> Result<String, String> {
    crate::bridge_shared::export_line_value(field, value)?;
    if value.contains('"') {
        return Err(format!(
            "its {field} holds a double quote, which ssh_config cannot write inside a value; rename the file and export again"
        ));
    }
    Ok(if value.contains(char::is_whitespace) {
        format!("\"{value}\"")
    } else {
        value.to_string()
    })
}

/// The lines of `server`'s `Host` block under its `Host` line, or why they
/// cannot be written. Every string is checked before any of it is written.
fn ssh_host_lines(server: &SshExportServer) -> Result<String, String> {
    let raw = |key: &str| {
        server
            .options
            .as_ref()
            .and_then(|o| o.get(key))
            .and_then(|v| v.as_str())
    };
    // The values as the profile holds them, before the trim below: a line
    // break at either end would be trimmed away, and the profile exported
    // with another value than it holds.
    for (field, value) in [
        ("host", Some(server.host.as_str())),
        ("user name", Some(server.username.as_str())),
        ("key file path", raw("private_key_path")),
        ("jump host", raw("proxyJump")),
    ] {
        crate::bridge_shared::export_line_value(field, value.unwrap_or(""))?;
    }
    let opt = |key: &str| raw(key).map(str::trim).filter(|s| !s.is_empty());
    let host = Some(server.host.trim()).filter(|s| !s.is_empty());
    let user = Some(server.username.trim()).filter(|s| !s.is_empty());
    let host = host.map(|h| ssh_word("host", h)).transpose()?;
    let user = user.map(|u| ssh_word("user name", u)).transpose()?;
    let key_file = opt("private_key_path")
        .map(|k| ssh_path("key file path", k))
        .transpose()?;
    let proxy = opt("proxyJump")
        .map(|p| ssh_word("jump host", p))
        .transpose()?;

    let mut block = String::new();
    if let Some(host) = host {
        block.push_str(&format!("  HostName {host}\n"));
    }
    block.push_str(&format!("  Port {}\n", server.port));
    if let Some(user) = user {
        block.push_str(&format!("  User {user}\n"));
    }
    if let Some(key_file) = key_file {
        block.push_str(&format!("  IdentityFile {key_file}\n"));
    }
    if let Some(proxy) = proxy {
        block.push_str(&format!("  ProxyJump {proxy}\n"));
    }
    block.push('\n');
    Ok(block)
}

pub fn export_ssh_config(
    servers: &[SshExportServer],
    _passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<crate::bridge_shared::BridgeExportOutcome, String> {
    let mut output = String::new();
    output.push_str("# Generated by AeroFTP - https://aeroftp.app\n");
    output.push_str(&format!(
        "# Exported: {}\n\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));

    let mut outcome = crate::bridge_shared::BridgeExportOutcome::default();
    // ssh takes the first `Host` that matches: a second one of an alias is
    // never reached by it.
    let mut names = crate::bridge_shared::ExportEntryNames::exact(
        servers.iter().map(|s| ssh_host_alias(&s.name)),
    );

    for server in servers {
        if server.protocol.as_deref() != Some("sftp") {
            continue;
        }
        let safe_alias = ssh_host_alias(&server.name);
        // The alias turns whitespace into `-`, line breaks included; any
        // other control character would still reach the `Host` line. A name
        // that is only whitespace or a line break has no alias at all.
        let lines = crate::bridge_shared::export_line_value("name", &server.name)
            .and_then(|()| {
                if safe_alias.is_empty() {
                    Err("its name is empty, and ssh finds a host only by its name; name the profile and export again".to_string())
                } else {
                    Ok(())
                }
            })
            .and_then(|()| ssh_host_lines(server));
        match lines {
            Ok(lines) => {
                output.push_str(&format!("Host {}\n{lines}", names.claim(&safe_alias)));
                outcome.exported += 1;
            }
            Err(reason) => outcome.skip(&server.name, reason),
        }
    }
    crate::bridge_shared::atomic_write_600(out, output.as_bytes())?;
    Ok(outcome)
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
            id: s.id.clone(),
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
        assert_eq!(exported.exported, 1);

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
            let _env = crate::test_env::lock();
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
                id: "ftp-box".to_string(),
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
                id: "ssh-box".to_string(),
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
        let n = export_ssh_config(&servers, &HashMap::new(), &tmp)
            .expect("export")
            .exported;
        let conf = std::fs::read_to_string(&tmp).unwrap();
        std::fs::remove_file(&tmp).ok();

        assert_eq!(n, 1);
        assert!(conf.contains("Host ssh-box"));
        assert!(!conf.contains("ftp-box"));
        // No secret material ever written.
        assert!(!conf.to_lowercase().contains("password"));
    }

    /// Two profiles of one name (a list saved before names were unique) were
    /// two `Host Lab` blocks, and ssh takes the first `Host` that matches:
    /// the second host was not reachable by any alias.
    #[test]
    fn two_profiles_of_one_name_export_as_two_hosts() {
        let (servers, passwords) = crate::bridge_shared::two_profiles_named_lab::<SshExportServer>(
            serde_json::json!({"host": "a.example.com", "port": 22, "username": "u", "protocol": "sftp"}),
            serde_json::json!({"host": "b.example.com", "port": 22, "username": "u", "protocol": "sftp"}),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        assert_eq!(
            export_ssh_config(&servers, &passwords, &path)
                .unwrap()
                .exported,
            2
        );
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("Host Lab\n  HostName a.example.com\n"),
            "{body}"
        );
        assert!(
            body.contains("Host Lab-2\n  HostName b.example.com\n"),
            "{body}"
        );
    }

    /// A line break in a value started keywords of its own,
    /// and ssh runs a `ProxyCommand` or a `LocalCommand` as soon as the alias
    /// is used. Each such profile is refused by name and field; a key path
    /// with spaces is quoted, and a non-ASCII name still becomes its alias.
    #[test]
    fn a_profile_whose_value_breaks_its_line_is_refused() {
        let payload = "ProxyCommand /tmp/run-me";
        let profile =
            |name: &str, host: &str, user: &str, options: serde_json::Value| -> SshExportServer {
                serde_json::from_value(serde_json::json!({
                    "id": format!("id-{name}"), "name": name, "host": host, "port": 22,
                    "username": user, "protocol": "sftp", "options": options,
                }))
                .unwrap()
            };
        let none = serde_json::json!({});
        for brk in crate::bridge_shared::LINE_BREAKS {
            let hostile = format!("x{brk}  {payload}");
            let servers = vec![
                profile(
                    &format!("name\u{1b}{brk}"),
                    "h.example.com",
                    "u",
                    none.clone(),
                ),
                profile("host", &hostile, "u", none.clone()),
                profile("user", "h.example.com", &hostile, none.clone()),
                profile(
                    "key",
                    "h.example.com",
                    "u",
                    serde_json::json!({"private_key_path": hostile}),
                ),
                profile(
                    "jump",
                    "h.example.com",
                    "u",
                    serde_json::json!({"proxyJump": hostile}),
                ),
                profile(
                    "Büro nas",
                    "nas.example.com",
                    "élodie",
                    serde_json::json!({"private_key_path": "/home/élodie/My Keys/id_ed25519"}),
                ),
            ];
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config");
            let outcome = export_ssh_config(&servers, &HashMap::new(), &path).unwrap();
            assert_eq!(outcome.exported, 1, "{brk:?}: {:?}", outcome.skipped);
            let fields = ["name", "host", "user name", "key file path", "jump host"];
            assert_eq!(outcome.skipped.len(), fields.len(), "{:?}", outcome.skipped);
            for (skip, (server, field)) in outcome.skipped.iter().zip(servers.iter().zip(fields)) {
                assert_eq!(skip.name, server.name);
                assert!(
                    skip.reason.starts_with(&format!("its {field} ")),
                    "{}",
                    skip.reason
                );
                assert!(!skip.reason.contains(payload));
            }
            let body = std::fs::read_to_string(&path).unwrap();
            crate::bridge_shared::assert_no_injected_line(&body, payload);
            assert!(
                body.contains("Host Büro-nas\n  HostName nas.example.com\n  Port 22\n  User élodie\n  IdentityFile \"/home/élodie/My Keys/id_ed25519\"\n"),
                "{body}"
            );
        }
        // A space ends the value as surely: refused, not cut.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let spaced = [profile("spaced", "h.example.com ProxyCommand", "u", none)];
        let outcome = export_ssh_config(&spaced, &HashMap::new(), &path).unwrap();
        assert_eq!(outcome.exported, 0);
        assert!(
            outcome.skipped[0]
                .reason
                .starts_with("its host holds whitespace"),
            "{:?}",
            outcome.skipped
        );
    }

    fn sftp(name: &str, host: &str, user: &str, options: serde_json::Value) -> SshExportServer {
        serde_json::from_value(serde_json::json!({
            "id": format!("id-{name}"), "name": name, "host": host, "port": 22,
            "username": user, "protocol": "sftp", "options": options,
        }))
        .unwrap()
    }

    /// A key file path with spaces is written in double quotes, read back
    /// without them, and exported again as the same path.
    #[test]
    fn a_quoted_key_path_survives_export_import_and_export_again() {
        let path = "/home/u/My Keys/id_ed25519";
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let servers = [sftp(
            "lab",
            "h.example.com",
            "u",
            serde_json::json!({"private_key_path": path}),
        )];
        assert_eq!(
            export_ssh_config(&servers, &HashMap::new(), &first)
                .unwrap()
                .exported,
            1
        );
        let imported = import_ssh_config(&first).unwrap();
        assert_eq!(imported.servers.len(), 1);
        let key = imported.servers[0]
            .options
            .as_ref()
            .and_then(|o| o.get("private_key_path"))
            .and_then(|v| v.as_str());
        assert_eq!(key, Some(path), "{:?}", imported.servers[0].options);
        let again = [sftp(
            "lab",
            "h.example.com",
            "u",
            serde_json::json!({"private_key_path": key.unwrap()}),
        )];
        let second = dir.path().join("second");
        let outcome = export_ssh_config(&again, &HashMap::new(), &second).unwrap();
        assert_eq!(outcome.exported, 1, "{:?}", outcome.skipped);
        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            std::fs::read_to_string(&second).unwrap()
        );
    }

    /// A line break at either end of a value is refused like one inside it
    /// (the trim must not hide it), a tab in a host or user name splits the
    /// value as a space would, and a name that is only whitespace or a line
    /// break is reported instead of dropped.
    #[test]
    fn a_break_at_the_edge_a_tab_or_an_empty_name_is_reported() {
        let none = serde_json::json!({});
        let servers = [
            sftp("lead", "\nh.example.com", "u", none.clone()),
            sftp("trail", "h.example.com", "u\r", none.clone()),
            sftp(
                "key",
                "h.example.com",
                "u",
                serde_json::json!({"private_key_path": "/keys/id\n"}),
            ),
            sftp(
                "jump",
                "h.example.com",
                "u",
                serde_json::json!({"proxyJump": "\r\nbastion"}),
            ),
            sftp("tab", "h.example.com", "a\tb", none.clone()),
            sftp("\n", "h.example.com", "u", none.clone()),
            sftp("   ", "h.example.com", "u", none.clone()),
            sftp("good", "h.example.com", "u", none.clone()),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let outcome = export_ssh_config(&servers, &HashMap::new(), &path).unwrap();
        assert_eq!(outcome.exported, 1, "{:?}", outcome.skipped);
        let want = [
            ("lead", "its host holds a line break"),
            ("trail", "its user name holds a line break"),
            ("key", "its key file path holds a line break"),
            ("jump", "its jump host holds a line break"),
            ("tab", "its user name holds whitespace"),
            ("\n", "its name holds a line break"),
            ("   ", "its name is empty"),
        ];
        assert_eq!(outcome.skipped.len(), want.len(), "{:?}", outcome.skipped);
        for (skip, (name, reason)) in outcome.skipped.iter().zip(want) {
            assert_eq!(skip.name, name);
            assert!(skip.reason.starts_with(reason), "{name:?}: {}", skip.reason);
        }
        assert_eq!(
            std::fs::read_to_string(&path)
                .unwrap()
                .matches("Host ")
                .count(),
            1
        );
    }
}
