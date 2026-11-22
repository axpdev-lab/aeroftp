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
//! Both files are read as the AWS CLI reads them ([`parse_aws_ini`],
//! [`aws_config_from`]): section names verbatim in the credentials file,
//! `[default]` and shell-quoted `[profile name]` in the config, `=` or `:`,
//! sub-sections, `services` endpoints; a file the AWS CLI refuses (a section
//! or key declared twice, a line it cannot parse) is refused here too.
//!
//! A profile without a non-empty `aws_access_key_id` is skipped with a reason
//! that starts "no aws_access_key_id" and names what it uses instead (a role,
//! SSO, `credential_process`). A profile with an access key id but no
//! `aws_secret_access_key` is skipped with a reason (partial credentials).
//! A profile with static keys that the AWS CLI would not sign in with as they
//! are (`role_arn`, SSO settings), or with a setting AeroFTP does not apply,
//! imports with a warning that says so. The import result names every skipped
//! profile and every warning.

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
    /// Profiles imported without something the AWS CLI applies to them.
    pub warnings: Vec<AwsSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

/// Import all usable profiles from `~/.aws/credentials`, augmented by the
/// resolved sibling `~/.aws/config` (env `AWS_CONFIG_FILE` respected).
pub fn import_aws_credentials(cred_path: &Path) -> Result<AwsImportResult, String> {
    let config_path = aws_config_path_for(cred_path);
    import_aws_credentials_with_config(cred_path, config_path.as_deref())
}

/// True when `path` is a regular file within the shared import size cap.
///
/// The secondary config file is opened by us, not by the caller, so it bypasses
/// the checks `import_bridge_config` / the CLI run on the primary path.
pub(crate) fn is_readable_regular_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) => m.is_file() && crate::bridge_shared::ensure_import_file_size_ok(path).is_ok(),
        Err(_) => false,
    }
}

// ============ The AWS CLI's reading of its files ============

/// Sections one file may declare; within the 10 MB cap a file could still
/// declare hundreds of thousands, each a vault write on import.
const AWS_MAX_SECTIONS: usize = 10_000;

/// One section as the AWS CLI reads it: plain values, and the values of an
/// indented block under an empty key (`s3 =` / `    addressing_style = path`).
#[derive(Default, Clone, Debug)]
struct AwsSection {
    values: std::collections::HashMap<String, String>,
    nested: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

impl AwsSection {
    fn get(&self, key: &str) -> Option<&str> {
        self.values
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    fn nested_get(&self, block: &str, key: &str) -> Option<&str> {
        self.nested
            .get(block)
            .and_then(|b| b.get(key))
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    fn is_true(&self, key: &str) -> bool {
        self.get(key)
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }

    /// botocore `Session.full_config`: the credentials file's keys win.
    fn merge_over(&mut self, other: AwsSection) {
        self.values.extend(other.values);
        self.nested.extend(other.nested);
    }
}

/// `content` read the way the AWS CLI reads its files: Python
/// `configparser.RawConfigParser` under botocore's `raw_config_parse`. Section
/// names are verbatim (`[name] ; note` is the section `name`), `=` or `:`
/// ends a key (the first of the two), a line starting with `#` or `;` is a
/// comment, a line indented deeper than its key continues that key's value,
/// and a value that starts on the next line is a sub-section of `key = value`
/// lines. A section or a key declared twice, a line with neither delimiter
/// and a key before any section are what configparser refuses, and with it
/// every AWS CLI command: the import refuses them too. The reason names the
/// line, never its text, which can hold a secret.
fn parse_aws_ini(content: &str, label: &str) -> Result<Vec<(String, AwsSection)>, String> {
    let refuse =
        |line: usize, why: &str| format!("the AWS CLI cannot read {label}: line {line} {why}");
    type RawOptions = Vec<(String, Vec<String>)>;
    let mut raw: Vec<(String, RawOptions)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // The indent of the key line the next deeper line continues.
    let mut key_indent: Option<usize> = None;
    for (index, line) in content.lines().enumerate() {
        let lineno = index + 1;
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let current = raw.last_mut().and_then(|(_, options)| options.last_mut());
        if trimmed.is_empty() {
            if let (Some(_), Some((_, lines))) = (key_indent, current) {
                lines.push(String::new());
            }
            continue;
        }
        if let (Some(base), Some((_, lines))) = (key_indent, current) {
            if indent > base {
                lines.push(trimmed.to_string());
                continue;
            }
        }
        if trimmed.starts_with('[') {
            if let Some(end) = trimmed.rfind(']').filter(|end| *end > 1) {
                let name = trimmed[1..end].to_string();
                if !seen.insert(name.clone()) {
                    return Err(refuse(lineno, "declares a section a second time"));
                }
                if raw.len() >= AWS_MAX_SECTIONS {
                    return Err(format!(
                        "{label} declares more than {AWS_MAX_SECTIONS} sections"
                    ));
                }
                raw.push((name, Vec::new()));
                key_indent = None;
                continue;
            }
        }
        let Some((_, options)) = raw.last_mut() else {
            return Err(refuse(lineno, "comes before any section"));
        };
        let Some(split) = trimmed.find(['=', ':']) else {
            return Err(refuse(lineno, "has neither `=` nor `:`"));
        };
        let key = trimmed[..split].trim().to_lowercase();
        if key.is_empty() {
            return Err(refuse(lineno, "has no key"));
        }
        if options.iter().any(|(k, _)| *k == key) {
            return Err(refuse(lineno, "sets a key its section already set"));
        }
        options.push((key, vec![trimmed[split + 1..].trim().to_string()]));
        key_indent = Some(indent);
    }

    let mut sections = Vec::with_capacity(raw.len());
    for (name, options) in raw {
        let mut section = AwsSection::default();
        for (key, lines) in options {
            let value = lines.join("\n").trim_end().to_string();
            if value.starts_with('\n') {
                let mut block = std::collections::HashMap::new();
                for line in value.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    let Some((k, v)) = line.split_once('=') else {
                        return Err(format!(
                            "the AWS CLI cannot read {label}: the `{key}` block of [{name}] has a line without `=`"
                        ));
                    };
                    block.insert(k.trim().to_string(), v.trim().to_string());
                }
                section.nested.insert(key, block);
            } else {
                section.values.insert(key, value);
            }
        }
        sections.push((name, section));
    }
    Ok(sections)
}

/// Python `shlex.split` (POSIX mode, no comments), which botocore uses on a
/// config section name. `None` where shlex raises (an open quote, a trailing
/// backslash).
fn shell_words(s: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => word.push(ch),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => {
                            let next = chars.next()?;
                            if next != '"' && next != '\\' {
                                word.push('\\');
                            }
                            word.push(next);
                        }
                        ch => word.push(ch),
                    }
                }
            }
            '\\' => {
                in_word = true;
                word.push(chars.next()?);
            }
            ' ' | '\t' | '\r' | '\n' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            ch => {
                in_word = true;
                word.push(ch);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Some(words)
}

/// botocore `_parse_section`: a header names an entry of `kind` only when it
/// splits into exactly two shell words whose first word is `kind`.
fn aws_named_section(header: &str, kind: &str) -> Option<String> {
    if !header.starts_with(kind) {
        return None;
    }
    match shell_words(header)?.as_slice() {
        [first, name] if first == kind => Some(name.clone()),
        _ => None,
    }
}

/// The profiles and `services` sections of a config file (botocore
/// `build_profile_map`). Any other section (`[name]` without the `profile`
/// prefix, `[profile a b]`, `[sso-session x]`) is no profile for the AWS CLI,
/// and none here. A later section of one name replaces the earlier one.
#[derive(Default)]
struct AwsConfig {
    profiles: std::collections::HashMap<String, AwsSection>,
    services: std::collections::HashMap<String, AwsSection>,
}

fn aws_config_from(sections: Vec<(String, AwsSection)>) -> AwsConfig {
    let mut config = AwsConfig::default();
    for (header, section) in sections {
        if header.starts_with("profile") {
            if let Some(name) = aws_named_section(&header, "profile") {
                config.profiles.insert(name, section);
            }
        } else if header.starts_with("sso-session") {
            // Read by the SSO sign-in, which AeroFTP does not perform.
        } else if header.starts_with("services") {
            if let Some(name) = aws_named_section(&header, "services") {
                config.services.insert(name, section);
            }
        } else if header == "default" {
            config.profiles.insert(header, section);
        }
    }
    config
}

/// Settings the AWS CLI applies to a profile and an imported AeroFTP profile
/// does not carry: `(where, key, only when "true")`.
const AWS_UNAPPLIED_SETTINGS: &[(Option<&str>, &str, bool)] = &[
    (None, "ca_bundle", false),
    (None, "use_fips_endpoint", true),
    (None, "use_dualstack_endpoint", true),
    (Some("s3"), "use_accelerate_endpoint", true),
    (Some("s3"), "use_dualstack_endpoint", true),
];

/// Import with an explicit (optional) config path. Kept separate for
/// testability: `import_aws_credentials` resolves the sibling/env config,
/// while tests inject a known config path or `None`.
pub fn import_aws_credentials_with_config(
    cred_path: &Path,
    config_path: Option<&Path>,
) -> Result<AwsImportResult, String> {
    let cred =
        std::fs::read_to_string(cred_path).map_err(|e| format!("read aws credentials: {e}"))?;
    let cred_sections = parse_aws_ini(&cred, &cred_path.display().to_string())?;

    let mut config = AwsConfig::default();
    if let Some(cp) = config_path {
        // The sibling config (or whatever AWS_CONFIG_FILE points at) never went
        // through the caller's regular-file + 10 MB checks, so apply them here:
        // reading a character device such as /dev/zero would grow the buffer
        // until the process is OOM-killed.
        if is_readable_regular_file(cp) {
            if let Ok(cfg) = std::fs::read_to_string(cp) {
                config = aws_config_from(parse_aws_ini(&cfg, &cp.display().to_string())?);
            }
        }
    }

    // botocore `Session.full_config`: each credentials section is the profile
    // of its own name (no `profile` prefix in this file), over the config's.
    let services = config.services;
    let mut profiles = config.profiles;
    for (name, section) in cred_sections {
        profiles.entry(name).or_default().merge_over(section);
    }

    let total_remotes = profiles.len();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();
    let mut warnings = Vec::new();

    // Stable ordering so the output is deterministic across runs.
    let mut names: Vec<&String> = profiles.keys().collect();
    names.sort();

    for name in names {
        let kv = &profiles[name];
        let entry = |reason: String| AwsSkippedRemote {
            name: name.clone(),
            kind: "aws".to_string(),
            reason,
        };
        // The AWS CLI reaches for these before the static keys (botocore's
        // chain: assume-role, web identity and SSO come before the shared
        // credentials file; credential_process comes after it).
        let assumes_role = kv.get("role_arn").is_some();
        let uses_sso = [
            "sso_session",
            "sso_start_url",
            "sso_account_id",
            "sso_role_name",
        ]
        .iter()
        .any(|k| kv.get(k).is_some());

        let akid = match kv.get("aws_access_key_id") {
            Some(v) => v.to_string(),
            None => {
                let why = if assumes_role {
                    ": it assumes a role (role_arn), which AeroFTP does not do; import the profile its source credentials come from"
                } else if uses_sso {
                    ": it signs in through AWS SSO (IAM Identity Center), which AeroFTP does not do"
                } else if kv.get("credential_process").is_some() {
                    ": it gets its keys from credential_process, which AeroFTP does not run"
                } else {
                    ""
                };
                skipped.push(entry(format!("no aws_access_key_id{why}")));
                continue;
            }
        };

        let Some(secret_key) = kv.get("aws_secret_access_key") else {
            skipped.push(entry(
                "no aws_secret_access_key: the AWS CLI rejects partial credentials without a secret key".to_string(),
            ));
            continue;
        };

        // botocore `ConfiguredEndpointProvider`: the profile's `services`
        // section's `s3` endpoint, then the profile's own `endpoint_url`; a
        // `services` name the file does not define fails every command.
        let service = match kv.get("services") {
            Some(section) => match services.get(section) {
                Some(s) => Some(s),
                None => {
                    skipped.push(entry(format!(
                        "its services section \"{section}\" is not in the config file, and the AWS CLI refuses the profile"
                    )));
                    continue;
                }
            },
            None => None,
        };
        let endpoint_url = if kv.is_true("ignore_configured_endpoint_urls") {
            None
        } else {
            service
                .and_then(|s| s.nested_get("s3", "endpoint_url"))
                .or_else(|| kv.get("endpoint_url"))
        };

        if assumes_role {
            warnings.push(entry(
                "the AWS CLI assumes its role_arn for this profile; AeroFTP signs in with its static keys, as another identity".to_string(),
            ));
        }
        if uses_sso {
            warnings.push(entry(
                "the AWS CLI signs this profile in through AWS SSO before it reads its static keys; AeroFTP uses the static keys".to_string(),
            ));
        }
        for (block, key, when_true) in AWS_UNAPPLIED_SETTINGS {
            let value = match block {
                Some(b) => kv.nested_get(b, key),
                None => kv.get(key),
            };
            if value.is_some_and(|v| !when_true || v.eq_ignore_ascii_case("true")) {
                let setting = match block {
                    Some(b) => format!("{b}.{key}"),
                    None => key.to_string(),
                };
                warnings.push(entry(format!(
                    "it sets {setting}, which AeroFTP does not apply to the connection"
                )));
            }
        }

        let (host, port, provider_id) = match endpoint_url {
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

        // `endpoint_url` is a URL: keep it whole, or a cleartext endpoint
        // off the ports the S3 connection guesses as HTTP comes back as HTTPS.
        // AWS itself stays implicit: an explicit endpoint switches the S3
        // connection to path-style, which AWS refuses for newer buckets.
        let endpoint = endpoint_url
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
            .filter(|s| crate::bridge_shared::map_s3_provider_from_endpoint(s) != "amazon-s3");
        let token = kv
            .get("aws_session_token")
            .or_else(|| kv.get("aws_security_token"));
        let mut options = crate::bridge_shared::json_map(&[
            ("region", kv.get("region").map(str::to_string)),
            ("sessionToken", token.map(str::to_string)),
            ("endpoint", endpoint),
        ]);
        // The `s3` sub-section's `addressing_style`.
        match kv
            .nested_get("s3", "addressing_style")
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("path") => {
                options.insert("pathStyle".into(), serde_json::Value::Bool(true));
            }
            Some("virtual") => {
                options.insert("pathStyle".into(), serde_json::Value::Bool(false));
            }
            _ => {}
        }

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
            credential: Some(secret_key.to_string()),
            has_stored_credential: None,
            public_url_base: None,
            ..Default::default()
        });
    }

    Ok(AwsImportResult {
        servers,
        skipped,
        warnings,
        source_path: cred_path.display().to_string(),
        total_remotes,
    })
}

// ============ Export to AWS shared credentials/config ============

const AWS_EXPORT_HEADER: &str = "# Generated by AeroFTP - https://aeroftp.app";

/// Typed integrity markers for AeroFTP AWS exports (AWS-01, AWS-02, AWS-15).
/// The marker sits on line 2 and carries the SHA-256 of every byte after the
/// marker line. A file without the exact marker for its kind or whose digest
/// no longer matches (edited by `aws configure`, edited by hand, legacy
/// header-only export, or another AeroFTP exporter) is treated as user-owned
/// and preserved untouched.
const AWS_CONFIG_MARKER: &str = "# AeroFTP-Export: aws-config sha256=";
const AWS_CREDENTIALS_MARKER: &str = "# AeroFTP-Export: aws-credentials sha256=";
const AWS_MARKER_PREFIX: &str = "# AeroFTP-Export: ";

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn aws_marked_file_bytes(marker: &str, body: &str) -> Vec<u8> {
    format!(
        "{AWS_EXPORT_HEADER}\n{marker}{}\n{body}",
        sha256_hex(body.as_bytes())
    )
    .into_bytes()
}

/// Private `0700` output directory for `aeroftp-cli export aws` when the
/// caller omitted `--output` (AWS-08). Avoids writing sibling `config` files
/// into a shared `/tmp` directory where another local user can plant entries.
pub fn default_aws_cli_export_path() -> Result<PathBuf, String> {
    let dir = tempfile::Builder::new()
        .prefix("aeroftp-aws-export-")
        .tempdir()
        .map_err(|e| format!("create private AWS export directory: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("set 0700 permissions on AWS export directory: {e}"))?;
    }
    Ok(dir.keep().join("credentials"))
}

fn aws_side_path(target_path: &Path, index: usize) -> PathBuf {
    let base = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("aws");
    let name = if index == 0 {
        format!("{base}.aeroftp")
    } else {
        format!("{base}.aeroftp.{index}")
    };
    target_path.with_file_name(name)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AwsFileOwnership {
    /// Exact typed marker and matching SHA-256 digest: unchanged AeroFTP export.
    Owned,
    /// Starts with the legacy AeroFTP header but has no typed integrity marker.
    /// Treated as user-owned because edits cannot be ruled out.
    LegacyHeaderOnly,
    /// Any other file, modified export, wrong export type, symlink, or directory.
    UserOwned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AwsProtectedWriteMode {
    Replace,
    Absent,
    Side,
    ReplaceSide(usize),
}

struct ProtectedWriteOutcome {
    path: PathBuf,
    protected: bool,
    existing_was_legacy: bool,
}

/// Read the bytes of the regular file at `path` through one handle that does
/// not follow a symlink at the final component and does not block on a FIFO,
/// checking `fstat` on that same handle (AWS-07). Returns `None` for symlinks,
/// directories, FIFOs, devices, or files over `MAX_IMPORT_FILE_BYTES`.
fn read_regular_file_no_follow(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: open a link itself, not its target.
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    let cap = crate::bridge_shared::MAX_IMPORT_FILE_BYTES;
    if !metadata.is_file() || metadata.len() > cap {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(cap + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= cap).then_some(bytes)
}

fn probe_aws_file_ownership(path: &Path, expected_marker: &str) -> AwsFileOwnership {
    let Some(bytes) = read_regular_file_no_follow(path) else {
        return AwsFileOwnership::UserOwned;
    };
    let Ok(content) = std::str::from_utf8(&bytes) else {
        return AwsFileOwnership::UserOwned;
    };
    let Some(rest) = content
        .strip_prefix(AWS_EXPORT_HEADER)
        .and_then(|r| r.strip_prefix("\r\n").or_else(|| r.strip_prefix('\n')))
    else {
        return AwsFileOwnership::UserOwned;
    };
    match rest.strip_prefix(expected_marker) {
        Some(marked) => {
            let matches = marked.split_once('\n').is_some_and(|(hash, body)| {
                hash.trim_end_matches('\r') == sha256_hex(body.as_bytes())
            });
            if matches {
                AwsFileOwnership::Owned
            } else {
                AwsFileOwnership::UserOwned
            }
        }
        None if rest.starts_with(AWS_MARKER_PREFIX) => AwsFileOwnership::UserOwned,
        None => AwsFileOwnership::LegacyHeaderOnly,
    }
}

fn aws_file_is_aeroftp_owned(path: &Path, expected_marker: &str) -> bool {
    probe_aws_file_ownership(path, expected_marker) == AwsFileOwnership::Owned
}

fn paths_collide(candidate: &Path, avoid: Option<&Path>) -> bool {
    let Some(av) = avoid else {
        return false;
    };
    let same_name = candidate
        .file_name()
        .and_then(|a| a.to_str())
        .zip(av.file_name().and_then(|b| b.to_str()))
        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b));
    if same_name && candidate.parent() == av.parent() {
        return true;
    }
    candidate.exists() && av.exists() && same_file::is_same_file(candidate, av).unwrap_or(false)
}

fn aws_side_output_path(
    target_path: &Path,
    avoid: Option<&Path>,
    expected_marker: &str,
    start_index: usize,
) -> Result<(PathBuf, AwsProtectedWriteMode), String> {
    let base = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("aws");
    for index in start_index..10_000 {
        let candidate = aws_side_path(target_path, index);
        if paths_collide(&candidate, avoid) {
            continue;
        }
        match std::fs::symlink_metadata(&candidate) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok((candidate, AwsProtectedWriteMode::Side));
            }
            Ok(_) if aws_file_is_aeroftp_owned(&candidate, expected_marker) => {
                return Ok((candidate, AwsProtectedWriteMode::ReplaceSide(index)));
            }
            _ => {}
        }
    }
    Err(format!("no free or reusable {base}.aeroftp side-file name"))
}

fn aws_protected_output_path(
    target_path: &Path,
    avoid: Option<&Path>,
    expected_marker: &str,
    label: &str,
) -> Result<(PathBuf, AwsProtectedWriteMode), String> {
    match std::fs::symlink_metadata(target_path) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok((target_path.to_path_buf(), AwsProtectedWriteMode::Absent));
        }
        Err(err) => return Err(format!("inspect existing {label}: {err}")),
    }
    // A link (e.g. a dotfiles checkout), a directory, a modified export, a
    // legacy header-only export or any user file stays untouched and the
    // generated output goes to the first absent or owned side file (AWS-06).
    if aws_file_is_aeroftp_owned(target_path, expected_marker) {
        return Ok((target_path.to_path_buf(), AwsProtectedWriteMode::Replace));
    }
    aws_side_output_path(target_path, avoid, expected_marker, 0)
}

#[allow(clippy::too_many_arguments)]
fn write_aws_protected_staged_with(
    target_path: &Path,
    avoid: Option<&Path>,
    expected_marker: &str,
    label: &str,
    mut tmp: tempfile::NamedTempFile,
    initial_mode: AwsProtectedWriteMode,
    mut persist_replace: impl FnMut(
        tempfile::NamedTempFile,
        &Path,
    ) -> Result<(), tempfile::PersistError>,
    mut persist_noclobber: impl FnMut(
        tempfile::NamedTempFile,
        &Path,
    ) -> Result<(), tempfile::PersistError>,
) -> Result<ProtectedWriteOutcome, String> {
    const MAX_RETRIES: usize = 10_000;
    let mut mode = initial_mode;
    let mut retries = 0usize;
    loop {
        match mode {
            AwsProtectedWriteMode::Replace => {
                // Re-probe immediately before atomic rename: narrows the race
                // (AWS-14 residual race is inherent without advisory locks).
                if !aws_file_is_aeroftp_owned(target_path, expected_marker) {
                    (_, mode) =
                        aws_protected_output_path(target_path, avoid, expected_marker, label)?;
                    continue;
                }
                persist_replace(tmp, target_path)
                    .map_err(|e| format!("write {label} {}: {}", target_path.display(), e.error))?;
                return Ok(ProtectedWriteOutcome {
                    path: target_path.to_path_buf(),
                    protected: false,
                    existing_was_legacy: false,
                });
            }
            AwsProtectedWriteMode::ReplaceSide(index) => {
                let candidate = aws_side_path(target_path, index);
                if paths_collide(&candidate, avoid)
                    || !aws_file_is_aeroftp_owned(&candidate, expected_marker)
                {
                    mode = AwsProtectedWriteMode::Side;
                    continue;
                }
                let existing_was_legacy = probe_aws_file_ownership(target_path, expected_marker)
                    == AwsFileOwnership::LegacyHeaderOnly;
                persist_replace(tmp, &candidate).map_err(|e| {
                    format!(
                        "write {label} side file {}: {}",
                        candidate.display(),
                        e.error
                    )
                })?;
                return Ok(ProtectedWriteOutcome {
                    path: candidate,
                    protected: true,
                    existing_was_legacy,
                });
            }
            AwsProtectedWriteMode::Side => {
                // Side-file contract: always select the first absent or owned
                // side-file slot from index 0 at write time.
                let (candidate, next_mode) =
                    aws_side_output_path(target_path, avoid, expected_marker, 0)?;
                if let AwsProtectedWriteMode::ReplaceSide(index) = next_mode {
                    mode = AwsProtectedWriteMode::ReplaceSide(index);
                    continue;
                }
                match persist_noclobber(tmp, &candidate) {
                    Ok(()) => {
                        let existing_was_legacy =
                            probe_aws_file_ownership(target_path, expected_marker)
                                == AwsFileOwnership::LegacyHeaderOnly;
                        return Ok(ProtectedWriteOutcome {
                            path: candidate,
                            protected: true,
                            existing_was_legacy,
                        });
                    }
                    Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => {
                        tmp = err.file;
                        retries += 1;
                        if retries >= MAX_RETRIES {
                            return Err(format!(
                                "write {label} side file {}: exhausted retry budget",
                                candidate.display()
                            ));
                        }
                    }
                    Err(err) => {
                        return Err(format!(
                            "write {label} side file {}: {}",
                            candidate.display(),
                            err.error
                        ));
                    }
                }
            }
            AwsProtectedWriteMode::Absent => match persist_noclobber(tmp, target_path) {
                Ok(()) => {
                    return Ok(ProtectedWriteOutcome {
                        path: target_path.to_path_buf(),
                        protected: false,
                        existing_was_legacy: false,
                    });
                }
                Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    tmp = err.file;
                    (_, mode) =
                        aws_protected_output_path(target_path, avoid, expected_marker, label)?;
                }
                Err(err) => {
                    return Err(format!(
                        "write {label} {}: {}",
                        target_path.display(),
                        err.error
                    ));
                }
            },
        }
    }
}

fn persist_aws_export_pair(
    cred_path: &Path,
    config_path: &Path,
    config_bytes: &[u8],
    cred_bytes: &[u8],
    mut persist_replace: impl FnMut(
        &str,
        tempfile::NamedTempFile,
        &Path,
    ) -> Result<(), tempfile::PersistError>,
    mut persist_noclobber: impl FnMut(
        &str,
        tempfile::NamedTempFile,
        &Path,
    ) -> Result<(), tempfile::PersistError>,
) -> Result<(ProtectedWriteOutcome, ProtectedWriteOutcome), String> {
    let parent = cred_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // AWS-09: stage and sync BOTH 0600 temp files before touching either target.
    let config_tmp = crate::bridge_shared::stage_export_temp_600(parent, config_bytes)
        .map_err(|e| format!("stage AWS config temp file: {e}"))?;
    let cred_tmp = crate::bridge_shared::stage_export_temp_600(parent, cred_bytes)
        .map_err(|e| format!("stage AWS credentials temp file: {e}"))?;

    // Persist config first (holds no secrets), then credentials.
    let (_, config_mode) = aws_protected_output_path(
        config_path,
        Some(cred_path),
        AWS_CONFIG_MARKER,
        "AWS config",
    )?;
    let config_written = write_aws_protected_staged_with(
        config_path,
        Some(cred_path),
        AWS_CONFIG_MARKER,
        "AWS config",
        config_tmp,
        config_mode,
        |tmp, path| persist_replace("config", tmp, path),
        |tmp, path| persist_noclobber("config", tmp, path),
    )?;

    let (_, cred_mode) = aws_protected_output_path(
        cred_path,
        Some(&config_written.path),
        AWS_CREDENTIALS_MARKER,
        "AWS credentials",
    )
    .map_err(|e| {
        format!(
            "{e} (AWS config was already written to {})",
            config_written.path.display()
        )
    })?;
    let cred_written = write_aws_protected_staged_with(
        cred_path,
        Some(&config_written.path),
        AWS_CREDENTIALS_MARKER,
        "AWS credentials",
        cred_tmp,
        cred_mode,
        |tmp, path| persist_replace("credentials", tmp, path),
        |tmp, path| persist_noclobber("credentials", tmp, path),
    )
    .map_err(|e| {
        format!(
            "{e} (AWS config was already written to {})",
            config_written.path.display()
        )
    })?;

    Ok((config_written, cred_written))
}

/// The `~/.aws/config` section header of `profile` (AWS-03). botocore splits a
/// `[profile ...]` header shell-style and takes it as a profile only when it
/// is two words: a name with a space, tab or `'` goes in double quotes, and a
/// name with a `"` or `\` has no header the AWS CLI reads back as that name.
fn aws_config_section_header(profile: &str) -> Result<String, String> {
    if profile == "default" {
        return Ok("[default]\n".to_string());
    }
    if profile.contains(['"', '\\']) {
        return Err("its name holds a double quote or a backslash, which an AWS config profile name cannot carry, so the AWS CLI would not find its endpoint or region; rename the profile and export again".to_string());
    }
    if profile.contains([' ', '\t', '\'']) {
        Ok(format!("[profile \"{profile}\"]\n"))
    } else {
        Ok(format!("[profile {profile}]\n"))
    }
}

/// A server profile to export as an AWS shared-credentials profile.
/// Mirrors `rclone_import::RcloneExportServer`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwsExportServer {
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

fn format_side_file_warning(
    kind: &str,
    target_path: &Path,
    side_path: &Path,
    legacy_header_only: bool,
) -> String {
    if legacy_header_only {
        format!(
            "existing {} was left unchanged (it has no AeroFTP integrity marker, so edits cannot be ruled out); AeroFTP {kind} were written to {}. Merge that file manually",
            target_path.display(),
            side_path.display()
        )
    } else {
        format!(
            "existing {} was left unchanged; AeroFTP {kind} were written to {}. Merge that file manually",
            target_path.display(),
            side_path.display()
        )
    }
}

/// Export server profiles to the native AWS INI files.
///
/// Writes the credentials file at `out` (`[name]` with
/// `aws_access_key_id`/`aws_secret_access_key`/optional
/// `aws_session_token`) and a sibling `config` file (`[profile name]` with
/// `region`/`endpoint_url`) so the round-trip is symmetric with import.
/// Both temp files are staged and synced at `0600` before either destination
/// is touched, then `config` is persisted before `credentials`.
///
/// An existing `config` or `credentials` target is replaced in place ONLY when
/// it carries AeroFTP's typed integrity marker and a matching SHA-256 digest
/// for its kind. Any other existing entry (user file, `aws configure` edit,
/// legacy pre-marker AeroFTP export, symlink, directory) is left untouched and
/// generated output goes to the first absent or AeroFTP-owned `.aeroftp` side
/// file at write time with an export warning.
///
/// `endpoint_url` is the endpoint the connection uses
/// ([`crate::bridge_shared::resolve_export_endpoint`]): a profile that keeps
/// it in `options.endpoint` (MinIO) or gets it from its preset used to be
/// written with none, which points the AWS tools at AWS. A path-style
/// connection adds the `s3` sub-section `addressing_style = path`.
pub fn export_aws_credentials(
    servers: &[AwsExportServer],
    passwords: &std::collections::HashMap<String, String>,
    out: &Path,
) -> Result<crate::bridge_shared::BridgeExportOutcome, String> {
    let config_path = out.with_file_name("config");
    let named_config = out
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.trim_end_matches('.').eq_ignore_ascii_case("config"));
    let same_existing_file = out.exists()
        && config_path.exists()
        && same_file::is_same_file(out, &config_path).unwrap_or(false);
    if named_config || same_existing_file {
        return Err(
            "AWS credentials output cannot be the sibling config file; choose another file name"
                .to_string(),
        );
    }

    // Bodies after the typed marker line (`aws_marked_file_bytes`).
    let mut cred = format!(
        "# Exported: {}\n\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    let mut config = String::from("\n");

    let mut outcome = crate::bridge_shared::BridgeExportOutcome::default();
    // Both files hold one section per profile name (`[name]`, `[profile name]`):
    // a second section of one name is not a second profile.
    let mut names = crate::bridge_shared::ExportEntryNames::exact(servers.iter().map(|s| &s.name));

    for server in servers {
        let resolved = match crate::bridge_shared::resolve_export_endpoint(
            "s3",
            &server.host,
            server.port,
            &server.username,
            server.options.as_ref(),
            server.provider_id.as_deref(),
        ) {
            Ok(e) => e,
            Err(reason) => {
                outcome.skip(&server.name, reason);
                continue;
            }
        };
        let opts = server.options.as_ref().and_then(|v| v.as_object());
        let opt_str = |key: &str| -> Option<String> {
            opts.and_then(|m| m.get(key))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
        };

        let secret = crate::bridge_shared::export_secret(passwords, &server.id);
        let token = opt_str("sessionToken");
        let region = opt_str("region");
        // Every string written below, before any of it is: a line break in
        // one would add keys of its own (a `credential_process` the AWS tools
        // run) to the profile or to another section.
        let unwritable = crate::bridge_shared::export_line_value("name", &server.name)
            .and_then(|()| crate::bridge_shared::export_line_value("access key", &server.username))
            .and_then(|()| crate::bridge_shared::export_line_value("secret", secret.unwrap_or("")))
            .and_then(|()| {
                crate::bridge_shared::export_line_value(
                    "session token",
                    token.as_deref().unwrap_or(""),
                )
            })
            .and_then(|()| {
                crate::bridge_shared::export_line_value("region", region.as_deref().unwrap_or(""))
            });
        if let Err(reason) = unwritable {
            outcome.skip(&server.name, reason);
            continue;
        }

        let path_style = resolved.as_ref().is_some_and(|ep| ep.s3_path_style);
        let endpoint = resolved.map(|ep| ep.base_url());
        let profile = names.claim(&server.name);
        // AWS-03: validate the config header before writing credentials, so a
        // refused profile writes nothing to either file.
        let config_header = if region.is_some() || endpoint.is_some() {
            match aws_config_section_header(&profile) {
                Ok(header) => Some(header),
                Err(reason) => {
                    outcome.skip(&server.name, reason);
                    continue;
                }
            }
        } else {
            None
        };

        cred.push_str(&format!("[{}]\n", profile));
        cred.push_str(&format!("aws_access_key_id = {}\n", server.username));
        if let Some(secret) = secret {
            cred.push_str(&format!("aws_secret_access_key = {secret}\n"));
        }
        if let Some(token) = token {
            cred.push_str(&format!("aws_session_token = {token}\n"));
        }
        cred.push('\n');

        if let Some(header) = config_header {
            config.push_str(&header);
            if let Some(r) = region {
                config.push_str(&format!("region = {r}\n"));
            }
            if let Some(ep) = endpoint {
                config.push_str(&format!("endpoint_url = {ep}\n"));
            }
            if path_style {
                config.push_str("s3 =\n    addressing_style = path\n");
            }
            config.push('\n');
        }

        outcome.exported += 1;
    }

    if outcome.exported == 0 {
        return Ok(outcome);
    }

    let config_bytes = aws_marked_file_bytes(AWS_CONFIG_MARKER, &config);
    let cred_bytes = aws_marked_file_bytes(AWS_CREDENTIALS_MARKER, &cred);
    let (config_written, cred_written) = persist_aws_export_pair(
        out,
        &config_path,
        &config_bytes,
        &cred_bytes,
        |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
        |_, tmp, path| crate::bridge_shared::persist_staged_temp_noclobber(tmp, path),
    )?;

    if config_written.protected {
        outcome.warn(
            "AWS config",
            format_side_file_warning(
                "settings",
                &config_path,
                &config_written.path,
                config_written.existing_was_legacy,
            ),
        );
    }
    if cred_written.protected {
        outcome.warn(
            "AWS credentials",
            format_side_file_warning(
                "credentials",
                out,
                &cred_written.path,
                cred_written.existing_was_legacy,
            ),
        );
    }

    Ok(outcome)
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
        // The import reads AWS_CONFIG_FILE, which the launch URL tests set
        // under the env lock: hold it so they never point this one elsewhere.
        let _env = crate::test_env::lock();
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
        // The import reads AWS_CONFIG_FILE, which the launch URL tests set
        // under the env lock: hold it so they never point this one elsewhere.
        let _env = crate::test_env::lock();
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
        // The import reads AWS_CONFIG_FILE, which the launch URL tests set
        // under the env lock: hold it so they never point this one elsewhere.
        let _env = crate::test_env::lock();
        let cred = write_tmp("rt-cred", CRED_FIXTURE);
        let cfg = cred.with_file_name("config");
        std::fs::write(&cfg, CONFIG_FIXTURE).unwrap();

        let first = import_aws_credentials(&cred).expect("first import");

        // Export to a fresh location, carrying secrets via the password map.
        let mut passwords: HashMap<String, String> = HashMap::new();
        for s in &first.servers {
            if let Some(c) = &s.credential {
                passwords.insert(s.id.clone(), c.clone());
            }
        }
        let export_servers: Vec<AwsExportServer> = first
            .servers
            .iter()
            .map(|s| AwsExportServer {
                id: s.id.clone(),
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
        assert_eq!(n.exported, 2);
        assert!(n.skipped.is_empty());

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
        // Process-wide env: serialised with every other test that edits it.
        let _env = crate::test_env::lock();
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

    /// A name or a value with a line break in it added sections and
    /// keys of its own to the files, a `credential_process` the AWS tools
    /// run among them. Each such profile is refused by name and field, and
    /// the others, spaces and non-ASCII in their names included, still go
    /// out exactly as they are.
    #[test]
    fn a_profile_whose_name_or_value_breaks_its_line_is_refused() {
        let payload = "credential_process = /tmp/run-me";
        let profile = |name: &str, user: &str, options: serde_json::Value| -> AwsExportServer {
            serde_json::from_value(serde_json::json!({
                "id": format!("id-{name}"), "name": name, "host": "", "port": 443,
                "username": user, "protocol": "s3", "providerId": "minio",
                "options": options,
            }))
            .unwrap()
        };
        let options = |extra: serde_json::Value| {
            let mut o = serde_json::json!({"endpoint": "https://s3.example.com", "bucket": "b", "region": "eu-west-1"});
            o.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            o
        };
        for brk in crate::bridge_shared::LINE_BREAKS {
            let hostile = format!("x{brk}{payload}{brk}");
            let cases: Vec<(AwsExportServer, &str)> = vec![
                (
                    profile(
                        &format!("evil{brk}[other"),
                        "AKN",
                        options(serde_json::json!({})),
                    ),
                    "name",
                ),
                (
                    profile("user", &hostile, options(serde_json::json!({}))),
                    "access key",
                ),
                (
                    profile(
                        "token",
                        "AKT",
                        options(serde_json::json!({"sessionToken": hostile})),
                    ),
                    "session token",
                ),
                (
                    profile(
                        "region",
                        "AKR",
                        options(serde_json::json!({"region": hostile})),
                    ),
                    "region",
                ),
                (
                    profile("secret", "AKS", options(serde_json::json!({}))),
                    "secret",
                ),
            ];
            let expected: Vec<(String, &str)> = cases
                .iter()
                .map(|(s, field)| (s.name.clone(), *field))
                .collect();
            let mut servers: Vec<AwsExportServer> = cases.into_iter().map(|(s, _)| s).collect();
            servers.push(profile(
                "Lab-östra-2",
                "AKLEGIT",
                options(serde_json::json!({})),
            ));
            let mut passwords: HashMap<String, String> = servers
                .iter()
                .map(|s| (s.id.clone(), "plain-secret".to_string()))
                .collect();
            passwords.insert("id-secret".into(), hostile.clone());

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("credentials");
            let outcome = export_aws_credentials(&servers, &passwords, &path).unwrap();
            assert_eq!(outcome.exported, 1, "{brk:?}: {:?}", outcome.skipped);
            assert_eq!(
                outcome.skipped.len(),
                expected.len(),
                "{brk:?}: {:?}",
                outcome.skipped
            );
            for ((name, field), skip) in expected.iter().zip(&outcome.skipped) {
                assert_eq!(&skip.name, name);
                assert!(
                    skip.reason.starts_with(&format!("its {field} ")),
                    "{}",
                    skip.reason
                );
                assert!(!skip.reason.contains(payload), "the reason shows the value");
            }
            let cred = std::fs::read_to_string(&path).unwrap();
            let config = std::fs::read_to_string(dir.path().join("config")).unwrap();
            crate::bridge_shared::assert_no_injected_line(&cred, payload);
            crate::bridge_shared::assert_no_injected_line(&config, payload);
            let sections = crate::bridge_shared::parse_ini_sections(&cred);
            assert_eq!(
                sections.keys().collect::<Vec<_>>(),
                ["Lab-östra-2"],
                "{cred}"
            );
            assert!(cred.contains("[Lab-östra-2]\naws_access_key_id = AKLEGIT\naws_secret_access_key = plain-secret\n"), "{cred}");
            assert!(
                config.contains("[profile Lab-östra-2]\nregion = eu-west-1\n"),
                "{config}"
            );
        }
    }

    fn zero_export_inputs() -> Vec<Vec<AwsExportServer>> {
        let refused: AwsExportServer = serde_json::from_value(serde_json::json!({
            "id": "refused", "name": "refused\nprofile", "host": "", "port": 443,
            "username": "KEY", "protocol": "s3", "providerId": "amazon-s3"
        }))
        .unwrap();
        vec![Vec::new(), vec![refused]]
    }

    #[test]
    fn zero_export_leaves_existing_credentials_and_config_untouched() {
        for servers in zero_export_inputs() {
            let dir = tempfile::tempdir().unwrap();
            let credentials = dir.path().join("credentials");
            let config = dir.path().join("config");
            std::fs::write(&credentials, b"existing credentials").unwrap();
            std::fs::write(&config, b"existing config").unwrap();

            let outcome = export_aws_credentials(&servers, &HashMap::new(), &credentials).unwrap();
            assert_eq!(outcome.exported, 0);
            assert_eq!(
                std::fs::read(&credentials).unwrap(),
                b"existing credentials"
            );
            assert_eq!(std::fs::read(&config).unwrap(), b"existing config");
            assert!(!dir.path().join("config.aeroftp").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn zero_export_never_reads_a_config_symlink() {
        for servers in zero_export_inputs() {
            let dir = tempfile::tempdir().unwrap();
            let credentials = dir.path().join("credentials");
            let target = dir.path().join("target");
            std::fs::write(&credentials, b"existing credentials").unwrap();
            std::fs::write(&target, b"existing config").unwrap();
            std::os::unix::fs::symlink(&target, dir.path().join("config")).unwrap();

            let outcome = export_aws_credentials(&servers, &HashMap::new(), &credentials).unwrap();
            assert_eq!(outcome.exported, 0);
            assert_eq!(
                std::fs::read(&credentials).unwrap(),
                b"existing credentials"
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"existing config");
            assert!(!dir.path().join("config.aeroftp").exists());
        }
    }

    fn export_static(dir: &Path) -> Result<crate::bridge_shared::BridgeExportOutcome, String> {
        export_static_to(&dir.join("credentials"))
    }

    fn export_static_to(out: &Path) -> Result<crate::bridge_shared::BridgeExportOutcome, String> {
        let server: AwsExportServer = serde_json::from_value(serde_json::json!({
            "id": "static", "name": "static", "host": "", "port": 443,
            "username": "NEWKEY", "protocol": "s3", "providerId": "amazon-s3",
            "options": {"region": "eu-west-1"}
        }))
        .unwrap();
        let passwords = HashMap::from([("static".to_string(), "newsecret".to_string())]);
        export_aws_credentials(&[server], &passwords, out)
    }

    #[test]
    fn export_preserves_existing_user_config_and_writes_a_warned_side_file() {
        let dir = tempfile::tempdir().unwrap();
        let original = b"[profile \"static\"] # keep\nrole_arn: arn:aws:iam::123456789012:role/bridge\ncredential_cache = memory\n";
        std::fs::write(dir.path().join("config"), original).unwrap();
        std::fs::write(dir.path().join("config.aeroftp"), "taken").unwrap();

        let outcome = export_static(dir.path()).unwrap();
        assert_eq!(std::fs::read(dir.path().join("config")).unwrap(), original);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.aeroftp")).unwrap(),
            "taken"
        );
        let side = dir.path().join("config.aeroftp.1");
        assert!(std::fs::read_to_string(side)
            .unwrap()
            .contains("region = eu-west-1"));
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].reason.contains("config.aeroftp.1"));
        assert!(outcome.warnings[0]
            .reason
            .contains("Merge that file manually"));
    }

    fn write_test_protected_config(
        config: &Path,
        mode: AwsProtectedWriteMode,
        bytes: &[u8],
    ) -> Result<ProtectedWriteOutcome, String> {
        let tmp =
            crate::bridge_shared::stage_export_temp_600(config.parent().unwrap(), bytes).unwrap();
        write_aws_protected_staged_with(
            config,
            None,
            AWS_CONFIG_MARKER,
            "AWS config",
            tmp,
            mode,
            crate::bridge_shared::persist_staged_temp,
            crate::bridge_shared::persist_staged_temp_noclobber,
        )
    }

    #[test]
    fn export_replaces_a_previous_aeroftp_config_without_a_side_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config"),
            aws_marked_file_bytes(
                AWS_CONFIG_MARKER,
                "\n[profile static]\nregion = us-west-1\n",
            ),
        )
        .unwrap();

        let outcome = export_static(dir.path()).unwrap();
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        let config = std::fs::read_to_string(dir.path().join("config")).unwrap();
        assert!(config.contains("region = eu-west-1"));
        assert!(!config.contains("region = us-west-1"));
        assert!(!dir.path().join("config.aeroftp").exists());
    }

    #[test]
    fn export_creates_an_absent_config_without_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = export_static(dir.path()).unwrap();
        assert!(outcome.warnings.is_empty());
        assert!(std::fs::read_to_string(dir.path().join("config"))
            .unwrap()
            .contains("region = eu-west-1"));
    }

    #[test]
    fn absent_config_created_before_the_write_is_preserved_with_a_side_file_warning() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let (_, mode) =
            aws_protected_output_path(&config, None, AWS_CONFIG_MARKER, "AWS config").unwrap();
        assert_eq!(mode, AwsProtectedWriteMode::Absent);
        let original = b"[profile user]\nregion = us-east-1\n";
        std::fs::write(&config, original).unwrap();

        let written = write_test_protected_config(&config, mode, b"generated settings").unwrap();
        assert!(
            written.protected,
            "the caller must emit the side-file warning"
        );
        assert_eq!(written.path, dir.path().join("config.aeroftp"));
        assert_eq!(std::fs::read(&config).unwrap(), original);
        assert_eq!(std::fs::read(written.path).unwrap(), b"generated settings");
    }

    #[test]
    fn owned_config_replaced_by_user_bytes_before_write_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::write(
            &config,
            aws_marked_file_bytes(AWS_CONFIG_MARKER, "\n[profile old]\nregion = us-west-1\n"),
        )
        .unwrap();
        let (_, mode) =
            aws_protected_output_path(&config, None, AWS_CONFIG_MARKER, "AWS config").unwrap();
        assert_eq!(mode, AwsProtectedWriteMode::Replace);
        let original = b"[profile user]\nregion = us-east-1\n";
        std::fs::write(&config, original).unwrap();

        let written = write_test_protected_config(&config, mode, b"generated settings").unwrap();
        assert!(written.protected);
        assert_eq!(written.path, dir.path().join("config.aeroftp"));
        assert_eq!(std::fs::read(&config).unwrap(), original);
        assert_eq!(std::fs::read(written.path).unwrap(), b"generated settings");
    }

    #[test]
    fn repeated_exports_reuse_the_owned_side_file_and_preserve_user_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let side = dir.path().join("config.aeroftp");
        let original = b"[profile user]\nregion = us-east-1\n";
        std::fs::write(&config, original).unwrap();

        let first = export_static(dir.path()).unwrap();
        assert_eq!(first.warnings.len(), 1);
        assert!(std::fs::read_to_string(&side)
            .unwrap()
            .contains("region = eu-west-1"));
        let second = export_static(dir.path()).unwrap();
        assert_eq!(second.warnings.len(), 1);
        assert_eq!(std::fs::read(&config).unwrap(), original);
        assert!(std::fs::read_to_string(&side)
            .unwrap()
            .starts_with(AWS_EXPORT_HEADER));
        assert!(!dir.path().join("config.aeroftp.1").exists());
    }

    #[test]
    fn export_refuses_credentials_output_named_config_in_any_case() {
        // Real hazard: uppercase `CONFIG` when lowercase `config` is absent.
        // `same_file` cannot fire because `config` does not exist; only the
        // case-folded `named_config` rule blocks the collision.
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("CONFIG");
        assert!(!dir.path().join("config").exists());

        let err = export_static_to(&output).unwrap_err();
        assert!(err.contains("cannot be the sibling config file"), "{err}");
        assert!(!output.exists());
        assert!(!dir.path().join("config").exists());
        assert!(!dir.path().join("config.aeroftp").exists());
    }

    #[test]
    fn export_refuses_credentials_output_named_config_without_writing() {
        // Real hazard: `config` does not exist yet, so `same_existing_file` is
        // false and only `named_config` prevents writing credentials into the
        // sibling config path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        assert!(!path.exists());

        let err = export_static_to(&path).unwrap_err();
        assert!(err.contains("cannot be the sibling config file"), "{err}");
        assert!(!path.exists());
        assert!(!dir.path().join("config.aeroftp").exists());
    }

    /// Two profiles of one name (a list saved before names were unique) were
    /// two `[Lab]` sections, both with the second one's secret.
    #[test]
    fn two_profiles_of_one_name_export_as_two_profiles_each_with_its_own_secret() {
        let (servers, passwords) = crate::bridge_shared::two_profiles_named_lab::<AwsExportServer>(
            serde_json::json!({"host": "", "port": 443, "username": "AKA", "protocol": "s3", "providerId": "minio", "options": {"endpoint": "https://a.example.com", "bucket": "tests", "region": "us-east-1"}}),
            serde_json::json!({"host": "", "port": 443, "username": "AKB", "protocol": "s3", "providerId": "minio", "options": {"endpoint": "https://b.example.com", "bucket": "tests", "region": "us-east-1"}}),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let outcome = export_aws_credentials(&servers, &passwords, &path).unwrap();
        assert_eq!(outcome.exported, 2, "{:?}", outcome.skipped);
        let cred = std::fs::read_to_string(&path).unwrap();
        assert!(
            cred.contains("[Lab]\naws_access_key_id = AKA\naws_secret_access_key = secret-a\n"),
            "{cred}"
        );
        assert!(
            cred.contains("[Lab-2]\naws_access_key_id = AKB\naws_secret_access_key = secret-b\n"),
            "{cred}"
        );
        let config = std::fs::read_to_string(dir.path().join("config")).unwrap();
        assert!(config.contains("[profile Lab]\n"), "{config}");
        assert!(config.contains("[profile Lab-2]\n"), "{config}");
    }

    // ---- NAS 2216 regression tests ----

    fn import_pair(cred: &str, config: Option<&str>) -> Result<AwsImportResult, String> {
        let _env = crate::test_env::lock();
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        std::fs::write(&cred_path, cred).unwrap();
        let config_path = dir.path().join("config");
        if let Some(c) = config {
            std::fs::write(&config_path, c).unwrap();
        }
        import_aws_credentials_with_config(&cred_path, config.map(|_| config_path.as_path()))
    }

    fn export_named(name: &str) -> (crate::bridge_shared::BridgeExportOutcome, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let server: AwsExportServer = serde_json::from_value(serde_json::json!({
            "id": "lab", "name": name, "host": "", "port": 443, "username": "AKLAB",
            "protocol": "s3", "providerId": "minio",
            "options": {"endpoint": "https://s3.lab.example.com:9000", "bucket": "b", "region": "eu-west-1"}
        }))
        .unwrap();
        let outcome =
            export_aws_credentials(&[server], &HashMap::new(), &dir.path().join("credentials"))
                .unwrap();
        (outcome, dir)
    }

    #[test]
    fn a_config_the_aws_cli_edited_after_an_export_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        export_static(dir.path()).unwrap();
        let mut edited = std::fs::read(&config).unwrap();
        edited.extend_from_slice(
            b"\n[profile sso-dev]\nsso_session = corp\nsso_account_id = 123456789012\n\
              \n[sso-session corp]\nsso_start_url = https://corp.awsapps.com/start\n",
        );
        std::fs::write(&config, &edited).unwrap();

        let outcome = export_static(dir.path()).unwrap();
        assert_eq!(std::fs::read(&config).unwrap(), edited);
        let side = std::fs::read_to_string(dir.path().join("config.aeroftp")).unwrap();
        assert!(
            side.contains("[profile static]\nregion = eu-west-1\n"),
            "{side}"
        );
        assert!(
            outcome.warnings.iter().any(|w| w.name == "AWS config"),
            "{:?}",
            outcome.warnings
        );
    }

    #[test]
    fn a_pre_marker_legacy_config_is_preserved_with_reason_warning() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let legacy = format!("{AWS_EXPORT_HEADER}\n\n[profile static]\nregion = us-west-1\n");
        std::fs::write(&config, &legacy).unwrap();

        let outcome = export_static(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), legacy);
        assert!(dir.path().join("config.aeroftp").exists());
        let warn = outcome
            .warnings
            .iter()
            .find(|w| w.name == "AWS config")
            .expect("expected config warning");
        assert!(warn.reason.contains("integrity marker"), "{}", warn.reason);
    }

    #[test]
    fn another_aeroftp_export_named_config_is_not_taken_for_an_aws_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let ssh = format!(
            "{AWS_EXPORT_HEADER}\n# Exported: 2026-11-01T10:00:00Z\n\nHost lab\n  HostName lab.example.com\n  Port 22\n\n"
        );
        std::fs::write(&config, &ssh).unwrap();

        let outcome = export_static(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), ssh);
        assert!(
            outcome.warnings.iter().any(|w| w.name == "AWS config"),
            "{:?}",
            outcome.warnings
        );
    }

    #[test]
    fn existing_user_credentials_file_is_preserved_with_0600_side_file_and_warning() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let original = b"[prod]\naws_access_key_id = AKPROD\naws_secret_access_key = prodsecret\n";
        std::fs::write(&cred, original).unwrap();

        let outcome = export_static(dir.path()).unwrap();
        assert_eq!(std::fs::read(&cred).unwrap(), original);
        let side = dir.path().join("credentials.aeroftp");
        assert!(side.exists(), "expected credentials.aeroftp side file");
        let side_text = std::fs::read_to_string(&side).unwrap();
        assert!(
            side_text.contains(
                "[static]\naws_access_key_id = NEWKEY\naws_secret_access_key = newsecret\n"
            ),
            "{side_text}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&side).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(
            outcome
                .warnings
                .iter()
                .any(|w| w.name == "AWS credentials" && w.reason.contains("credentials.aeroftp")),
            "{:?}",
            outcome.warnings
        );
    }

    #[test]
    fn credentials_written_under_a_side_file_name_are_never_replaced_by_the_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config"),
            b"[profile user]\nregion = us-east-1\n",
        )
        .unwrap();
        let out = dir.path().join("config.aeroftp");
        let config_side = dir.path().join("config.aeroftp.1");
        for _ in 0..2 {
            export_static_to(&out).unwrap();
            let cred = std::fs::read_to_string(&out).unwrap();
            assert!(
                cred.contains(
                    "[static]\naws_access_key_id = NEWKEY\naws_secret_access_key = newsecret\n"
                ),
                "{cred}"
            );
            assert!(
                !aws_file_is_aeroftp_owned(&out, AWS_CONFIG_MARKER),
                "credentials file must never be recognized as an aws-config export"
            );
            let cfg = std::fs::read_to_string(&config_side).unwrap();
            assert!(
                cfg.contains("[profile static]\nregion = eu-west-1\n"),
                "{cfg}"
            );
            assert!(
                !aws_file_is_aeroftp_owned(&config_side, AWS_CREDENTIALS_MARKER),
                "config side file must never be recognized as an aws-credentials export"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_config_symlink_or_directory_stays_and_settings_go_to_a_side_file() {
        let dir = tempfile::tempdir().unwrap();
        let owned_looking = dir.path().join("dotfiles-config");
        let original = format!("{AWS_EXPORT_HEADER}\n\n[profile static]\nregion = us-west-1\n");
        std::fs::write(&owned_looking, &original).unwrap();
        for target in [owned_looking.as_path(), Path::new("/dev/zero")] {
            let case = tempfile::tempdir().unwrap();
            let link = case.path().join("config");
            std::os::unix::fs::symlink(target, &link).unwrap();

            let outcome = export_static(case.path()).unwrap();
            assert_eq!(std::fs::read_link(&link).unwrap(), target);
            let side = std::fs::read_to_string(case.path().join("config.aeroftp")).unwrap();
            assert!(
                side.contains("[profile static]\nregion = eu-west-1\n"),
                "{side}"
            );
            assert_eq!(outcome.warnings.len(), 1, "{target:?}");
        }
        assert_eq!(std::fs::read_to_string(&owned_looking).unwrap(), original);

        let dir_case = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir_case.path().join("config")).unwrap();
        let outcome = export_static(dir_case.path()).unwrap();
        assert!(dir_case.path().join("config").is_dir());
        assert!(dir_case.path().join("config.aeroftp").exists());
        assert_eq!(outcome.warnings.len(), 1);
    }

    #[test]
    fn a_profile_name_with_spaces_keeps_its_endpoint_for_the_aws_cli() {
        let (outcome, dir) = export_named("My MinIO");
        assert_eq!(outcome.exported, 1, "{:?}", outcome.skipped);
        let config = std::fs::read_to_string(dir.path().join("config")).unwrap();
        assert!(
            config.contains("[profile \"My MinIO\"]\n"),
            "expected shell-quoted header, got:\n{config}"
        );
    }

    #[test]
    fn a_profile_name_no_config_header_carries_is_refused() {
        for name in ["lab \"x\"", "lab\\x"] {
            let (outcome, dir) = export_named(name);
            assert_eq!(outcome.exported, 0, "{name}");
            assert!(
                outcome.skipped[0]
                    .reason
                    .contains("double quote or a backslash"),
                "{name}: {:?}",
                outcome.skipped
            );
            assert!(!dir.path().join("credentials").exists(), "{name}");
        }
    }

    #[test]
    fn a_section_header_with_a_trailing_comment_is_its_own_profile() {
        let result = import_pair(
            "[first]\naws_access_key_id = AKFIRST\naws_secret_access_key = first-secret\n\n\
             [second] ; production\naws_access_key_id = AKSECOND\naws_secret_access_key = second-secret\n",
            None,
        )
        .unwrap();
        let keys: Vec<_> = result
            .servers
            .iter()
            .map(|s| {
                (
                    s.name.as_str(),
                    s.username.as_str(),
                    s.credential.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            keys,
            [
                ("first", "AKFIRST", Some("first-secret")),
                ("second", "AKSECOND", Some("second-secret"))
            ]
        );
    }

    #[test]
    fn a_quoted_config_profile_name_reaches_its_credentials_section() {
        let result = import_pair(
            "[My MinIO]\naws_access_key_id = AKLAB\naws_secret_access_key = lab-secret\n",
            Some("[profile \"My MinIO\"]\nendpoint_url = https://s3.lab.example.com:9000\n"),
        )
        .unwrap();
        assert_eq!(result.servers.len(), 1);
        assert_eq!(result.servers[0].name, "My MinIO");
        assert_eq!(result.servers[0].host, "s3.lab.example.com:9000");
        assert_eq!(result.total_remotes, 1);
    }

    #[test]
    fn a_services_section_endpoint_is_the_profile_endpoint() {
        let result = import_pair(
            "[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = lab-secret\n",
            Some(
                "[profile lab]\nservices = lab-s3\nendpoint_url = https://elsewhere.example.com\n\n\
                 [services lab-s3]\ns3 =\n  endpoint_url = https://s3.lab.example.com:9000\n",
            ),
        )
        .unwrap();
        assert_eq!(result.servers.len(), 1);
        assert_eq!(result.servers[0].host, "s3.lab.example.com:9000");
        assert_eq!(result.total_remotes, 1);
    }

    #[test]
    fn a_key_of_a_sub_section_does_not_replace_the_profile_key() {
        let result = import_pair(
            "[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = lab-secret\n",
            Some(
                "[profile lab]\nendpoint_url = https://s3.lab.example.com:9000\ns3 =\n    \
                 endpoint_url = https://elsewhere.example.com\n    addressing_style = path\n",
            ),
        )
        .unwrap();
        let lab = &result.servers[0];
        assert_eq!(lab.host, "s3.lab.example.com:9000");
        assert_eq!(
            lab.options.as_ref().unwrap()["pathStyle"],
            serde_json::Value::Bool(true)
        );
    }

    #[test]
    fn a_colon_delimited_key_is_read() {
        let result = import_pair(
            "[lab]\naws_access_key_id: AKLAB\naws_secret_access_key: lab-secret\n",
            Some("[profile lab]\nendpoint_url: https://s3.lab.example.com:9000\n"),
        )
        .unwrap();
        assert_eq!(result.servers.len(), 1);
        assert_eq!(result.servers[0].credential.as_deref(), Some("lab-secret"));
        assert_eq!(result.servers[0].host, "s3.lab.example.com:9000");
    }

    #[test]
    fn a_config_section_the_aws_cli_takes_for_no_profile_adds_nothing() {
        let result = import_pair(
            "[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = lab-secret\n",
            Some("[lab]\nendpoint_url = https://s3.lab.example.com:9000\n\n[profile lab x]\nregion = eu-west-1\n"),
        )
        .unwrap();
        assert_eq!(result.servers.len(), 1);
        assert_eq!(result.servers[0].host, "");
        assert_eq!(result.total_remotes, 1);
    }

    #[test]
    fn a_credentials_profile_prefix_section_is_read_verbatim() {
        let result = import_pair(
            "[profile literal]\naws_access_key_id = AKLIT\naws_secret_access_key = lit-secret\n",
            None,
        )
        .unwrap();
        assert_eq!(result.servers.len(), 1);
        assert_eq!(result.servers[0].name, "profile literal");
    }

    #[test]
    fn a_file_the_aws_cli_refuses_is_refused_without_quoting_its_lines() {
        for cred in [
            "[lab]\naws_access_key_id = AKONE\n\n[lab]\naws_secret_access_key = SECRETVALUE\n",
            "[lab]\naws_access_key_id = AKONE\naws_access_key_id = AKTWO\naws_secret_access_key = SECRETVALUE\n",
            "[lab]\naws_access_key_id = AKONE\naws_secret_access_key SECRETVALUE\n",
        ] {
            let err = import_pair(cred, None)
                .err()
                .unwrap_or_else(|| panic!("imported: {cred}"));
            assert!(err.contains("the AWS CLI cannot read"), "{err}");
            assert!(!err.contains("SECRETVALUE") && !err.contains("AKONE"), "{err}");
        }
    }

    #[test]
    fn static_keys_the_aws_cli_would_not_sign_in_with_import_with_a_warning() {
        let result = import_pair(
            "[admin]\naws_access_key_id = AKADMIN\naws_secret_access_key = admin-secret\n\n\
             [dev]\naws_access_key_id = AKDEV\naws_secret_access_key = dev-secret\n",
            Some(
                "[profile admin]\nrole_arn = arn:aws:iam::123456789012:role/admin\nsource_profile = admin\n\n\
                 [profile dev]\nsso_session = corp\nsso_account_id = 123456789012\nsso_role_name = Dev\n",
            ),
        )
        .unwrap();
        assert_eq!(result.servers.len(), 2);
        let json = serde_json::to_value(&result).unwrap();
        let warnings = json["warnings"]
            .as_array()
            .expect("AwsImportResult must serialize warnings");
        let warned: Vec<_> = warnings
            .iter()
            .map(|w| {
                (
                    w["name"].as_str().unwrap(),
                    w["reason"].as_str().unwrap().contains("role_arn")
                        || w["reason"].as_str().unwrap().contains("SSO"),
                )
            })
            .collect();
        assert_eq!(warned, [("admin", true), ("dev", true)]);
    }

    #[test]
    fn access_key_without_secret_is_skipped_with_reason() {
        let result = import_pair("[partial]\naws_access_key_id = AKONLY\n", None).unwrap();
        assert!(
            result.servers.is_empty(),
            "partial credentials must be skipped"
        );
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].name, "partial");
        assert!(
            result.skipped[0].reason.contains("aws_secret_access_key"),
            "{}",
            result.skipped[0].reason
        );
    }

    #[test]
    fn unapplied_aws_settings_emit_import_warnings() {
        let result = import_pair(
            "[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = lab-secret\n",
            Some(
                "[profile lab]\nca_bundle = /etc/ssl/corp.pem\nuse_fips_endpoint = true\n\
                 use_dualstack_endpoint = true\ns3 =\n    use_accelerate_endpoint = true\n",
            ),
        )
        .unwrap();
        assert_eq!(result.servers.len(), 1);
        let json = serde_json::to_value(&result).unwrap();
        let warnings = json["warnings"]
            .as_array()
            .expect("AwsImportResult must serialize warnings");
        let reasons: Vec<&str> = warnings
            .iter()
            .map(|w| w["reason"].as_str().unwrap())
            .collect();
        for needle in [
            "ca_bundle",
            "use_fips_endpoint",
            "use_dualstack_endpoint",
            "s3.use_accelerate_endpoint",
        ] {
            assert!(
                reasons.iter().any(|r| r.contains(needle)),
                "missing warning for {needle}: {reasons:?}"
            );
        }
    }

    #[test]
    fn role_sso_and_credential_process_skips_name_their_mechanism() {
        let result = import_pair(
            "",
            Some(
                "[profile r]\nrole_arn = arn:aws:iam::123456789012:role/r\n\n\
                 [profile s]\nsso_start_url = https://corp.awsapps.com/start\n\n\
                 [profile p]\ncredential_process = /usr/local/bin/creds\n",
            ),
        )
        .unwrap();
        assert!(result.servers.is_empty());
        let reason_for = |name: &str| {
            result
                .skipped
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.reason.as_str())
                .unwrap_or("")
        };
        assert!(reason_for("r").contains("role_arn"), "{}", reason_for("r"));
        assert!(reason_for("s").contains("SSO"), "{}", reason_for("s"));
        assert!(
            reason_for("p").contains("credential_process"),
            "{}",
            reason_for("p")
        );
    }

    #[test]
    fn write_time_side_selection_uses_first_absent_slot() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let first_side = dir.path().join("config.aeroftp");
        std::fs::write(&config, "user config").unwrap();
        std::fs::write(&first_side, "user side file").unwrap();
        let (_, mode) =
            aws_protected_output_path(&config, None, AWS_CONFIG_MARKER, "AWS config").unwrap();
        assert_eq!(mode, AwsProtectedWriteMode::Side);

        std::fs::remove_file(&first_side).unwrap();
        let written = write_test_protected_config(&config, mode, b"generated settings").unwrap();
        assert!(written.protected);
        assert_eq!(written.path, first_side);
        assert!(!dir.path().join("config.aeroftp.1").exists());
        assert_eq!(std::fs::read(&first_side).unwrap(), b"generated settings");
    }

    #[test]
    fn replace_side_reprobe_preserves_side_file_replaced_by_user() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let side0 = dir.path().join("config.aeroftp");
        std::fs::write(&config, b"[profile user]\nregion = us-east-1\n").unwrap();
        std::fs::write(
            &side0,
            aws_marked_file_bytes(AWS_CONFIG_MARKER, "\n[profile old]\nregion = us-west-1\n"),
        )
        .unwrap();

        let (_, mode) =
            aws_protected_output_path(&config, None, AWS_CONFIG_MARKER, "AWS config").unwrap();
        assert_eq!(mode, AwsProtectedWriteMode::ReplaceSide(0));

        // User replaces side0 between planning and write.
        let user_side = b"user edited side file\n";
        std::fs::write(&side0, user_side).unwrap();

        let written = write_test_protected_config(&config, mode, b"new settings").unwrap();
        assert!(written.protected);
        assert_eq!(written.path, dir.path().join("config.aeroftp.1"));
        assert_eq!(std::fs::read(&side0).unwrap(), user_side);
        assert_eq!(std::fs::read(&written.path).unwrap(), b"new settings");
    }

    #[test]
    fn side_write_retries_on_already_exists_race() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let side0 = dir.path().join("config.aeroftp");
        std::fs::write(&config, b"[profile user]\nregion = us-east-1\n").unwrap();

        let tmp =
            crate::bridge_shared::stage_export_temp_600(dir.path(), b"generated settings").unwrap();
        let mut raced_once = false;
        let written = write_aws_protected_staged_with(
            &config,
            None,
            AWS_CONFIG_MARKER,
            "AWS config",
            tmp,
            AwsProtectedWriteMode::Side,
            crate::bridge_shared::persist_staged_temp,
            |t, p| {
                if !raced_once && p == side0 {
                    raced_once = true;
                    std::fs::write(&side0, b"raced user file").unwrap();
                }
                crate::bridge_shared::persist_staged_temp_noclobber(t, p)
            },
        )
        .unwrap();

        assert!(raced_once);
        assert!(written.protected);
        assert_eq!(written.path, dir.path().join("config.aeroftp.1"));
        assert_eq!(std::fs::read(&side0).unwrap(), b"raced user file");
        assert_eq!(std::fs::read(&written.path).unwrap(), b"generated settings");
    }

    #[test]
    fn default_aws_cli_export_path_uses_private_0700_directory() {
        let path = default_aws_cli_export_path().unwrap();
        let parent = path.parent().expect("parent directory");
        assert_ne!(parent, std::env::temp_dir().as_path());
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("credentials")
        );
        assert!(parent.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(parent).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn partial_write_after_config_persist_reports_written_config_without_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        let config_path = dir.path().join("config");
        let config_bytes =
            aws_marked_file_bytes(AWS_CONFIG_MARKER, "\n[profile lab]\nregion = eu-west-1\n");
        let cred_bytes = aws_marked_file_bytes(
            AWS_CREDENTIALS_MARKER,
            "\n[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = SUPERSECRET99\n",
        );

        let order = std::cell::RefCell::new(Vec::<String>::new());
        let err = persist_aws_export_pair(
            &cred_path,
            &config_path,
            &config_bytes,
            &cred_bytes,
            |kind, tmp, path| {
                order.borrow_mut().push(kind.to_string());
                crate::bridge_shared::persist_staged_temp(tmp, path)
            },
            |kind, tmp, path| {
                order.borrow_mut().push(kind.to_string());
                if kind == "credentials" {
                    return Err(tempfile::PersistError {
                        error: std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "simulated credentials persist failure",
                        ),
                        file: tmp,
                    });
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .err()
        .expect("expected credentials persist failure");

        assert_eq!(*order.borrow(), ["config", "credentials"]);
        assert!(
            config_path.exists(),
            "config should have been persisted first"
        );
        assert!(!cred_path.exists(), "credentials persist failed");
        assert!(
            err.contains(&config_path.display().to_string()),
            "partial write error must report written config path: {err}"
        );
        assert!(
            !err.contains("SUPERSECRET99"),
            "secret leaked into error: {err}"
        );
    }
}
