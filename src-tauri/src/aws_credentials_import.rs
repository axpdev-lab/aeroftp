// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from the AWS shared credentials/config files.
//!
//! Source format is INI. `~/.aws/credentials` holds `[name]` sections with
//! `aws_access_key_id`, `aws_secret_access_key`, `aws_session_token`. The
//! sibling `~/.aws/config` holds `[profile name]` sections with `region`
//! and `endpoint_url`; it AUGMENTS the credentials sections (config keys are
//! added to the matching credentials section, never replacing access keys).
//!
//! The access key id and secret access key live in the file in plain text,
//! so the bridge transports them and re-encrypts them into the AeroFTP
//! AES-256-GCM vault on import (a security upgrade over the plain INI file).
//!
//! Secret policy (APPENDIX-BRIDGE section 1):
//!
//! | Case                          | Policy here                                  |
//! |-------------------------------|----------------------------------------------|
//! | Secret plain in file          | populate `credential` (this module's case)   |
//! | Secret obfuscated reversible  | n/a (AWS files are plain)                    |
//! | Secret in OS keychain         | n/a                                          |
//! | OAuth token                   | n/a (AWS uses static keys / STS tokens)      |
//! | SSH key referenced            | n/a                                          |
//!
//! A section without `aws_access_key_id` cannot authenticate, so it is
//! skipped with reason "no aws_access_key_id" (this also drops a bare
//! `[profile x]` config-only section that was never present in credentials).

use crate::profile_export::ServerProfileExport;
use std::path::{Path, PathBuf};

// ============ Default config path detection ============

/// Returns the default `~/.aws/credentials` path for the current platform.
///
/// Chain: the `AWS_SHARED_CREDENTIALS_FILE` env var, then the per-OS default
/// (`$HOME/.aws/credentials` on unix/macOS, `%USERPROFILE%\.aws\credentials`
/// on Windows). Mirrors the cfg-gated shape of
/// `rclone_import::default_rclone_config_path`.
pub fn default_aws_credentials_config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("AWS_SHARED_CREDENTIALS_FILE") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                return Some(PathBuf::from(home).join(".aws").join("credentials"));
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            if !profile.is_empty() {
                return Some(PathBuf::from(profile).join(".aws").join("credentials"));
            }
        }
    }

    None
}

/// Resolve the `~/.aws/config` path that augments a given credentials path.
///
/// Chain: the `AWS_CONFIG_FILE` env var, then a `config` file sitting next
/// to the credentials file (AWS keeps both inside `~/.aws/`).
fn aws_config_path_for(cred_path: &Path) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("AWS_CONFIG_FILE") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    Some(cred_path.with_file_name("config"))
}

// ============ Public API ============

/// A profile that was skipped (no usable access key id).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AwsSkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Result of importing the AWS shared credentials/config files.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AwsImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<AwsSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

/// Import all usable profiles from `~/.aws/credentials`, augmented by the
/// resolved sibling `~/.aws/config` (env `AWS_CONFIG_FILE` respected).
pub fn import_aws_credentials(cred_path: &Path) -> Result<AwsImportResult, String> {
    let config_path = aws_config_path_for(cred_path);
    import_aws_credentials_with_config(cred_path, config_path.as_deref())
}

/// Import with an explicit (optional) config path. Kept separate for
/// testability: `import_aws_credentials` resolves the sibling/env config,
/// while tests inject a known config path or `None`.
pub fn import_aws_credentials_with_config(
    cred_path: &Path,
    config_path: Option<&Path>,
) -> Result<AwsImportResult, String> {
    let cred =
        std::fs::read_to_string(cred_path).map_err(|e| format!("read aws credentials: {e}"))?;
    let mut sections = crate::bridge_shared::parse_ini_sections(&cred);

    if let Some(cp) = config_path {
        if let Ok(cfg) = std::fs::read_to_string(cp) {
            // `[profile x]` collapses to `x` inside parse_ini_sections, so
            // config keys land in the matching credentials section. Config
            // augments credentials: it never replaces an existing key.
            for (name, kv) in crate::bridge_shared::parse_ini_sections(&cfg) {
                let dest = sections.entry(name).or_default();
                for (k, v) in kv {
                    dest.entry(k).or_insert(v);
                }
            }
        }
    }

    let total_remotes = sections.len();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    // Stable ordering so the output is deterministic across runs.
    let mut names: Vec<&String> = sections.keys().collect();
    names.sort();

    for name in names {
        let kv = &sections[name];

        let akid = match kv.get("aws_access_key_id").filter(|s| !s.is_empty()) {
            Some(v) => v.clone(),
            None => {
                skipped.push(AwsSkippedRemote {
                    name: name.clone(),
                    kind: "aws".to_string(),
                    reason: "no aws_access_key_id".to_string(),
                });
                continue;
            }
        };

        let (host, port, provider_id) = match kv.get("endpoint_url").filter(|s| !s.is_empty()) {
            Some(ep) => {
                let host_port = crate::bridge_shared::endpoint_host(ep);
                let port = match host_port.rsplit_once(':') {
                    Some((_, p)) => p
                        .parse::<u32>()
                        .unwrap_or_else(|_| crate::bridge_shared::default_port_for("s3")),
                    None => crate::bridge_shared::default_port_for("s3"),
                };
                let pid = crate::bridge_shared::map_s3_provider_from_endpoint(ep);
                (host_port, port, pid.to_string())
            }
            None => (
                String::new(),
                crate::bridge_shared::default_port_for("s3"),
                "amazon-s3".to_string(),
            ),
        };

        let options = crate::bridge_shared::json_map(&[
            ("region", kv.get("region").cloned()),
            ("sessionToken", kv.get("aws_session_token").cloned()),
        ]);

        let id = format!(
            "aws-{}-{}",
            name.to_lowercase().replace(' ', "-"),
            &crate::bridge_shared::uuid_v4()[..8]
        );

        servers.push(ServerProfileExport {
            id,
            name: name.clone(),
            host,
            port,
            username: akid,
            protocol: Some("s3".to_string()),
            initial_path: None,
            local_initial_path: None,
            color: None,
            last_connected: None,
            options: if options.is_empty() {
                None
            } else {
                Some(serde_json::Value::Object(options))
            },
            provider_id: Some(provider_id),
            credential: kv
                .get("aws_secret_access_key")
                .filter(|s| !s.is_empty())
                .cloned(),
            has_stored_credential: None,
            public_url_base: None,
            ..Default::default()
        });
    }

    Ok(AwsImportResult {
        servers,
        skipped,
        source_path: cred_path.display().to_string(),
        total_remotes,
    })
}

// ============ Export to AWS shared credentials/config ============

/// A server profile to export as an AWS shared-credentials profile.
/// Mirrors `rclone_import::RcloneExportServer`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwsExportServer {
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

/// Export server profiles to the native AWS INI files.
///
/// Writes the credentials file at `out` (`[name]` with
/// `aws_access_key_id`/`aws_secret_access_key`/optional
/// `aws_session_token`) and a sibling `config` file (`[profile name]` with
/// `region`/`endpoint_url`) so the round-trip is symmetric with import.
/// Both are written atomically with `0600` because the credentials file
/// carries the secret.
pub fn export_aws_credentials(
    servers: &[AwsExportServer],
    passwords: &std::collections::HashMap<String, String>,
    out: &Path,
) -> Result<usize, String> {
    let mut cred = String::new();
    cred.push_str("# Generated by AeroFTP - https://aeroftp.app\n");
    cred.push_str(&format!(
        "# Exported: {}\n\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));

    let mut config = String::new();
    config.push_str("# Generated by AeroFTP - https://aeroftp.app\n\n");

    let mut exported = 0;

    for server in servers {
        let opts = server.options.as_ref().and_then(|v| v.as_object());
        let opt_str = |key: &str| -> Option<String> {
            opts.and_then(|m| m.get(key))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
        };

        cred.push_str(&format!("[{}]\n", server.name));
        cred.push_str(&format!("aws_access_key_id = {}\n", server.username));
        if let Some(secret) = passwords.get(&server.name).filter(|s| !s.is_empty()) {
            cred.push_str(&format!("aws_secret_access_key = {secret}\n"));
        }
        if let Some(token) = opt_str("sessionToken") {
            cred.push_str(&format!("aws_session_token = {token}\n"));
        }
        cred.push('\n');

        let region = opt_str("region");
        let endpoint = if !server.host.is_empty() {
            Some(
                if server.host.starts_with("http://") || server.host.starts_with("https://") {
                    server.host.clone()
                } else {
                    format!("https://{}", server.host)
                },
            )
        } else {
            None
        };
        if region.is_some() || endpoint.is_some() {
            config.push_str(&format!("[profile {}]\n", server.name));
            if let Some(r) = region {
                config.push_str(&format!("region = {r}\n"));
            }
            if let Some(ep) = endpoint {
                config.push_str(&format!("endpoint_url = {ep}\n"));
            }
            config.push('\n');
        }

        exported += 1;
    }

    crate::bridge_shared::atomic_write_600(out, cred.as_bytes())?;
    let config_path = out.with_file_name("config");
    crate::bridge_shared::atomic_write_600(&config_path, config.as_bytes())?;

    Ok(exported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const CRED_FIXTURE: &str = r#"
[default]
aws_access_key_id = AKIADEFAULT
aws_secret_access_key = defaultsecret

[minio]
aws_access_key_id = AKIAMINIO
aws_secret_access_key = miniosecret
aws_session_token = SESSIONTOKEN123

[no-keys]
region = eu-west-1
"#;

    const CONFIG_FIXTURE: &str = r#"
[profile default]
region = us-east-1

[profile minio]
region = us-east-1
endpoint_url = https://s3.lab.example.com:9000

[profile no-keys]
region = eu-west-1
"#;

    fn write_tmp(name: &str, content: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "aeroftp-aws-test-{}-{}",
            name,
            crate::bridge_shared::uuid_v4()
        ));
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn test_parse_fixture_three_profiles_one_skipped() {
        let cred = write_tmp("cred", CRED_FIXTURE);
        let cfg = write_tmp("cfg", CONFIG_FIXTURE);

        let result = import_aws_credentials_with_config(&cred, Some(&cfg)).expect("should import");
        std::fs::remove_file(&cred).ok();
        std::fs::remove_file(&cfg).ok();

        // 3 sections total: default, minio, no-keys.
        assert_eq!(result.total_remotes, 3);
        // default + minio map; no-keys skipped.
        assert_eq!(result.servers.len(), 2);
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].name, "no-keys");
        assert_eq!(result.skipped[0].reason, "no aws_access_key_id");
        assert_eq!(result.skipped[0].kind, "aws");

        // default: no endpoint -> empty host, amazon-s3.
        let def = result.servers.iter().find(|s| s.name == "default").unwrap();
        assert_eq!(def.host, "");
        assert_eq!(def.port, 443);
        assert_eq!(def.protocol.as_deref(), Some("s3"));
        assert_eq!(def.provider_id.as_deref(), Some("amazon-s3"));
        assert_eq!(def.username, "AKIADEFAULT");

        // minio: endpoint with port -> host:port + custom-s3 + region/token.
        let mc = result.servers.iter().find(|s| s.name == "minio").unwrap();
        assert_eq!(mc.host, "s3.lab.example.com:9000");
        assert_eq!(mc.port, 9000);
        assert_eq!(mc.provider_id.as_deref(), Some("custom-s3"));
        let opts = mc.options.as_ref().unwrap().as_object().unwrap();
        assert_eq!(
            opts.get("region").and_then(|v| v.as_str()),
            Some("us-east-1")
        );
        assert_eq!(
            opts.get("sessionToken").and_then(|v| v.as_str()),
            Some("SESSIONTOKEN123")
        );
    }

    #[test]
    fn test_secret_policy_plain_credential_populated() {
        let cred = write_tmp("sec", CRED_FIXTURE);
        let result = import_aws_credentials_with_config(&cred, None).expect("should import");
        std::fs::remove_file(&cred).ok();

        // File-plain secret -> credential is populated (not None).
        let def = result.servers.iter().find(|s| s.name == "default").unwrap();
        assert_eq!(def.credential.as_deref(), Some("defaultsecret"));
        assert!(def.has_stored_credential.is_none());

        let mc = result.servers.iter().find(|s| s.name == "minio").unwrap();
        assert_eq!(mc.credential.as_deref(), Some("miniosecret"));
    }

    #[test]
    fn test_roundtrip_import_export_import_idempotent() {
        let cred = write_tmp("rt-cred", CRED_FIXTURE);
        let cfg = cred.with_file_name("config");
        std::fs::write(&cfg, CONFIG_FIXTURE).unwrap();

        let first = import_aws_credentials(&cred).expect("first import");

        // Export to a fresh location, carrying secrets via the password map.
        let mut passwords: HashMap<String, String> = HashMap::new();
        for s in &first.servers {
            if let Some(c) = &s.credential {
                passwords.insert(s.name.clone(), c.clone());
            }
        }
        let export_servers: Vec<AwsExportServer> = first
            .servers
            .iter()
            .map(|s| AwsExportServer {
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

        let out = std::env::temp_dir().join(format!(
            "aeroftp-aws-rt-{}/credentials",
            crate::bridge_shared::uuid_v4()
        ));
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        let n = export_aws_credentials(&export_servers, &passwords, &out).expect("export");
        assert_eq!(n, 2);

        let second = import_aws_credentials(&out).expect("second import");

        std::fs::remove_file(&cred).ok();
        std::fs::remove_file(&cfg).ok();
        let _ = std::fs::remove_dir_all(out.parent().unwrap());

        // Metadata idempotence: same set of profiles with the same mapping.
        assert_eq!(first.servers.len(), second.servers.len());
        for fs in &first.servers {
            let ss = second
                .servers
                .iter()
                .find(|x| x.name == fs.name)
                .expect("profile must survive round-trip");
            assert_eq!(fs.host, ss.host);
            assert_eq!(fs.port, ss.port);
            assert_eq!(fs.username, ss.username);
            assert_eq!(fs.protocol, ss.protocol);
            assert_eq!(fs.provider_id, ss.provider_id);
            assert_eq!(fs.credential, ss.credential);
            let fo = fs.options.as_ref().and_then(|v| v.as_object());
            let so = ss.options.as_ref().and_then(|v| v.as_object());
            assert_eq!(
                fo.and_then(|m| m.get("region")),
                so.and_then(|m| m.get("region"))
            );
            assert_eq!(
                fo.and_then(|m| m.get("sessionToken")),
                so.and_then(|m| m.get("sessionToken"))
            );
        }
    }

    #[test]
    fn test_default_path_env_override() {
        let key = "AWS_SHARED_CREDENTIALS_FILE";
        let saved = std::env::var(key).ok();

        std::env::set_var(key, "/tmp/custom-aws-creds");
        assert_eq!(
            default_aws_credentials_config_path(),
            Some(PathBuf::from("/tmp/custom-aws-creds"))
        );

        match saved {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
