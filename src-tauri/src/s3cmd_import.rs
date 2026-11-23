// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import/export server profiles from the s3cmd configuration file (`~/.s3cfg`).
//!
//! s3cmd is mono-config: a single INI file with one `[default]` section. The
//! relevant keys are `access_key`, `secret_key`, `host_base`, `host_bucket`,
//! `bucket_location` (region), and `use_https`. A configured `proxy_host` is
//! reported as an unimported direct-connection warning. The S3 secret lives
//! plain in the file, so it is bridged and re-encrypted into our AES-256-GCM vault.
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
/// Returns the skip reason when `access_key` is empty (without it the profile
/// cannot authenticate to S3) or when `host_base` is not a `host[:port]`
/// endpoint (see `bridge_shared::split_endpoint`).
fn map_s3cmd(section: &HashMap<String, String>) -> Result<MappedProfile, &'static str> {
    let get = |k: &str| section.get(k).map(|s| s.trim()).filter(|s| !s.is_empty());

    let access_key = get("access_key").ok_or("no access_key")?.to_string();

    let host_base_raw = get("host_base").unwrap_or("").to_string();

    // host_base may carry an explicit `host:port`. Only an omitted or blank
    // host_base means the AWS default endpoint; any explicit non-empty
    // host_base without a valid host (such as `https:///bucket` or ambiguous
    // userinfo) is skipped rather than silently redirected to AWS.
    let (host, explicit_port) = if host_base_raw.is_empty() {
        (String::new(), None)
    } else {
        let host_base_endpoint = crate::bridge_shared::try_endpoint_host(&host_base_raw)
            .ok_or("host_base is not a valid host[:port] endpoint")?;
        split_host_port(&host_base_endpoint)
    };
    let provider_id = crate::bridge_shared::map_s3_provider_from_endpoint(&host).to_string();

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
        // s3cmd addresses path-style exactly when the template has no bucket.
        options.insert(
            "pathStyle".into(),
            serde_json::Value::Bool(!hb.contains("%(bucket)s")),
        );
        options.insert("hostBucket".into(), serde_json::Value::String(hb));
    }
    options.insert("useHttps".into(), serde_json::Value::Bool(use_https));
    // `use_https` is the scheme, and `host` + `port` alone lose it (a
    // cleartext endpoint on 8080 would come back as HTTPS).
    // AWS stays implicit: an explicit endpoint switches the S3 connection to
    // path-style, which AWS refuses for newer buckets.
    let aws = provider_id == "amazon-s3";
    let default_aws_transport = aws && use_https && port == 443;
    if !host.is_empty() && !default_aws_transport {
        let scheme = if use_https { "https" } else { "http" };
        let default_port = if use_https { 443 } else { 80 };
        let endpoint = if port == default_port {
            format!("{scheme}://{host}")
        } else {
            format!("{scheme}://{host}:{port}")
        };
        options.insert("endpoint".into(), serde_json::Value::String(endpoint));
    }

    Ok(MappedProfile {
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
        .and_then(crate::bridge_shared::try_endpoint_host);
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
    pub warnings: Vec<S3cmdImportWarning>,
    pub source_path: String,
    pub total_remotes: usize,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct S3cmdImportWarning {
    pub name: String,
    pub reason: String,
}

/// Import the single s3cmd profile from a `~/.s3cfg` file.
pub fn import_s3cmd(path: &Path) -> Result<S3cmdImportResult, String> {
    let content = crate::bridge_shared::read_bounded_utf8_file(
        path,
        crate::bridge_shared::MAX_IMPORT_FILE_BYTES,
    )
    .map_err(|e| format!("read s3cmd config: {e}"))?;

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
    let mut warnings = Vec::new();

    let total_remotes = if section.is_some() { 1 } else { 0 };

    if let Some(section) = section {
        let name = s3cmd_profile_name(&section);
        match map_s3cmd(&section) {
            Ok(mapped) => {
                if section
                    .get("proxy_host")
                    .is_some_and(|value| !value.trim().is_empty())
                {
                    warnings.push(S3cmdImportWarning {
                        name: crate::bridge_shared::sanitize_warning_name(&name, "s3cmd default"),
                        reason: "s3cmd proxy_host is configured; the proxy is not imported and AeroFTP will connect directly".to_string(),
                    });
                }
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
                    ..Default::default()
                });
            }
            Err(reason) => {
                skipped.push(S3cmdSkippedRemote {
                    name,
                    kind: "s3".to_string(),
                    reason: reason.to_string(),
                });
            }
        }
    }

    Ok(S3cmdImportResult {
        servers,
        skipped,
        warnings,
        source_path: path.display().to_string(),
        total_remotes,
    })
}

// ============ Export to ~/.s3cfg ============

/// A server profile to export as an s3cmd `[default]` section.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S3cmdExportServer {
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

/// Export the first S3 profile to a native s3cmd `~/.s3cfg` file.
///
/// s3cmd is mono-config: only one `[default]` section is emitted (the first
/// S3 profile in `servers`); every other S3 profile is reported as skipped
/// instead of vanishing. The S3 secret is written in cleartext, exactly as
/// s3cmd does natively; the file is written `0600` via `atomic_write_600`.
///
/// `host_base` is the bare `host[:port]` of the endpoint the connection uses
/// ([`crate::bridge_shared::resolve_export_endpoint`]), `use_https` carries
/// its scheme, and `host_bucket` is `host_base` itself when the connection
/// addresses buckets path-style (s3cmd's own path-style switch), else the
/// `%(bucket)s.` template on that host.
/// Every string of the profile that goes into `.s3cfg` stays one value: a
/// line break in one would add keys to `[default]`, a `gpg_command` with
/// `encrypt = True` among them, which s3cmd runs on the next upload. The
/// endpoint needs no check: it is rebuilt from a parsed URL.
fn unwritable_s3cmd_value(
    server: &S3cmdExportServer,
    passwords: &HashMap<String, String>,
) -> Result<(), String> {
    let option = |key: &str| {
        server
            .options
            .as_ref()
            .and_then(|o| o.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or("")
    };
    let line = crate::bridge_shared::export_line_value;
    line("access key", &server.username)?;
    line(
        "secret",
        crate::bridge_shared::export_secret(passwords, &server.id).unwrap_or(""),
    )?;
    line("region", option("region"))?;
    line("bucket host template", option("hostBucket"))
}

pub fn export_s3cmd(
    servers: &[S3cmdExportServer],
    passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<crate::bridge_shared::BridgeExportOutcome, String> {
    let mut outcome = crate::bridge_shared::BridgeExportOutcome::default();
    let mut chosen: Option<(
        &S3cmdExportServer,
        Option<crate::bridge_shared::ExportEndpoint>,
    )> = None;
    for s in servers
        .iter()
        .filter(|s| s.protocol.as_deref() == Some("s3"))
    {
        if let Some((first, _)) = chosen {
            outcome.skip(
                &s.name,
                format!(
                    "s3cmd holds one configuration and it went to \"{}\": \
                     export this profile on its own",
                    first.name
                ),
            );
            continue;
        }
        match crate::bridge_shared::resolve_export_endpoint(
            "s3",
            &s.host,
            s.port,
            &s.username,
            s.options.as_ref(),
            s.provider_id.as_deref(),
        )
        .and_then(|ep| unwritable_s3cmd_value(s, passwords).map(|()| ep))
        {
            Ok(ep) => chosen = Some((s, ep)),
            Err(reason) => outcome.skip(&s.name, reason),
        }
    }

    let Some((server, endpoint)) = chosen else {
        if outcome.skipped.is_empty() {
            return Err("export s3cmd: no s3 profile to export".to_string());
        }
        return Ok(outcome);
    };

    let opts = server.options.as_ref().and_then(|v| v.as_object());
    let opt_str = |k: &str| {
        opts.and_then(|m| m.get(k))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    };
    let opt_bool = |k: &str| opts.and_then(|m| m.get(k)).and_then(|v| v.as_bool());

    let secret = crate::bridge_shared::export_secret(passwords, &server.id).unwrap_or_default();
    let region = opt_str("region").unwrap_or_default();

    // No endpoint of its own: the profile is on AWS, s3cmd's default.
    let (host_base, use_https, path_style) = match &endpoint {
        Some(ep) => (ep.authority(), ep.is_tls(), ep.s3_path_style),
        None => (
            "s3.amazonaws.com".to_string(),
            opt_bool("useHttps").unwrap_or(true),
            false,
        ),
    };
    // A template preserved from an s3cmd import round-trips verbatim.
    let host_bucket = opt_str("hostBucket").unwrap_or_else(|| {
        if path_style {
            host_base.clone()
        } else {
            format!("%(bucket)s.{host_base}")
        }
    });

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

    outcome.exported = 1;
    crate::bridge_shared::write_export(outcome.exported, out, body.as_bytes())?;
    Ok(outcome)
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
    fn configured_proxy_imports_with_a_direct_connection_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s3cfg");
        std::fs::write(
            &path,
            "[default]\naccess_key = AKIAIOSFODNN7EXAMPLE\nsecret_key = secret\nhost_base = s3.amazonaws.com\nproxy_host = proxy.example.com\nproxy_port = 3128\n",
        )
        .unwrap();

        let result = import_s3cmd(&path).unwrap();
        assert_eq!(result.servers.len(), 1);
        assert!(result.skipped.is_empty());
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].reason.contains("proxy_host"));
        assert!(result.warnings[0].reason.contains("connect directly"));
    }

    #[test]
    fn proxy_warning_name_strips_userinfo_and_query_secrets_from_host_base() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s3cfg");
        let user = "s3user";
        let secret = "topSecretPass";
        let token = "querySecretTok";
        std::fs::write(
            &path,
            format!(
                "[default]\naccess_key = DEMOACCESSKEY123456\nsecret_key = demosecret\nhost_base = https://{user}:{secret}@s3.example.com:9000?token={token}#frag\nproxy_host = proxy.example.com\n"
            ),
        )
        .unwrap();

        let result = import_s3cmd(&path).unwrap();
        assert_eq!(result.servers.len(), 1);
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].name, "s3cmd s3.example.com:9000");
        let serialized = serde_json::to_string(&result.warnings).unwrap();
        assert!(!serialized.contains(user));
        assert!(!serialized.contains(secret));
        assert!(!serialized.contains(token));
    }

    #[test]
    fn host_base_with_an_unencoded_separator_in_its_password_is_skipped_without_leaking_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s3cfg");
        let user = "alice";
        let pw = "pa/ssSecret";
        std::fs::write(
            &path,
            format!(
                "[default]\naccess_key = DEMOACCESSKEY123456\nsecret_key = demosecret\nhost_base = https://{user}:{pw}@s3.example.com\nproxy_host = proxy.example.com\n"
            ),
        )
        .unwrap();

        let result = import_s3cmd(&path).unwrap();
        assert!(result.servers.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert!(result.skipped[0].reason.contains("host_base"));
        let serialized =
            serde_json::to_string(&(&result.servers, &result.skipped, &result.warnings)).unwrap();
        for secret in [user, "pa/ss", "ssSecret"] {
            assert!(
                !serialized.contains(secret),
                "{secret} leaked: {serialized}"
            );
        }
    }

    #[test]
    fn empty_authority_skipped_while_query_scheme_and_aws_userinfo_classify_parsed_host() {
        let dir = tempfile::tempdir().unwrap();

        // Explicit endpoint with empty authority must be skipped, not defaulted to AWS.
        let bad_path = dir.path().join("bad.s3cfg");
        std::fs::write(
            &bad_path,
            "[default]\naccess_key = DEMOKEY\nsecret_key = demosecret\nhost_base = https:///bucket\n",
        )
        .unwrap();
        let bad = import_s3cmd(&bad_path).unwrap();
        assert!(bad.servers.is_empty());
        assert_eq!(bad.skipped.len(), 1);
        assert!(bad.skipped[0].reason.contains("host_base"));

        // Omitted host_base still uses the AWS default.
        let blank_path = dir.path().join("blank.s3cfg");
        std::fs::write(
            &blank_path,
            "[default]\naccess_key = DEMOKEY\nsecret_key = demosecret\n",
        )
        .unwrap();
        let blank = import_s3cmd(&blank_path).unwrap();
        assert_eq!(blank.servers.len(), 1);
        assert_eq!(blank.servers[0].provider_id.as_deref(), Some("amazon-s3"));
        assert!(blank.servers[0].host.is_empty());

        // Query value containing `://` must not become the host or warning label.
        let tok = "QUERYSECRETTOK";
        let query_path = dir.path().join("query.s3cfg");
        std::fs::write(
            &query_path,
            format!(
                "[default]\naccess_key = DEMOKEY\nsecret_key = demosecret\nhost_base = s3.example.test?token=https://{tok}\nproxy_host = proxy.corp\n"
            ),
        )
        .unwrap();
        let query = import_s3cmd(&query_path).unwrap();
        assert_eq!(query.servers.len(), 1);
        assert_eq!(query.servers[0].host, "s3.example.test");
        assert_eq!(query.warnings.len(), 1);
        assert_eq!(query.warnings[0].name, "s3cmd s3.example.test");
        let serialized = serde_json::to_string(&(&query.servers, &query.warnings)).unwrap();
        assert!(!serialized.contains(tok), "token leaked: {serialized}");

        // AWS host with userinfo classifies as amazon-s3 (no options.endpoint);
        // custom host with explicit port stays custom-s3 with options.endpoint.
        let user = "alice";
        let pw = "pwSecret";
        let aws_path = dir.path().join("aws.s3cfg");
        std::fs::write(
            &aws_path,
            format!(
                "[default]\naccess_key = DEMOKEY\nsecret_key = demosecret\nhost_base = https://{user}:{pw}@s3.amazonaws.com\n"
            ),
        )
        .unwrap();
        let aws = import_s3cmd(&aws_path).unwrap();
        assert_eq!(aws.servers.len(), 1);
        assert_eq!(aws.servers[0].provider_id.as_deref(), Some("amazon-s3"));
        assert_eq!(aws.servers[0].host, "s3.amazonaws.com");
        assert!(aws.servers[0]
            .options
            .as_ref()
            .and_then(|o| o.get("endpoint"))
            .is_none());

        let custom_path = dir.path().join("custom.s3cfg");
        std::fs::write(
            &custom_path,
            "[default]\naccess_key = DEMOKEY\nsecret_key = demosecret\nhost_base = objects.example.test:9000\n",
        )
        .unwrap();
        let custom = import_s3cmd(&custom_path).unwrap();
        assert_eq!(custom.servers.len(), 1);
        assert_eq!(custom.servers[0].provider_id.as_deref(), Some("custom-s3"));
        assert_eq!(custom.servers[0].port, 9000);
        assert_eq!(
            custom.servers[0]
                .options
                .as_ref()
                .and_then(|o| o.get("endpoint")),
            Some(&serde_json::json!("https://objects.example.test:9000"))
        );

        // Port-only endpoint `https://:9000` is skipped; bracketed IPv6 + port succeeds.
        let port_only_path = dir.path().join("port-only.s3cfg");
        std::fs::write(
            &port_only_path,
            "[default]\naccess_key = DEMOKEY\nsecret_key = demosecret\nhost_base = https://:9000\n",
        )
        .unwrap();
        let port_only = import_s3cmd(&port_only_path).unwrap();
        assert!(port_only.servers.is_empty());
        assert_eq!(port_only.skipped.len(), 1);

        let ipv6_path = dir.path().join("ipv6.s3cfg");
        std::fs::write(
            &ipv6_path,
            "[default]\naccess_key = DEMOKEY\nsecret_key = demosecret\nhost_base = https://[::1]:9000\n",
        )
        .unwrap();
        let ipv6 = import_s3cmd(&ipv6_path).unwrap();
        assert_eq!(ipv6.servers.len(), 1);
        assert_eq!(ipv6.servers[0].host, "[::1]");
        assert_eq!(ipv6.servers[0].port, 9000);

        for bad_ep in [
            "https://objects.example.test:70000",
            "https://objects.example.test:0",
            "https://objects.example.test:abc",
        ] {
            let bp = dir.path().join("badport.s3cfg");
            std::fs::write(
                &bp,
                format!(
                    "[default]\naccess_key = DEMOKEY\nsecret_key = demosecret\nhost_base = {bad_ep}\n"
                ),
            )
            .unwrap();
            let bres = import_s3cmd(&bp).unwrap();
            assert!(bres.servers.is_empty(), "{bad_ep} must be skipped");
            assert_eq!(bres.skipped.len(), 1, "{bad_ep}");
        }
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
            id: s1.id.clone(),
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
        passwords.insert(s1.id.clone(), s1.credential.clone().unwrap_or_default());

        let out = std::env::temp_dir().join("aeroftp-test-s3cmd-rt-out.cfg");
        let exported = export_s3cmd(&export_servers, &passwords, &out).expect("export");
        assert_eq!(exported.exported, 1);
        assert!(exported.skipped.is_empty());

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
        // Process-wide env: serialised with every other test that edits it.
        let _env = crate::test_env::lock();
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

    /// Of two profiles of one name (a list saved before names were unique),
    /// the one written is the first, and with its own secret: the map keyed
    /// by name gave it the second one's.
    #[test]
    fn of_two_profiles_of_one_name_the_one_written_has_its_own_secret() {
        let (servers, passwords) = crate::bridge_shared::two_profiles_named_lab::<S3cmdExportServer>(
            serde_json::json!({"host": "", "port": 443, "username": "AKA", "protocol": "s3", "providerId": "minio", "options": {"endpoint": "https://a.example.com", "bucket": "tests", "region": "us-east-1"}}),
            serde_json::json!({"host": "", "port": 443, "username": "AKB", "protocol": "s3", "providerId": "minio", "options": {"endpoint": "https://b.example.com", "bucket": "tests", "region": "us-east-1"}}),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s3cfg");
        let outcome = export_s3cmd(&servers, &passwords, &path).unwrap();
        assert_eq!((outcome.exported, outcome.skipped.len()), (1, 1));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("access_key = AKA\nsecret_key = secret-a\n"),
            "{body}"
        );
        assert!(body.contains("host_base = a.example.com"), "{body}");
    }

    /// A line break in a value added keys to `[default]`,
    /// a `gpg_command` s3cmd runs (with `encrypt = True`) among them. Each
    /// such profile is refused by name and field, and the next profile, a
    /// non-ASCII region and a key with spaces included, is the one written.
    #[test]
    fn a_profile_whose_value_breaks_its_line_is_refused_and_the_next_is_written() {
        let payload = "gpg_command = /tmp/run-me";
        let profile = |name: &str, user: &str, options: serde_json::Value| -> S3cmdExportServer {
            serde_json::from_value(serde_json::json!({
                "id": format!("id-{name}"), "name": name, "host": "", "port": 443,
                "username": user, "protocol": "s3", "providerId": "minio",
                "options": options,
            }))
            .unwrap()
        };
        let options = |key: &str, value: &str| {
            let mut o = serde_json::json!({"endpoint": "https://s3.example.com", "bucket": "b", "region": "eu-west-1"});
            if !key.is_empty() {
                o[key] = value.into();
            }
            o
        };
        for brk in crate::bridge_shared::LINE_BREAKS {
            let hostile = format!("x{brk}encrypt = True{brk}{payload}");
            let servers = vec![
                profile("user", &hostile, options("", "")),
                profile("region", "AKR", options("region", &hostile)),
                profile("bucket", "AKB", options("hostBucket", &hostile)),
                profile("secret", "AKS", options("", "")),
                profile("legit", "AK LEGIT", options("region", "région-1")),
            ];
            let mut passwords: HashMap<String, String> = servers
                .iter()
                .map(|s| (s.id.clone(), "plain-secret".to_string()))
                .collect();
            passwords.insert("id-secret".into(), hostile.clone());
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("s3cfg");
            let outcome = export_s3cmd(&servers, &passwords, &path).unwrap();
            assert_eq!(outcome.exported, 1);
            let refused: Vec<(&str, &str)> = outcome
                .skipped
                .iter()
                .map(|s| (s.name.as_str(), s.reason.as_str()))
                .collect();
            for ((name, reason), field) in
                refused
                    .iter()
                    .zip(["access key", "region", "bucket host template", "secret"])
            {
                assert!(
                    reason.starts_with(&format!("its {field} ")),
                    "{name}: {reason}"
                );
                assert!(!reason.contains(payload));
            }
            assert_eq!(
                refused.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
                ["user", "region", "bucket", "secret"]
            );
            let body = std::fs::read_to_string(&path).unwrap();
            crate::bridge_shared::assert_no_injected_line(&body, payload);
            assert!(!body.contains("encrypt"), "{body}");
            assert!(
                body.contains("access_key = AK LEGIT\nsecret_key = plain-secret\n"),
                "{body}"
            );
            assert!(body.contains("bucket_location = région-1\n"), "{body}");
        }
    }
}
