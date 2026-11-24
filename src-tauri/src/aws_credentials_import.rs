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
#[derive(Debug, serde::Serialize)]
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

// ============ The AWS CLI's reading of its files ============

/// Sections one file may declare; within the 10 MB cap a file could still
/// declare hundreds of thousands, each a vault write on import.
const AWS_MAX_SECTIONS: usize = 10_000;

/// Maximum length of a single imported AWS credential/config field (64 KiB).
/// Bounds pathological `[DEFAULT]` values before they are cloned across profiles.
const MAX_AWS_IMPORT_FIELD_BYTES: usize = 64 * 1024;

/// Maximum aggregate bytes materialized across all imported profiles (32 MiB).
/// Prevents a `<10 MiB` file with a large `[DEFAULT]` value and thousands of
/// profiles from expanding into gigabytes of cloned strings in `AwsImportResult`.
const MAX_AWS_IMPORT_MATERIALIZED_BYTES: usize = 32 * 1024 * 1024;

/// One section as the AWS CLI reads it: plain values, and the values of an
/// indented block under an empty key (`s3 =` / `    addressing_style = path`).
/// `[DEFAULT]` values from the same file are shared via `Arc` and resolved
/// lazily on lookup so a large `[DEFAULT]` with thousands of sections stays
/// O(file size) in memory.
#[derive(Default, Clone, Debug)]
struct AwsSection {
    values: std::collections::HashMap<String, String>,
    nested: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    defaults: Option<std::sync::Arc<AwsSection>>,
}

/// Keys that form an AWS static credential set (botocore
/// `SharedCredentialProvider` vs `ConfigProvider`). These are selected
/// atomically from one file rather than merged field-by-field across
/// `credentials` and `config`.
const AWS_CREDENTIAL_KEYS: &[&str] = &[
    "aws_access_key_id",
    "aws_secret_access_key",
    "aws_session_token",
    "aws_security_token",
];

#[derive(Default, Clone, Copy, Debug)]
struct AwsCredentialRef<'a> {
    access_key_id: Option<&'a str>,
    secret_access_key: Option<&'a str>,
    session_token: Option<&'a str>,
}

impl AwsSection {
    /// Raw lookup preserving an explicit empty value on the section itself so
    /// Raw lookup preserving an explicit empty value (or nested-block override)
    /// on the section itself so an empty or nested override masks `[DEFAULT]`
    /// just like Python `configparser`.
    fn raw_get(&self, key: &str) -> Option<&str> {
        if let Some(v) = self.values.get(key) {
            return Some(v.as_str());
        }
        if self.nested.contains_key(key) {
            return Some("");
        }
        self.defaults.as_deref().and_then(|d| d.raw_get(key))
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.raw_get(key).filter(|v| !v.is_empty())
    }

    fn defines_key(&self, key: &str) -> bool {
        self.values.contains_key(key)
            || self.nested.contains_key(key)
            || self.defaults.as_deref().is_some_and(|d| d.defines_key(key))
    }

    /// Whether the section defines anything at all, inherited `[DEFAULT]`
    /// keys included: a referenced `[services ...]` section with no entry of
    /// its own is unresolved for the AWS CLI, which refuses the client.
    fn has_any_key(&self) -> bool {
        !self.values.is_empty()
            || !self.nested.is_empty()
            || self.defaults.as_deref().is_some_and(|d| d.has_any_key())
    }

    /// Resolve a nested block respecting scalar overrides: an explicit scalar
    /// (such as `s3 =`) on this section masks an inherited `[DEFAULT]` nested
    /// block rather than resurrecting it.
    fn nested_block(&self, block: &str) -> Option<&std::collections::HashMap<String, String>> {
        if let Some(b) = self.nested.get(block) {
            return Some(b);
        }
        if self.values.contains_key(block) {
            return None;
        }
        self.defaults.as_deref().and_then(|d| d.nested_block(block))
    }

    fn nested_get(&self, block: &str, key: &str) -> Option<&str> {
        self.nested_block(block)
            .and_then(|b| b.get(key))
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// Botocore's `SharedCredentialProvider` and `ConfigProvider` select a
    /// section by field presence (`aws_access_key_id` defined, whether scalar,
    /// empty, or nested block); a leftover `aws_session_token` or
    /// `aws_secret_access_key` alone does not shadow a later provider with a
    /// complete key pair, while an explicitly empty or malformed nested
    /// `aws_access_key_id` still claims the section so we fail/skip instead of
    /// silently falling back to a different identity in `config`.
    fn has_access_key_id(&self) -> bool {
        self.defines_key("aws_access_key_id")
    }

    /// Borrow static credential fields without cloning inherited `[DEFAULT]`
    /// values until after skip and size-bound checks succeed.
    fn credential_set(&self) -> AwsCredentialRef<'_> {
        // Match botocore's static credential providers: check `aws_security_token`
        // presence before `aws_session_token`, then filter out empty values so
        // an explicit empty `aws_security_token =` does not resurrect a stale
        // `aws_session_token`.
        AwsCredentialRef {
            access_key_id: self.get("aws_access_key_id"),
            secret_access_key: self.get("aws_secret_access_key"),
            session_token: self
                .raw_get("aws_security_token")
                .or_else(|| self.raw_get("aws_session_token"))
                .filter(|v| !v.is_empty()),
        }
    }
}

/// Zero-copy non-credential view combining a profile's `credentials` section
/// over its `config` section (credentials wins on non-credential keys, while
/// static credential keys stay isolated per file).
struct MergedProfileView<'a> {
    cred: Option<&'a AwsSection>,
    cfg: Option<&'a AwsSection>,
}

impl<'a> MergedProfileView<'a> {
    fn defines_key(&self, key: &str) -> bool {
        self.cred.is_some_and(|s| s.defines_key(key))
            || self.cfg.is_some_and(|s| s.defines_key(key))
    }

    fn get(&self, key: &str) -> Option<&'a str> {
        debug_assert!(!AWS_CREDENTIAL_KEYS.contains(&key));
        self.raw_get(key).filter(|v| !v.is_empty())
    }

    /// Like [`Self::get`] but keeps empty values: distinguishes an omitted key
    /// from a present `key =` with an empty value.
    fn raw_get(&self, key: &str) -> Option<&'a str> {
        debug_assert!(!AWS_CREDENTIAL_KEYS.contains(&key));
        self.cred
            .and_then(|s| s.raw_get(key))
            .or_else(|| self.cfg.and_then(|s| s.raw_get(key)))
    }

    fn nested_get(&self, block: &str, key: &str) -> Option<&'a str> {
        let resolved_block = if let Some(cred) = self.cred {
            if cred.defines_key(block) {
                cred.nested_block(block)
            } else {
                self.cfg.and_then(|s| s.nested_block(block))
            }
        } else {
            self.cfg.and_then(|s| s.nested_block(block))
        };
        resolved_block
            .and_then(|b| b.get(key))
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    fn is_true(&self, key: &str) -> bool {
        self.get(key)
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }
}

/// `content` read the way the AWS CLI reads its files: Python
/// `configparser.RawConfigParser` under botocore's `raw_config_parse`. Section
/// names are verbatim (`[name] ; note` is the section `name`), `=` or `:`
/// ends a key (the first of the two), a line starting with `#` or `;` is a
/// comment, a line indented deeper than its key continues that key's value,
/// and a value that starts on the next line is a sub-section of `key = value`
/// lines. `[DEFAULT]` supplies fallback keys to every ordinary section without
/// being emitted as a section itself. A section or a key declared twice, a
/// line with neither delimiter and a key before any section are what
/// configparser refuses, and with it every AWS CLI command: the import refuses
/// them too. The reason names the line, never its text, which can hold a
/// secret.
fn parse_aws_ini(content: &str, label: &str) -> Result<Vec<(String, AwsSection)>, String> {
    let refuse =
        |line: usize, why: &str| format!("the AWS CLI cannot read {label}: line {line} {why}");
    /// One continuation line of a key's value. A run of blank lines coalesces
    /// into one `Blank(n)` entry: they only add '\n' to the joined value, and
    /// one allocation per blank line would amplify a blank-line-heavy file.
    enum Cont {
        Line(usize, String),
        Blank(usize),
    }
    type RawOptions = Vec<(String, Vec<Cont>)>;
    let mut raw: Vec<(String, RawOptions)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut section_keys = std::collections::HashSet::new();
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
                match lines.last_mut() {
                    Some(Cont::Blank(n)) => *n += 1,
                    _ => lines.push(Cont::Blank(1)),
                }
            }
            continue;
        }
        if let (Some(base), Some((_, lines))) = (key_indent, current) {
            if indent > base {
                lines.push(Cont::Line(lineno, trimmed.to_string()));
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
                section_keys.clear();
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
        if !section_keys.insert(key.clone()) {
            return Err(refuse(lineno, "sets a key its section already set"));
        }
        options.push((
            key,
            vec![Cont::Line(lineno, trimmed[split + 1..].trim().to_string())],
        ));
        key_indent = Some(indent);
    }

    let mut defaults: Option<AwsSection> = None;
    let mut sections = Vec::with_capacity(raw.len());
    for (name, options) in raw {
        let mut section = AwsSection::default();
        for (key, lines) in options {
            // Join the continuation lines without a second Vec of references:
            // a blank run contributes its '\n's from the count alone.
            let mut joined = String::new();
            for (i, cont) in lines.iter().enumerate() {
                if i > 0 {
                    joined.push('\n');
                }
                match cont {
                    Cont::Line(_, s) => joined.push_str(s),
                    // n blank lines: the separator above is the first, n-1 more follow.
                    Cont::Blank(n) => {
                        for _ in 1..*n {
                            joined.push('\n');
                        }
                    }
                }
            }
            let value = joined.trim_end().to_string();
            if value.starts_with('\n') {
                let mut block = std::collections::HashMap::new();
                for cont in lines.iter().skip(1) {
                    let (sub_lineno, sub_line) = match cont {
                        Cont::Line(no, s) => (*no, s.as_str()),
                        Cont::Blank(_) => continue,
                    };
                    let trimmed = sub_line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let Some((k, v)) = trimmed.split_once('=') else {
                        return Err(refuse(sub_lineno, "in a nested block has no `=`"));
                    };
                    block.insert(k.trim().to_string(), v.trim().to_string());
                }
                section.nested.insert(key, block);
            } else {
                section.values.insert(key, value);
            }
        }
        // Python `configparser.DEFAULTSECT` is exact uppercase `DEFAULT`.
        if name == "DEFAULT" {
            defaults = Some(section);
        } else {
            sections.push((name, section));
        }
    }

    if let Some(default_section) = defaults {
        let shared = std::sync::Arc::new(default_section);
        for (_, section) in &mut sections {
            section.defaults = Some(std::sync::Arc::clone(&shared));
        }
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

/// Validate an imported AWS `endpoint_url` (from either a `services` section
/// or the profile itself) before it reaches provider classification, profile
/// options, or any report. Rejects URL userinfo (`user:pass@`), query strings
/// (`?token=...`), fragments, and non-HTTP(S) URLs without ever echoing the
/// raw URL in the skip reason.
fn validate_aws_import_endpoint(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let parsed = url::Url::parse(trimmed)
        .map_err(|_| "its endpoint URL is not a valid HTTP(S) URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none_or(str::is_empty) {
        return Err("its endpoint URL is not a valid HTTP(S) URL".to_string());
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(
            "its endpoint URL carries embedded credentials or query parameters, which AeroFTP refuses to import".to_string(),
        );
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

/// Import with an explicit (optional) config path. Kept separate for
/// testability: `import_aws_credentials` resolves the sibling/env config,
/// while tests inject a known config path or `None`.
pub fn import_aws_credentials_with_config(
    cred_path: &Path,
    config_path: Option<&Path>,
) -> Result<AwsImportResult, String> {
    import_aws_credentials_with_config_limits(
        cred_path,
        config_path,
        crate::bridge_shared::MAX_IMPORT_FILE_BYTES,
        MAX_AWS_IMPORT_MATERIALIZED_BYTES,
    )
}

fn import_aws_credentials_with_config_limits(
    cred_path: &Path,
    config_path: Option<&Path>,
    max_file_bytes: u64,
    max_materialized_bytes: usize,
) -> Result<AwsImportResult, String> {
    // Read both files through a single non-blocking, bounded handle so a FIFO
    // never hangs in `open(2)` and a file that grows or is swapped after an
    // external stat check is still refused at read time.
    let cred = crate::bridge_shared::read_bounded_utf8_file(cred_path, max_file_bytes)
        .map_err(|e| format!("read aws credentials: {e}"))?;
    let cred_sections = parse_aws_ini(&cred, &cred_path.display().to_string())?;

    let mut config = AwsConfig::default();
    if let Some(cp) = config_path {
        if let Some(cfg) = crate::bridge_shared::read_optional_bounded_utf8_file(cp, max_file_bytes)
            .map_err(|e| format!("read aws config: {e}"))?
        {
            config = aws_config_from(parse_aws_ini(&cfg, &cp.display().to_string())?);
        }
    }

    // Keep static credentials isolated per source file (botocore selects
    // `SharedCredentialProvider` when `aws_access_key_id` is present before
    // falling back to `ConfigProvider`, never mixing keys or tokens across
    // files), while merging non-credential settings.
    let services = config.services;
    let config_profiles = config.profiles;
    let mut cred_profiles: std::collections::HashMap<String, AwsSection> =
        std::collections::HashMap::with_capacity(cred_sections.len());
    for (name, section) in cred_sections {
        cred_profiles.insert(name, section);
    }

    let mut name_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    name_set.extend(config_profiles.keys().cloned());
    name_set.extend(cred_profiles.keys().cloned());

    let total_remotes = name_set.len();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();
    let mut warnings = Vec::new();
    let mut materialized_bytes = 0usize;

    for name in name_set {
        let cfg_sec = config_profiles.get(&name);
        let cred_sec = cred_profiles.get(&name);

        let kv = MergedProfileView {
            cred: cred_sec,
            cfg: cfg_sec,
        };

        let (cred_set, creds_from_config) =
            if let Some(s) = cred_sec.filter(|s| s.has_access_key_id()) {
                (s.credential_set(), false)
            } else if let Some(s) = cfg_sec.filter(|s| s.has_access_key_id()) {
                (s.credential_set(), true)
            } else {
                (AwsCredentialRef::default(), false)
            };

        let entry = |reason: String| AwsSkippedRemote {
            name: name.clone(),
            kind: "aws".to_string(),
            reason,
        };
        let mut push_report = |list: &mut Vec<AwsSkippedRemote>,
                               reason: String|
         -> Result<(), String> {
            let bytes = name.len().saturating_add(3).saturating_add(reason.len());
            materialized_bytes = materialized_bytes.saturating_add(bytes);
            if materialized_bytes > max_materialized_bytes {
                return Err(format!(
                        "AWS import expands beyond maximum allowed output size ({max_materialized_bytes} bytes)"
                    ));
            }
            list.push(entry(reason));
            Ok(())
        };
        // The AWS CLI reaches for these before the static keys (botocore's
        // chain: assume-role, web identity and SSO come before the shared
        // credentials file; credential_process comes after shared credentials
        // and before config-file static keys).
        let assumes_role = kv.get("role_arn").is_some();
        let uses_sso = [
            "sso_session",
            "sso_start_url",
            "sso_account_id",
            "sso_role_name",
        ]
        .iter()
        .any(|k| kv.get(k).is_some());
        let has_credential_process = kv.defines_key("credential_process");
        let uses_credential_process = kv.get("credential_process").is_some();

        let akid = match cred_set.access_key_id {
            Some(v) => v,
            None => {
                let why = if assumes_role {
                    ": it assumes a role (role_arn), which AeroFTP does not do; import the profile its source credentials come from"
                } else if uses_sso {
                    ": it signs in through AWS SSO (IAM Identity Center), which AeroFTP does not do"
                } else if uses_credential_process {
                    ": it gets its keys from credential_process, which AeroFTP does not run"
                } else {
                    ""
                };
                push_report(&mut skipped, format!("no aws_access_key_id{why}"))?;
                continue;
            }
        };

        let Some(secret_key) = cred_set.secret_access_key else {
            push_report(
                &mut skipped,
                "no aws_secret_access_key: the AWS CLI rejects partial credentials without a secret key"
                    .to_string(),
            )?;
            continue;
        };

        // botocore `ConfiguredEndpointProvider`: when
        // `ignore_configured_endpoint_urls` is true, skip configured endpoints
        // and any `services` reference altogether; otherwise look up the
        // `services` section's `s3` endpoint before the profile's `endpoint_url`.
        let raw_endpoint_url = if kv.is_true("ignore_configured_endpoint_urls") {
            None
        } else {
            // `raw_get` distinguishes an omitted `services` key from an
            // empty `services =` value: the latter is a present reference to
            // an undefined/empty section, which the AWS CLI refuses, so it
            // must not fall back to the profile-level `endpoint_url`.
            let service = match kv.raw_get("services") {
                Some(section) => match services.get(section) {
                    Some(s) => Some(s),
                    None => {
                        push_report(
                            &mut skipped,
                            "its services section is not defined in the config file, and the AWS CLI refuses the profile"
                                .to_string(),
                        )?;
                        continue;
                    }
                },
                None => None,
            };
            if let Some(svc) = service {
                if !svc.has_any_key() {
                    push_report(
                        &mut skipped,
                        "its referenced services section is empty, and the AWS CLI refuses the profile"
                            .to_string(),
                    )?;
                    continue;
                }
                if svc.defines_key("s3") && svc.nested_block("s3").is_none() {
                    push_report(
                        &mut skipped,
                        "its referenced services section defines s3 as a scalar instead of a nested block, and the AWS CLI refuses the profile"
                            .to_string(),
                    )?;
                    continue;
                }
            }
            service
                .and_then(|s| s.nested_get("s3", "endpoint_url"))
                .or_else(|| kv.get("endpoint_url"))
        };
        // Enforce the per-field length limit on the borrowed endpoint BEFORE
        // parsing it as a URL so a multi-megabyte inherited `[DEFAULT]`
        // endpoint cannot trigger gigabytes of repeated URL parsing across
        // rejected profiles.
        if raw_endpoint_url.is_some_and(|ep| ep.len() > MAX_AWS_IMPORT_FIELD_BYTES) {
            return Err(format!(
                "AWS profile field exceeds maximum allowed length ({MAX_AWS_IMPORT_FIELD_BYTES} bytes)"
            ));
        }
        let endpoint_url = match raw_endpoint_url {
            Some(ep) => match validate_aws_import_endpoint(ep) {
                Ok(clean) => Some(clean),
                Err(reason) => {
                    push_report(&mut skipped, reason)?;
                    continue;
                }
            },
            None => None,
        };

        // An Amazon S3 host with a path prefix (a gateway URL) cannot be
        // represented: AWS stays implicit below and the host drops the
        // prefix, so the imported profile could not reach it. Skip the
        // profile instead of importing a different effective target.
        if let Some(ep) = endpoint_url.as_deref() {
            let after_scheme = ep.split_once("://").map(|(_, rest)| rest).unwrap_or(ep);
            let has_path_prefix = after_scheme
                .split_once('/')
                .is_some_and(|(_, path)| !path.is_empty());
            if has_path_prefix
                && crate::bridge_shared::map_s3_provider_from_endpoint(ep) == "amazon-s3"
            {
                push_report(
                    &mut skipped,
                    "its endpoint is an Amazon S3 URL with a path prefix, which an implicit AWS endpoint cannot represent"
                        .to_string(),
                )?;
                continue;
            }
        }

        if assumes_role {
            push_report(
                &mut warnings,
                "the AWS CLI assumes its role_arn for this profile; AeroFTP signs in with its static keys, as another identity"
                    .to_string(),
            )?;
        }
        if uses_sso {
            push_report(
                &mut warnings,
                "the AWS CLI signs this profile in through AWS SSO before it reads its static keys; AeroFTP uses the static keys"
                    .to_string(),
            )?;
        }
        if creds_from_config && has_credential_process {
            let reason = if uses_credential_process {
                "the AWS CLI runs credential_process before reading static keys from config; AeroFTP signs in with the config static keys, as another identity"
            } else {
                "it sets an empty credential_process, which prevents the AWS CLI from falling back to config static keys; AeroFTP signs in with the config static keys"
            };
            push_report(&mut warnings, reason.to_string())?;
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
                push_report(
                    &mut warnings,
                    format!("it sets {setting}, which AeroFTP does not apply to the connection"),
                )?;
            }
        }

        let (host, port, provider_id) = match endpoint_url.as_deref() {
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

        // Enforce per-field and aggregate materialized output bounds (including
        // the generated profile ID, derived host, and provider ID) before
        // cloning inherited `[DEFAULT]` fields into each `ServerProfileExport`.
        let region_opt = kv.get("region");
        // Charge the actual generated ID length: lowercasing can expand
        // UTF-8 (e.g. U+0130 expands 2 -> 3 bytes), so estimate from the
        // transformed slug, not the original name.
        let id_slug = name.to_lowercase().replace(' ', "-");
        let generated_id_len = "aws-".len() + id_slug.len() + 1 + 8;
        let field_lens = [
            name.len(),
            akid.len(),
            secret_key.len(),
            cred_set.session_token.map_or(0, str::len),
            endpoint_url.as_ref().map_or(0, String::len),
            region_opt.map_or(0, str::len),
        ];
        if field_lens
            .iter()
            .any(|&len| len > MAX_AWS_IMPORT_FIELD_BYTES)
        {
            return Err(format!(
                "AWS profile field exceeds maximum allowed length ({MAX_AWS_IMPORT_FIELD_BYTES} bytes)"
            ));
        }
        let profile_bytes: usize = field_lens
            .iter()
            .copied()
            .sum::<usize>()
            .saturating_add(generated_id_len)
            .saturating_add(host.len())
            .saturating_add(provider_id.len());
        materialized_bytes = materialized_bytes.saturating_add(profile_bytes);
        if materialized_bytes > max_materialized_bytes {
            return Err(format!(
                "AWS import expands beyond maximum allowed output size ({max_materialized_bytes} bytes)"
            ));
        }

        // `endpoint_url` is a validated URL without userinfo or query string.
        // AWS itself stays implicit: an explicit endpoint switches the S3
        // connection to path-style, which AWS refuses for newer buckets.
        let endpoint = endpoint_url
            .filter(|s| crate::bridge_shared::map_s3_provider_from_endpoint(s) != "amazon-s3");
        let mut options = crate::bridge_shared::json_map(&[
            ("region", region_opt.map(str::to_string)),
            ("sessionToken", cred_set.session_token.map(str::to_string)),
            ("endpoint", endpoint),
        ]);
        // The `s3` sub-section's `addressing_style`: compare the borrowed
        // trimmed slice with `eq_ignore_ascii_case` so a large inherited
        // `[DEFAULT]` value never allocates a lowercase copy per profile.
        match kv.nested_get("s3", "addressing_style").map(str::trim) {
            Some(style) if style.eq_ignore_ascii_case("path") => {
                options.insert("pathStyle".into(), serde_json::Value::Bool(true));
            }
            Some(style) if style.eq_ignore_ascii_case("virtual") => {
                options.insert("pathStyle".into(), serde_json::Value::Bool(false));
            }
            _ => {}
        }

        let id = format!("aws-{id_slug}-{}", &crate::bridge_shared::uuid_v4()[..8]);

        servers.push(ServerProfileExport {
            id,
            name: name.clone(),
            host,
            port,
            username: akid.to_string(),
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
/// marker line. Both `config` and `credentials` markers bind to the lossless
/// exact-case bytes of the requested credentials filename so exporting distinct
/// credentials files in the same directory (including case variants on
/// case-sensitive Linux/APFS/NTFS volumes, non-UTF-8 Unix filenames, or
/// `credentials.aeroftp` vs `credentials`) never overwrites another pair's
/// `config` or side-file credentials.
const AWS_MARKER_PREFIX: &str = "# AeroFTP-Export: ";

fn aws_pair_binding_id(cred_path: &Path) -> String {
    // If `cred_path` already resolves to a non-symlink regular file on disk,
    // use the filesystem's stored entry spelling (via `canonicalize`) so a
    // case-only alias on a case-insensitive NTFS/APFS directory (e.g.
    // `CREDENTIALS` referring to an existing `credentials` file) reuses that
    // pair's marker and updates in place, while distinct files on case-sensitive
    // volumes keep distinct markers.
    let canonical_existing = std::fs::symlink_metadata(cred_path)
        .ok()
        .filter(|m| m.is_file())
        .and_then(|_| std::fs::canonicalize(cred_path).ok());
    let effective_path = canonical_existing.as_deref().unwrap_or(cred_path);

    #[cfg(unix)]
    let raw_bytes: &[u8] = {
        use std::os::unix::ffi::OsStrExt;
        effective_path
            .file_name()
            .filter(|n| !n.is_empty())
            .map(OsStrExt::as_bytes)
            .unwrap_or(b"credentials")
    };
    #[cfg(not(unix))]
    let raw_bytes: &[u8] = effective_path
        .file_name()
        .filter(|n| !n.is_empty())
        .map(std::ffi::OsStr::as_encoded_bytes)
        .unwrap_or(b"credentials");
    sha256_hex(raw_bytes)[..16].to_string()
}

fn aws_config_marker_for(cred_path: &Path) -> String {
    format!(
        "# AeroFTP-Export: aws-config cred={} sha256=",
        aws_pair_binding_id(cred_path)
    )
}

fn aws_credentials_marker_for(cred_path: &Path) -> String {
    format!(
        "# AeroFTP-Export: aws-credentials cred={} sha256=",
        aws_pair_binding_id(cred_path)
    )
}

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
    crate::windows_acl::restrict_to_owner(dir.path());
    Ok(dir.keep().join("credentials"))
}

fn aws_side_path(target_path: &Path, index: usize) -> PathBuf {
    let mut name = target_path
        .file_name()
        .filter(|n| !n.is_empty())
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_else(|| std::ffi::OsString::from("aws"));
    if index == 0 {
        name.push(".aeroftp");
    } else {
        name.push(format!(".aeroftp.{index}"));
    }
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
    SideAt(usize),
    ReplaceSide(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ActualWriteAction {
    /// Newly created a previously absent file (`persist_noclobber`).
    CreatedNew,
    /// Replaced an existing AeroFTP-owned file (`persist_replace`), recording
    /// the exact bytes that were present immediately before replacement.
    Replaced { previous_bytes: Vec<u8> },
}

struct ProtectedWriteOutcome {
    path: PathBuf,
    protected: bool,
    existing_was_legacy: bool,
    paired_inactive: bool,
    action: ActualWriteAction,
}

/// Read the bytes of the regular file at `path` through one handle that does
/// not follow a symlink at the final component and does not block on a FIFO,
/// checking `fstat` on that same handle (AWS-07). Returns `None` for symlinks,
/// directories, FIFOs, devices, or files over `MAX_IMPORT_FILE_BYTES`.
fn read_regular_file_no_follow(path: &Path) -> Option<Vec<u8>> {
    read_regular_file_no_follow_bounded(path, crate::bridge_shared::MAX_IMPORT_FILE_BYTES)
}

fn read_regular_file_no_follow_bounded(path: &Path, cap: u64) -> Option<Vec<u8>> {
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
    if !metadata.is_file() || metadata.len() > cap {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= cap).then_some(bytes)
}

fn classify_aws_file_bytes(bytes: &[u8], expected_marker: &str) -> AwsFileOwnership {
    let Ok(content) = std::str::from_utf8(bytes) else {
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

fn probe_aws_file_ownership(path: &Path, expected_marker: &str) -> AwsFileOwnership {
    let Some(bytes) = read_regular_file_no_follow(path) else {
        return AwsFileOwnership::UserOwned;
    };
    classify_aws_file_bytes(&bytes, expected_marker)
}

fn read_owned_aws_file_bytes(path: &Path, expected_marker: &str) -> Option<Vec<u8>> {
    let bytes = read_regular_file_no_follow(path)?;
    (classify_aws_file_bytes(&bytes, expected_marker) == AwsFileOwnership::Owned).then_some(bytes)
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

const AWS_MAX_SIDE_SLOTS: usize = 10_000;

/// Parse the side-slot index out of a directory entry name for `base_path`
/// (`<name>.aeroftp` -> 0, `<name>.aeroftp.<n>` -> n), mirroring
/// [`aws_side_path`].
fn aws_side_index_of(entry_name: &std::ffi::OsStr, base_path: &Path) -> Option<usize> {
    let base_name = base_path.file_name()?.to_str()?;
    let name = entry_name.to_str()?;
    let suffix = name.strip_prefix(base_name)?.strip_prefix(".aeroftp")?;
    if suffix.is_empty() {
        return Some(0);
    }
    suffix.strip_prefix('.')?.parse().ok()
}

/// Select side-file slots for `config` and `credentials` TOGETHER as one
/// matched pair:
/// - Pass 1: reuse the first index where BOTH `config` and `credentials` side
///   files are owned by this pair's markers.
/// - Pass 2: otherwise allocate the first index where BOTH side files are
///   absent, leaving any older partial/asymmetric pair untouched.
fn aws_side_pair_modes_with_limit(
    config_path: &Path,
    cred_path: &Path,
    config_marker: &str,
    cred_marker: &str,
    max_slots: usize,
) -> Result<(AwsProtectedWriteMode, AwsProtectedWriteMode), String> {
    // Enumerate existing directory entries once per parent directory instead
    // of stat-ing all `max_slots` candidates: only existing entries can hold
    // an owned matched pair, and only absent indices can be allocated.
    let mut existing: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for path in [config_path, cred_path] {
        if let Some(parent) = path.parent() {
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    if let Some(index) = aws_side_index_of(&entry.file_name(), path) {
                        existing.insert(index);
                    }
                }
            }
        }
    }
    // Pass 1: reuse the lowest index where BOTH side files exist and are
    // owned by this pair's markers. Existing matched pair wins over any
    // lower free slot.
    for index in existing.iter().copied().filter(|i| *i < max_slots) {
        let cfg_cand = aws_side_path(config_path, index);
        let cred_cand = aws_side_path(cred_path, index);
        if paths_collide(&cfg_cand, Some(cred_path))
            || paths_collide(&cred_cand, Some(config_path))
            || paths_collide(&cfg_cand, Some(&cred_cand))
        {
            continue;
        }
        let cfg_owned = aws_file_is_aeroftp_owned(&cfg_cand, config_marker);
        let cred_owned = aws_file_is_aeroftp_owned(&cred_cand, cred_marker);
        if cfg_owned && cred_owned {
            return Ok((
                AwsProtectedWriteMode::ReplaceSide(index),
                AwsProtectedWriteMode::ReplaceSide(index),
            ));
        }
    }
    // Pass 2: allocate the first index absent from both directories, with no
    // collision against the active paths or between the two candidates. A
    // non-existing candidate cannot be same-file with anything, so textual
    // comparison suffices here.
    for index in 0..max_slots {
        if existing.contains(&index) {
            continue;
        }
        let cfg_cand = aws_side_path(config_path, index);
        let cred_cand = aws_side_path(cred_path, index);
        if cfg_cand == *cred_path || cred_cand == *config_path || cfg_cand == cred_cand {
            continue;
        }
        return Ok((
            AwsProtectedWriteMode::SideAt(index),
            AwsProtectedWriteMode::SideAt(index),
        ));
    }
    Err("no free or reusable matched .aeroftp side-file pair".to_string())
}

fn aws_side_output_path_with_limit(
    target_path: &Path,
    avoid: Option<&Path>,
    expected_marker: &str,
    start_index: usize,
    max_slots: usize,
) -> Result<(PathBuf, AwsProtectedWriteMode), String> {
    let base = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("aws");
    for index in start_index..max_slots {
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

#[cfg(test)]
fn aws_protected_output_path(
    target_path: &Path,
    avoid: Option<&Path>,
    expected_marker: &str,
    label: &str,
) -> Result<(PathBuf, AwsProtectedWriteMode), String> {
    aws_protected_output_path_with_limit(
        target_path,
        avoid,
        expected_marker,
        label,
        AWS_MAX_SIDE_SLOTS,
    )
}

fn aws_protected_output_path_with_limit(
    target_path: &Path,
    avoid: Option<&Path>,
    expected_marker: &str,
    label: &str,
    max_side_slots: usize,
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
    aws_side_output_path_with_limit(target_path, avoid, expected_marker, 0, max_side_slots)
}

/// Serialize cooperating exports targeting the same destination directory
/// (all exports into one folder contend on its sibling `config` and
/// `config.aeroftp*` side slots).
fn aws_export_dir_lock_key(cred_path: &Path) -> String {
    let parent = cred_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canon_parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    canon_parent.to_string_lossy().into_owned()
}

fn aws_export_pair_mutex(key: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut map = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    Arc::clone(
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

struct AwsExportFileLock(std::fs::File);

impl Drop for AwsExportFileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Private per-user directory inside AeroFTP's own data root for export lock
/// files, never the shared `/tmp` where another local user could pre-create a
/// symlink or hold a conflicting lock.
fn aws_export_lock_dir() -> Result<PathBuf, String> {
    let root = crate::portable::try_aeroftp_data_root()
        .map_err(|e| format!("resolve AeroFTP data directory for AWS export lock: {e}"))?;
    let dir = root.join("locks");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create AWS export lock directory {}: {e}", dir.display()))?;
    let meta = std::fs::symlink_metadata(&dir)
        .map_err(|e| format!("inspect AWS export lock directory {}: {e}", dir.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(format!(
            "AWS export lock directory {} is not a regular directory",
            dir.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("secure AWS export lock directory {}: {e}", dir.display()))?;
    }
    crate::windows_acl::restrict_to_owner(&dir);
    Ok(dir)
}

fn aws_export_lock_path(key: &str) -> Result<PathBuf, String> {
    let digest = sha256_hex(key.as_bytes());
    Ok(aws_export_lock_dir()?.join(format!("aws-export-{}.lock", &digest[..16])))
}

fn acquire_aws_export_file_lock(key: &str) -> Result<AwsExportFileLock, String> {
    let lock_path = aws_export_lock_path(key)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: refuse to follow a reparse point.
        options.custom_flags(0x0020_0000);
    }
    let file = options
        .open(&lock_path)
        .map_err(|e| format!("open AWS export lock: {e}"))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("inspect AWS export lock: {e}"))?;
    if !meta.is_file() {
        return Err("AWS export lock path is not a regular file".to_string());
    }
    crate::windows_acl::restrict_to_owner(&lock_path);
    file.lock()
        .map_err(|e| format!("lock AWS export destination directory: {e}"))?;
    Ok(AwsExportFileLock(file))
}

/// Error returned by [`write_aws_protected_staged_with`] when a PAIR-selected
/// side slot (`SideAt`/`ReplaceSide` on a pair write, `paired == true`) collides
/// or loses ownership at write time. The caller must then roll back the already
/// published sibling and replan BOTH files from the pair selector: an
/// independent per-side `Side` scan must never run for a pair write, because it
/// can replace a side file of an older pair and split that pair (C1).
const AWS_PAIR_SLOT_COLLISION: &str = "aws protected pair side slot collision";

#[allow(clippy::too_many_arguments)]
fn write_aws_protected_staged_with(
    target_path: &Path,
    avoid: Option<&Path>,
    expected_marker: &str,
    label: &str,
    mut tmp: tempfile::NamedTempFile,
    initial_mode: AwsProtectedWriteMode,
    existing_was_legacy: bool,
    max_side_slots: usize,
    paired: bool,
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
    macro_rules! bump_retry {
        ($label:expr, $path:expr) => {{
            retries += 1;
            if retries >= MAX_RETRIES {
                return Err(format!(
                    "write {} {}: exhausted retry budget",
                    $label,
                    $path.display()
                ));
            }
        }};
    }
    loop {
        match mode {
            AwsProtectedWriteMode::Replace => {
                // Re-probe and snapshot immediately before atomic rename:
                // narrows the race (AWS-14 residual race against lockless
                // external writers like `aws configure` is inherent on POSIX
                // and explicitly accepted; cooperating AeroFTP exports are
                // serialized by the directory lock) and records the exact
                // pre-replace bytes for rollback even if mode changed from
                // Absent -> Replace after an AlreadyExists race.
                let Some(previous_bytes) = read_owned_aws_file_bytes(target_path, expected_marker)
                else {
                    bump_retry!(label, target_path);
                    (_, mode) = aws_protected_output_path_with_limit(
                        target_path,
                        avoid,
                        expected_marker,
                        label,
                        max_side_slots,
                    )?;
                    continue;
                };
                persist_replace(tmp, target_path)
                    .map_err(|e| format!("write {label} {}: {}", target_path.display(), e.error))?;
                return Ok(ProtectedWriteOutcome {
                    path: target_path.to_path_buf(),
                    protected: false,
                    existing_was_legacy: false,
                    paired_inactive: false,
                    action: ActualWriteAction::Replaced { previous_bytes },
                });
            }
            AwsProtectedWriteMode::ReplaceSide(index) => {
                let candidate = aws_side_path(target_path, index);
                if paths_collide(&candidate, avoid) {
                    if paired {
                        return Err(AWS_PAIR_SLOT_COLLISION.to_string());
                    }
                    bump_retry!(label, &candidate);
                    mode = AwsProtectedWriteMode::Side;
                    continue;
                }
                let Some(previous_bytes) = read_owned_aws_file_bytes(&candidate, expected_marker)
                else {
                    if paired {
                        return Err(AWS_PAIR_SLOT_COLLISION.to_string());
                    }
                    bump_retry!(label, &candidate);
                    mode = AwsProtectedWriteMode::Side;
                    continue;
                };
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
                    paired_inactive: false,
                    action: ActualWriteAction::Replaced { previous_bytes },
                });
            }
            AwsProtectedWriteMode::SideAt(index) => {
                let candidate = aws_side_path(target_path, index);
                if paths_collide(&candidate, avoid) {
                    if paired {
                        return Err(AWS_PAIR_SLOT_COLLISION.to_string());
                    }
                    bump_retry!(label, &candidate);
                    mode = AwsProtectedWriteMode::Side;
                    continue;
                }
                match persist_noclobber(tmp, &candidate) {
                    Ok(()) => {
                        return Ok(ProtectedWriteOutcome {
                            path: candidate,
                            protected: true,
                            existing_was_legacy,
                            paired_inactive: false,
                            action: ActualWriteAction::CreatedNew,
                        });
                    }
                    Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => {
                        if paired {
                            // A pair write never falls back to the independent
                            // per-side scan: report the collision so the caller
                            // rolls back the published sibling and replans both.
                            return Err(AWS_PAIR_SLOT_COLLISION.to_string());
                        }
                        tmp = err.file;
                        retries += 1;
                        if retries >= MAX_RETRIES {
                            return Err(format!(
                                "write {label} side file {}: exhausted retry budget",
                                candidate.display()
                            ));
                        }
                        mode = AwsProtectedWriteMode::Side;
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
            AwsProtectedWriteMode::Side => {
                // Side-file contract: always select the first absent or owned
                // side-file slot from index 0 at write time within `max_side_slots`.
                let (candidate, next_mode) = aws_side_output_path_with_limit(
                    target_path,
                    avoid,
                    expected_marker,
                    0,
                    max_side_slots,
                )?;
                if let AwsProtectedWriteMode::ReplaceSide(index) = next_mode {
                    bump_retry!(label, target_path);
                    mode = AwsProtectedWriteMode::ReplaceSide(index);
                    continue;
                }
                match persist_noclobber(tmp, &candidate) {
                    Ok(()) => {
                        return Ok(ProtectedWriteOutcome {
                            path: candidate,
                            protected: true,
                            existing_was_legacy,
                            paired_inactive: false,
                            action: ActualWriteAction::CreatedNew,
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
                        paired_inactive: false,
                        action: ActualWriteAction::CreatedNew,
                    });
                }
                Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    tmp = err.file;
                    retries += 1;
                    if retries >= MAX_RETRIES {
                        return Err(format!(
                            "write {label} {}: exhausted retry budget",
                            target_path.display()
                        ));
                    }
                    (_, mode) = aws_protected_output_path_with_limit(
                        target_path,
                        avoid,
                        expected_marker,
                        label,
                        max_side_slots,
                    )?;
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

/// Undo a just-published `config` or `credentials` write (active or side file)
/// using its actual write action. Verifies without following symlinks that the
/// file still holds `published_bytes` before modifying or removing it so any
/// detected external edit is preserved instead of clobbered.
fn rollback_written_file(
    written: &ProtectedWriteOutcome,
    published_bytes: &[u8],
    label: &str,
) -> Result<String, String> {
    match read_regular_file_no_follow_bounded(&written.path, published_bytes.len() as u64) {
        Some(current) if current == published_bytes => {}
        _ => {
            return Err(format!(
                "{} was modified after export wrote it; leaving it untouched to preserve newer changes",
                written.path.display()
            ));
        }
    }
    match &written.action {
        ActualWriteAction::Replaced { previous_bytes } => {
            crate::bridge_shared::atomic_write_600(&written.path, previous_bytes)?;
            Ok(format!("previous {label} was restored"))
        }
        ActualWriteAction::CreatedNew => {
            std::fs::remove_file(&written.path)
                .map_err(|e| format!("remove newly written {label}: {e}"))?;
            Ok(format!("newly written {label} was removed"))
        }
    }
}

fn format_error_with_rollback(
    err: String,
    written: &ProtectedWriteOutcome,
    published_bytes: &[u8],
    label: &str,
) -> String {
    match rollback_written_file(written, published_bytes, label) {
        Ok(note) => format!("{err} ({note} at {})", written.path.display()),
        Err(rb_err) => format!(
            "{err} ({label} was written to {} and rollback failed: {rb_err})",
            written.path.display()
        ),
    }
}

fn persist_aws_export_pair(
    cred_path: &Path,
    config_path: &Path,
    config_bytes: &[u8],
    cred_bytes: &[u8],
    persist_replace: impl FnMut(
        &str,
        tempfile::NamedTempFile,
        &Path,
    ) -> Result<(), tempfile::PersistError>,
    persist_noclobber: impl FnMut(
        &str,
        tempfile::NamedTempFile,
        &Path,
    ) -> Result<(), tempfile::PersistError>,
) -> Result<(ProtectedWriteOutcome, ProtectedWriteOutcome), String> {
    persist_aws_export_pair_with_hooks(
        cred_path,
        config_path,
        config_bytes,
        cred_bytes,
        AWS_MAX_SIDE_SLOTS,
        crate::bridge_shared::stage_export_temp_600,
        persist_replace,
        persist_noclobber,
    )
}

#[allow(clippy::too_many_arguments)]
fn persist_aws_export_pair_with_hooks(
    cred_path: &Path,
    config_path: &Path,
    config_bytes: &[u8],
    cred_bytes: &[u8],
    max_side_slots: usize,
    mut stage_temp: impl FnMut(&Path, &[u8]) -> std::io::Result<tempfile::NamedTempFile>,
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
    // Serialize cooperating exports targeting the same destination directory
    // across threads and processes so two exports can never interleave config
    // and credentials from different generations or race on `config.aeroftp*`.
    let dir_key = aws_export_dir_lock_key(cred_path);
    let dir_mutex = aws_export_pair_mutex(&dir_key);
    let _thread_guard = dir_mutex.lock().unwrap_or_else(|e| e.into_inner());
    let _file_guard = acquire_aws_export_file_lock(&dir_key)?;

    let config_marker = aws_config_marker_for(cred_path);
    let cred_marker = aws_credentials_marker_for(cred_path);

    let parent = cred_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    // Plan BOTH outputs before publishing either one: if either destination
    // requires a manual-merge side file, keep BOTH outputs inactive in side
    // files so an active file is never paired with a stale sibling.
    let (_, raw_config_mode) = aws_protected_output_path_with_limit(
        config_path,
        Some(cred_path),
        &config_marker,
        "AWS config",
        max_side_slots,
    )?;
    let (_, raw_cred_mode) = aws_protected_output_path_with_limit(
        cred_path,
        Some(config_path),
        &cred_marker,
        "AWS credentials",
        max_side_slots,
    )?;
    let config_legacy =
        probe_aws_file_ownership(config_path, &config_marker) == AwsFileOwnership::LegacyHeaderOnly;
    let cred_legacy =
        probe_aws_file_ownership(cred_path, &cred_marker) == AwsFileOwnership::LegacyHeaderOnly;

    let config_needs_side = matches!(
        raw_config_mode,
        AwsProtectedWriteMode::Side
            | AwsProtectedWriteMode::SideAt(_)
            | AwsProtectedWriteMode::ReplaceSide(_)
    );
    let cred_needs_side = matches!(
        raw_cred_mode,
        AwsProtectedWriteMode::Side
            | AwsProtectedWriteMode::SideAt(_)
            | AwsProtectedWriteMode::ReplaceSide(_)
    );
    let force_inactive_pair = config_needs_side || cred_needs_side;

    // C1: a pair-selected side slot that collides at write time (an external
    // writer claims it between the plan and the persist) never falls back to an
    // independent per-side scan, which could replace a side file of an older
    // pair and split that pair. Instead the published sibling is rolled back
    // and BOTH files are replanned from the pair selector, advancing the pair
    // to the next free slot together.
    let mut pair_replans = 0usize;
    let mut replan_as_pair = force_inactive_pair;
    let (config_written, cred_written) = loop {
        let (config_mode, cred_mode) = if replan_as_pair {
            aws_side_pair_modes_with_limit(
                config_path,
                cred_path,
                &config_marker,
                &cred_marker,
                max_side_slots,
            )?
        } else {
            (raw_config_mode, raw_cred_mode)
        };

        // AWS-09: stage and sync BOTH 0600 temp files before touching either target.
        let config_tmp = stage_temp(parent, config_bytes)
            .map_err(|e| format!("stage AWS config temp file: {e}"))?;
        let cred_tmp = stage_temp(parent, cred_bytes)
            .map_err(|e| format!("stage AWS credentials temp file: {e}"))?;

        // Persist config first (holds no secrets), then credentials.
        let mut cfg_written = match write_aws_protected_staged_with(
            config_path,
            Some(cred_path),
            &config_marker,
            "AWS config",
            config_tmp,
            config_mode,
            config_legacy,
            max_side_slots,
            replan_as_pair,
            |tmp, path| persist_replace("config", tmp, path),
            |tmp, path| persist_noclobber("config", tmp, path),
        ) {
            Ok(written) => written,
            Err(err) if replan_as_pair && err == AWS_PAIR_SLOT_COLLISION => {
                pair_replans += 1;
                if pair_replans > max_side_slots {
                    return Err(format!(
                        "write AWS protected pair: no free matched .aeroftp side-file pair after {pair_replans} replans"
                    ));
                }
                continue;
            }
            Err(err) => return Err(err),
        };
        if replan_as_pair && !config_needs_side {
            cfg_written.paired_inactive = true;
        }

        // An active-planned config write that redirected to a side file must
        // never be followed by an active credentials write: the two
        // independent side scans could pick different slots and split an
        // older pair. Roll the config write back and replan BOTH files as
        // one matched pair BEFORE any credentials persist hook runs.
        if !replan_as_pair && cfg_written.protected {
            if let Err(rb_err) = rollback_written_file(&cfg_written, config_bytes, "AWS config") {
                return Err(format!(
                    "the AWS config write redirected to side file {} and rolling it back failed: {rb_err}",
                    cfg_written.path.display()
                ));
            }
            pair_replans += 1;
            if pair_replans > max_side_slots {
                return Err(format!(
                    "write AWS protected pair: no free matched .aeroftp side-file pair after {pair_replans} replans"
                ));
            }
            replan_as_pair = true;
            continue;
        }

        let mut cred_written = match write_aws_protected_staged_with(
            cred_path,
            Some(&cfg_written.path),
            &cred_marker,
            "AWS credentials",
            cred_tmp,
            cred_mode,
            cred_legacy,
            max_side_slots,
            replan_as_pair,
            |tmp, path| persist_replace("credentials", tmp, path),
            |tmp, path| persist_noclobber("credentials", tmp, path),
        ) {
            Ok(written) => written,
            Err(err) if replan_as_pair && err == AWS_PAIR_SLOT_COLLISION => {
                // Roll back the published side file, then replan BOTH files as
                // one pair from the selector.
                if let Err(rb_err) = rollback_written_file(&cfg_written, config_bytes, "AWS config")
                {
                    return Err(format!(
                        "an AWS credentials side slot raced after {} was published, and rolling that config write back failed: {rb_err}",
                        cfg_written.path.display()
                    ));
                }
                pair_replans += 1;
                if pair_replans > max_side_slots {
                    return Err(format!(
                        "write AWS protected pair: no free matched .aeroftp side-file pair after {pair_replans} replans"
                    ));
                }
                continue;
            }
            Err(err) => {
                return Err(format_error_with_rollback(
                    err,
                    &cfg_written,
                    config_bytes,
                    "AWS config",
                ));
            }
        };
        if replan_as_pair && !cred_needs_side {
            cred_written.paired_inactive = true;
        }

        // A write planned for an active target can still redirect to a side
        // file at write time (an external writer claimed the active target).
        // The two independent side scans can then pick different slots and
        // split an older pair, so roll back BOTH writes of this attempt and
        // replan through the matched-pair selector. The no-race active write
        // (neither side protected) is kept unchanged.
        if !replan_as_pair && (cfg_written.protected || cred_written.protected) {
            let cred_rb = rollback_written_file(&cred_written, cred_bytes, "AWS credentials");
            let cfg_rb = rollback_written_file(&cfg_written, config_bytes, "AWS config");
            if let Err(cfg_rb_err) = &cfg_rb {
                let base = format!(
                    "a write redirected to a side file after active config was published to {}, and rolling back active config failed: {cfg_rb_err}",
                    cfg_written.path.display()
                );
                return Err(match &cred_rb {
                    Ok(note) => format!("{base} ({note})"),
                    Err(err) => {
                        format!("{base}; rolling back the credentials write failed too: {err}")
                    }
                });
            }
            if let Err(err) = &cred_rb {
                return Err(format!(
                    "rolling back the redirected credentials write {} failed: {err}",
                    cred_written.path.display()
                ));
            }
            pair_replans += 1;
            if pair_replans > max_side_slots {
                return Err(format!(
                    "write AWS protected pair: no free matched .aeroftp side-file pair after {pair_replans} replans"
                ));
            }
            replan_as_pair = true;
            continue;
        }

        break (cfg_written, cred_written);
    };

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
    paired_inactive: bool,
) -> String {
    if paired_inactive {
        format!(
            "{} was kept inactive so the exported pair stays matched; AeroFTP {kind} were written to {}. Merge that file manually",
            target_path.display(),
            side_path.display()
        )
    } else if legacy_header_only {
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
    // Botocore reads `[DEFAULT]` as defaults, not as a named credentials
    // profile, so a profile called DEFAULT must never be written under that
    // section: a second profile without a secret would inherit its keys.
    // Reserve it, and the claim below writes the same safe name in both files.
    let mut names = crate::bridge_shared::ExportEntryNames::exact(servers.iter().map(|s| &s.name))
        .with_taken(["DEFAULT"]);

    for server in servers {
        let (resolved, endpoint_less_path_style) =
            match crate::bridge_shared::resolve_export_endpoint_with_path_style(
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

        // With an endpoint the flag rides on it; without one (AWS itself) it
        // comes back separately, so an explicit path-style setting is kept.
        let path_style = resolved
            .as_ref()
            .map(|ep| ep.s3_path_style)
            .unwrap_or(endpoint_less_path_style);
        let endpoint = resolved.map(|ep| ep.url());
        let profile = names.claim(&server.name);
        // AWS-03: validate the config header before writing credentials, so a
        // refused profile writes nothing to either file.
        let config_header = if region.is_some() || endpoint.is_some() || path_style {
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

    let config_marker = aws_config_marker_for(out);
    let cred_marker = aws_credentials_marker_for(out);
    let config_bytes = aws_marked_file_bytes(&config_marker, &config);
    let cred_bytes = aws_marked_file_bytes(&cred_marker, &cred);
    let (config_written, cred_written) = persist_aws_export_pair(
        out,
        &config_path,
        &config_bytes,
        &cred_bytes,
        |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
        |_, tmp, path| crate::bridge_shared::persist_staged_temp_noclobber(tmp, path),
    )?;
    // `written_path` documents a REDIRECTION: Some only when the credentials
    // actually landed on a side file, not on every active write.
    if cred_written.path != *out {
        outcome.written_path = Some(cred_written.path.clone());
    }

    if config_written.protected {
        outcome.warn(
            "AWS config",
            format_side_file_warning(
                "settings",
                &config_path,
                &config_written.path,
                config_written.existing_was_legacy,
                config_written.paired_inactive,
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
                cred_written.paired_inactive,
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
        // Both inputs live in a dedicated TempDir, never in the shared
        // temp_dir(), and the config path is passed explicitly so
        // AWS_CONFIG_FILE cannot redirect the import.
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        std::fs::write(&cred, CRED_FIXTURE).unwrap();
        let cfg = dir.path().join("config");
        std::fs::write(&cfg, CONFIG_FIXTURE).unwrap();

        let first = import_aws_credentials_with_config(&cred, Some(&cfg)).expect("first import");

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

        let second = import_aws_credentials_with_config(&out, Some(&out.with_file_name("config")))
            .expect("second import");

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
        let newline_refused: AwsExportServer = serde_json::from_value(serde_json::json!({
            "id": "refused", "name": "refused\nprofile", "host": "", "port": 443,
            "username": "KEY", "protocol": "s3", "providerId": "amazon-s3"
        }))
        .unwrap();
        let header_refused: AwsExportServer = serde_json::from_value(serde_json::json!({
            "id": "quote", "name": "lab \"x\"", "host": "", "port": 443,
            "username": "KEY", "protocol": "s3", "providerId": "minio",
            "options": {"endpoint": "https://s3.lab.example.com:9000", "region": "eu-west-1"}
        }))
        .unwrap();
        vec![Vec::new(), vec![newline_refused], vec![header_refused]]
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
        assert!(
            !dir.path().join("credentials").exists(),
            "credentials must stay inactive when config needed a side file"
        );
        assert!(
            std::fs::read_to_string(dir.path().join("credentials.aeroftp.1"))
                .unwrap()
                .contains("[static]\naws_access_key_id = NEWKEY\n")
        );
        assert_eq!(outcome.warnings.len(), 2, "{:?}", outcome.warnings);
        assert_eq!(outcome.warnings[0].name, "AWS config");
        assert!(outcome.warnings[0].reason.contains("config.aeroftp.1"));
        assert!(outcome.warnings[0]
            .reason
            .contains("Merge that file manually"));
        assert_eq!(outcome.warnings[1].name, "AWS credentials");
        assert!(outcome.warnings[1].reason.contains("credentials.aeroftp.1"));
    }

    fn test_config_marker() -> String {
        aws_config_marker_for(Path::new("credentials"))
    }

    fn write_test_protected_config(
        config: &Path,
        mode: AwsProtectedWriteMode,
        bytes: &[u8],
    ) -> Result<ProtectedWriteOutcome, String> {
        let marker = test_config_marker();
        let tmp =
            crate::bridge_shared::stage_export_temp_600(config.parent().unwrap(), bytes).unwrap();
        write_aws_protected_staged_with(
            config,
            None,
            &marker,
            "AWS config",
            tmp,
            mode,
            false,
            AWS_MAX_SIDE_SLOTS,
            false,
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
                &test_config_marker(),
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
            aws_protected_output_path(&config, None, &test_config_marker(), "AWS config").unwrap();
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
        let marker = test_config_marker();
        std::fs::write(
            &config,
            aws_marked_file_bytes(&marker, "\n[profile old]\nregion = us-west-1\n"),
        )
        .unwrap();
        let (_, mode) = aws_protected_output_path(&config, None, &marker, "AWS config").unwrap();
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
        assert_eq!(first.warnings.len(), 2, "{:?}", first.warnings);
        assert!(std::fs::read_to_string(&side)
            .unwrap()
            .contains("region = eu-west-1"));
        assert!(dir.path().join("credentials.aeroftp").exists());
        assert!(!dir.path().join("credentials").exists());
        let second = export_static(dir.path()).unwrap();
        assert_eq!(second.warnings.len(), 2, "{:?}", second.warnings);
        assert_eq!(std::fs::read(&config).unwrap(), original);
        assert!(std::fs::read_to_string(&side)
            .unwrap()
            .starts_with(AWS_EXPORT_HEADER));
        assert!(!dir.path().join("config.aeroftp.1").exists());
        assert!(!dir.path().join("credentials.aeroftp.1").exists());
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
        let out = dir.path().join("config.aeroftp");
        // First export with no user config writes AeroFTP-marked credentials
        // directly to `config.aeroftp` and config to `config`.
        export_static_to(&out).unwrap();
        let cred_at_out = std::fs::read_to_string(&out).unwrap();
        assert!(
            cred_at_out.contains(
                "[static]\naws_access_key_id = NEWKEY\naws_secret_access_key = newsecret\n"
            ),
            "{cred_at_out}"
        );
        assert!(
            !aws_file_is_aeroftp_owned(&out, &aws_config_marker_for(&out)),
            "credentials file must never be recognized as an aws-config export"
        );

        // Now user replaces `config` with their own config. On subsequent
        // exports, `config.aeroftp` (carrying `aws-credentials` marker) must
        // NEVER be treated as an owned config side file or overwritten by
        // config; config must go to `config.aeroftp.1`.
        std::fs::write(
            dir.path().join("config"),
            b"[profile user]\nregion = us-east-1\n",
        )
        .unwrap();
        let config_side = dir.path().join("config.aeroftp.1");
        let cred_side = dir.path().join("config.aeroftp.aeroftp.1");
        for _ in 0..2 {
            export_static_to(&out).unwrap();
            assert_eq!(
                std::fs::read_to_string(&out).unwrap(),
                cred_at_out,
                "existing credentials at config.aeroftp must never be overwritten by config"
            );
            let cfg = std::fs::read_to_string(&config_side).unwrap();
            assert!(
                cfg.contains("[profile static]\nregion = eu-west-1\n"),
                "{cfg}"
            );
            assert!(
                !aws_file_is_aeroftp_owned(&config_side, &aws_credentials_marker_for(&out)),
                "config side file must never be recognized as an aws-credentials export"
            );
            let cred_inactive = std::fs::read_to_string(&cred_side).unwrap();
            assert!(
                cred_inactive.contains(
                    "[static]\naws_access_key_id = NEWKEY\naws_secret_access_key = newsecret\n"
                ),
                "{cred_inactive}"
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
            assert!(!case.path().join("credentials").exists());
            assert!(case.path().join("credentials.aeroftp").exists());
            assert_eq!(
                outcome.warnings.len(),
                2,
                "{target:?}: {:?}",
                outcome.warnings
            );
        }
        assert_eq!(std::fs::read_to_string(&owned_looking).unwrap(), original);

        let dir_case = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir_case.path().join("config")).unwrap();
        let outcome = export_static(dir_case.path()).unwrap();
        assert!(dir_case.path().join("config").is_dir());
        assert!(dir_case.path().join("config.aeroftp").exists());
        assert!(!dir_case.path().join("credentials").exists());
        assert!(dir_case.path().join("credentials.aeroftp").exists());
        assert_eq!(outcome.warnings.len(), 2, "{:?}", outcome.warnings);
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
            aws_protected_output_path(&config, None, &test_config_marker(), "AWS config").unwrap();
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
        let marker = test_config_marker();
        std::fs::write(&config, b"[profile user]\nregion = us-east-1\n").unwrap();
        std::fs::write(
            &side0,
            aws_marked_file_bytes(&marker, "\n[profile old]\nregion = us-west-1\n"),
        )
        .unwrap();

        let (_, mode) = aws_protected_output_path(&config, None, &marker, "AWS config").unwrap();
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
            &test_config_marker(),
            "AWS config",
            tmp,
            AwsProtectedWriteMode::Side,
            false,
            AWS_MAX_SIDE_SLOTS,
            false,
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
        let config_bytes = aws_marked_file_bytes(
            &aws_config_marker_for(&cred_path),
            "\n[profile lab]\nregion = eu-west-1\n",
        );
        let cred_bytes = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred_path),
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
            !config_path.exists(),
            "newly written active config must be rolled back when credentials persist fails"
        );
        assert!(!cred_path.exists(), "credentials persist failed");
        assert!(
            err.contains(&config_path.display().to_string()),
            "partial write error must report config path status: {err}"
        );
        assert!(
            !err.contains("SUPERSECRET99"),
            "secret leaked into error: {err}"
        );
    }

    // ---- CodeRabbit CLI cycle 1 (NAS 2243) regression tests ----

    /// Finding 1: `[DEFAULT]` keys apply to ordinary sections in AWS config
    /// without emitting `DEFAULT` as a profile, and explicit profile values
    /// override inherited defaults.
    #[test]
    fn default_section_values_apply_to_profiles_with_explicit_override() {
        let result = import_pair(
            "[inherited]\naws_access_key_id = AKINH\naws_secret_access_key = inh-secret\n\n\
             [overridden]\naws_access_key_id = AKOVR\naws_secret_access_key = ovr-secret\n",
            Some(
                "[DEFAULT]\nendpoint_url = https://default.lab.example.com:9000\nregion = us-west-2\n\n\
                 [profile inherited]\nregion = eu-west-1\n\n\
                 [profile overridden]\nendpoint_url = https://custom.lab.example.com:9001\n",
            ),
        )
        .unwrap();
        assert_eq!(result.total_remotes, 2);
        assert_eq!(result.servers.len(), 2);
        assert!(result.servers.iter().all(|s| s.name != "DEFAULT"));

        let inh = result
            .servers
            .iter()
            .find(|s| s.name == "inherited")
            .unwrap();
        assert_eq!(inh.host, "default.lab.example.com:9000");
        assert_eq!(
            inh.options.as_ref().unwrap()["region"],
            serde_json::json!("eu-west-1")
        );

        let ovr = result
            .servers
            .iter()
            .find(|s| s.name == "overridden")
            .unwrap();
        assert_eq!(ovr.host, "custom.lab.example.com:9001");
        assert_eq!(
            ovr.options.as_ref().unwrap()["region"],
            serde_json::json!("us-west-2")
        );
    }

    /// Finding 2: if existing user `credentials` contains `lab` and `config`
    /// is absent, BOTH outputs go to inactive `.aeroftp` side files so `config`
    /// is never activated against the user's old `credentials`.
    #[test]
    fn export_keeps_both_files_inactive_when_credentials_needs_side_file() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let cfg = dir.path().join("config");
        let original_cred =
            b"[lab]\naws_access_key_id = AKOLD\naws_secret_access_key = oldsecret\n";
        std::fs::write(&cred, original_cred).unwrap();
        assert!(!cfg.exists());

        let server: AwsExportServer = serde_json::from_value(serde_json::json!({
            "id": "lab", "name": "lab", "host": "", "port": 443,
            "username": "AKNEW", "protocol": "s3", "providerId": "minio",
            "options": {"endpoint": "https://new.lab.example.com:9000", "region": "eu-west-1"}
        }))
        .unwrap();
        let passwords = HashMap::from([("lab".to_string(), "newsecret".to_string())]);
        let outcome = export_aws_credentials(&[server], &passwords, &cred).unwrap();

        assert_eq!(outcome.exported, 1);
        assert_eq!(std::fs::read(&cred).unwrap(), original_cred);
        assert!(
            !cfg.exists(),
            "active config must not be created while credentials needed a side file"
        );
        let side_cfg = std::fs::read_to_string(dir.path().join("config.aeroftp")).unwrap();
        let side_cred = std::fs::read_to_string(dir.path().join("credentials.aeroftp")).unwrap();
        assert!(side_cfg.contains("endpoint_url = https://new.lab.example.com:9000"));
        assert!(side_cred.contains("aws_access_key_id = AKNEW"));
        assert_eq!(outcome.warnings.len(), 2, "{:?}", outcome.warnings);
    }

    /// Finding 3: per-section hash set handles large single-section INI inputs
    /// linearly and still rejects case-insensitive duplicate keys.
    #[test]
    fn large_single_section_parses_linearly_and_rejects_case_insensitive_duplicate_key() {
        let mut ini = String::from("[big]\n");
        for i in 0..10_000 {
            ini.push_str(&format!("k{i} = v{i}\n"));
        }
        let parsed = parse_aws_ini(&ini, "big.ini").unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].1.values.len(), 10_000);

        ini.push_str("K9999 = dup\n");
        let err = parse_aws_ini(&ini, "big.ini").unwrap_err();
        assert!(err.contains("sets a key its section already set"), "{err}");
    }

    /// Finding 4: when replacing an existing active generated pair and
    /// credentials persistence fails after config replacement, the previous
    /// active config is restored so old credentials stay paired with their
    /// original endpoint.
    #[test]
    fn existing_pair_restores_previous_config_when_credentials_persist_fails() {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        let config_path = dir.path().join("config");

        let marker = aws_config_marker_for(&cred_path);
        let cred_marker = aws_credentials_marker_for(&cred_path);
        let old_config = aws_marked_file_bytes(
            &marker,
            "\n[profile lab]\nendpoint_url = https://old.lab.example.com:9000\n",
        );
        let old_cred = aws_marked_file_bytes(
            &cred_marker,
            "\n[lab]\naws_access_key_id = AKOLD\naws_secret_access_key = oldsecret\n",
        );
        std::fs::write(&config_path, &old_config).unwrap();
        std::fs::write(&cred_path, &old_cred).unwrap();

        let new_config = aws_marked_file_bytes(
            &marker,
            "\n[profile lab]\nendpoint_url = https://new.lab.example.com:9000\n",
        );
        let new_cred = aws_marked_file_bytes(
            &cred_marker,
            "\n[lab]\naws_access_key_id = AKNEW\naws_secret_access_key = NEWSECRET42\n",
        );

        let err = persist_aws_export_pair(
            &cred_path,
            &config_path,
            &new_config,
            &new_cred,
            |kind, tmp, path| {
                if kind == "credentials" {
                    return Err(tempfile::PersistError {
                        error: std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "simulated replace failure",
                        ),
                        file: tmp,
                    });
                }
                crate::bridge_shared::persist_staged_temp(tmp, path)
            },
            |_, tmp, path| crate::bridge_shared::persist_staged_temp_noclobber(tmp, path),
        )
        .err()
        .expect("expected credentials replace failure");

        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            old_config,
            "previous config must be restored when credentials replace fails"
        );
        assert_eq!(std::fs::read(&cred_path).unwrap(), old_cred);
        assert!(err.contains("restored"), "{err}");
        assert!(!err.contains("NEWSECRET42"), "{err}");
    }

    /// Finding 6: a `services` or profile `endpoint_url` carrying URL userinfo
    /// or query parameters is refused without echoing any secret into reports
    /// or profile options.
    #[test]
    fn services_and_profile_endpoints_with_userinfo_or_query_are_refused_without_leaking() {
        let result = import_pair(
            "[svc]\naws_access_key_id = AKSVC\naws_secret_access_key = svc-secret\n\n\
             [prof]\naws_access_key_id = AKPROF\naws_secret_access_key = prof-secret\n",
            Some(
                "[profile svc]\nservices = bad-s3\n\n\
                 [services bad-s3]\ns3 =\n  endpoint_url = https://u:USERINFO_SECRET@s3.lab.example.com:9000/?token=QUERY_SECRET\n\n\
                 [profile prof]\nendpoint_url = https://s3.lab.example.com:9000/?sig=PROF_QUERY_SECRET\n",
            ),
        )
        .unwrap();

        assert!(
            result.servers.is_empty(),
            "unsafe endpoints must not import"
        );
        assert_eq!(result.skipped.len(), 2);
        let serialized = serde_json::to_string(&result).unwrap();
        for secret in ["USERINFO_SECRET", "QUERY_SECRET", "PROF_QUERY_SECRET"] {
            assert!(
                !serialized.contains(secret),
                "secret {secret} leaked in import result: {serialized}"
            );
        }
    }

    /// Finding 8: concurrent exports targeting the same destination pair are
    /// serialized so the final config endpoint and credentials key always come
    /// from the same export.
    #[test]
    fn concurrent_exports_to_same_destination_pair_never_mix_config_and_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        let config_path = dir.path().join("config");

        let marker = aws_config_marker_for(&cred_path);
        let cred_marker = aws_credentials_marker_for(&cred_path);
        let cfg_a = aws_marked_file_bytes(
            &marker,
            "\n[profile lab]\nendpoint_url = https://a.lab.example.com:9000\n",
        );
        let cred_a = aws_marked_file_bytes(
            &cred_marker,
            "\n[lab]\naws_access_key_id = AKA\naws_secret_access_key = secreta\n",
        );
        let cfg_b = aws_marked_file_bytes(
            &marker,
            "\n[profile lab]\nendpoint_url = https://b.lab.example.com:9000\n",
        );
        let cred_b = aws_marked_file_bytes(
            &cred_marker,
            "\n[lab]\naws_access_key_id = AKB\naws_secret_access_key = secretb\n",
        );

        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let cred_a_path = cred_path.clone();
        let config_a_path = config_path.clone();

        let thread_a = std::thread::spawn(move || {
            let started_tx = std::cell::RefCell::new(Some(started_tx));
            persist_aws_export_pair(
                &cred_a_path,
                &config_a_path,
                &cfg_a,
                &cred_a,
                |kind, tmp, path| {
                    let res = crate::bridge_shared::persist_staged_temp(tmp, path);
                    if kind == "config" {
                        if let Some(tx) = started_tx.borrow_mut().take() {
                            let _ = tx.send(());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(60));
                    }
                    res
                },
                |kind, tmp, path| {
                    let res = crate::bridge_shared::persist_staged_temp_noclobber(tmp, path);
                    if kind == "config" {
                        if let Some(tx) = started_tx.borrow_mut().take() {
                            let _ = tx.send(());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(60));
                    }
                    res
                },
            )
            .unwrap();
        });

        // Wait until A has persisted config and is paused before persisting
        // credentials; if thread_a fails before the first config callback,
        // `started_tx` drops and `recv()` returns `Err` instead of deadlocking.
        if let Err(err) = started_rx.recv() {
            thread_a.join().unwrap();
            panic!("worker exited before persisting config: {err}");
        }
        persist_aws_export_pair(
            &cred_path,
            &config_path,
            &cfg_b,
            &cred_b,
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |_, tmp, path| crate::bridge_shared::persist_staged_temp_noclobber(tmp, path),
        )
        .unwrap();
        thread_a.join().unwrap();

        let final_cfg = std::fs::read_to_string(&config_path).unwrap();
        let final_cred = std::fs::read_to_string(&cred_path).unwrap();
        let is_pair_a = final_cfg.contains("a.lab.example.com")
            && final_cred.contains("aws_access_key_id = AKA");
        let is_pair_b = final_cfg.contains("b.lab.example.com")
            && final_cred.contains("aws_access_key_id = AKB");
        assert!(
            is_pair_a || is_pair_b,
            "mixed export pair detected!\nconfig:\n{final_cfg}\ncredentials:\n{final_cred}"
        );
    }

    /// Finding 9: static credentials are selected as an atomic set per file
    /// (matching botocore `SharedCredentialProvider` vs `ConfigProvider`):
    /// (a) a complete credentials-file pair never borrows a stale config token,
    /// (b) a partial credentials-file entry is never completed from config,
    /// (c) a complete config-only credential set imports cleanly.
    #[test]
    fn static_credentials_are_selected_as_a_set_per_file() {
        let result = import_pair(
            "[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = labsecret\n\n\
             [partial]\naws_access_key_id = AKPARTIAL\n",
            Some(
                "[profile lab]\naws_access_key_id = AKOLD\naws_secret_access_key = oldsecret\n\
                 aws_session_token = STALE_CONFIG_TOKEN\nregion = eu-west-1\n\n\
                 [profile partial]\naws_secret_access_key = cfgsecret\nregion = eu-west-1\n\n\
                 [profile cfgonly]\naws_access_key_id = AKCFG\naws_secret_access_key = cfgsecret\n\
                 aws_session_token = CFG_TOKEN\nregion = us-west-2\n",
            ),
        )
        .unwrap();

        // `lab` uses credentials-file keys alone (no stale config token) + config region.
        let lab = result.servers.iter().find(|s| s.name == "lab").unwrap();
        assert_eq!(lab.username, "AKLAB");
        assert_eq!(lab.credential.as_deref(), Some("labsecret"));
        assert!(
            lab.options
                .as_ref()
                .and_then(|o| o.get("sessionToken"))
                .is_none(),
            "stale config session token must not be borrowed into complete credentials-file pair: {:?}",
            lab.options
        );
        assert_eq!(
            lab.options.as_ref().unwrap()["region"],
            serde_json::json!("eu-west-1")
        );

        // `partial` is skipped because credentials file has key id without secret.
        assert!(result.servers.iter().all(|s| s.name != "partial"));
        let partial_skip = result
            .skipped
            .iter()
            .find(|s| s.name == "partial")
            .expect("partial profile must be skipped");
        assert!(partial_skip.reason.contains("aws_secret_access_key"));

        // `cfgonly` imports cleanly from config's complete static set.
        let cfgonly = result.servers.iter().find(|s| s.name == "cfgonly").unwrap();
        assert_eq!(cfgonly.username, "AKCFG");
        assert_eq!(cfgonly.credential.as_deref(), Some("cfgsecret"));
        assert_eq!(
            cfgonly.options.as_ref().unwrap()["sessionToken"],
            serde_json::json!("CFG_TOKEN")
        );
    }

    /// Cycle 2 #1: when `config` starts Absent, then an AeroFTP-owned `config`
    /// appears before noclobber (switching the actual write from Absent to
    /// Replace), a subsequent credentials failure restores that replaced config
    /// instead of deleting it.
    #[test]
    fn absent_config_racing_to_replace_restores_replaced_config_on_cred_failure() {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        let config_path = dir.path().join("config");
        let marker = aws_config_marker_for(&cred_path);

        let raced_owned_cfg = aws_marked_file_bytes(
            &marker,
            "\n[profile raced]\nendpoint_url = https://raced.example.com:9000\n",
        );
        let new_cfg = aws_marked_file_bytes(
            &marker,
            "\n[profile new]\nendpoint_url = https://new.example.com:9000\n",
        );
        let new_cred = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred_path),
            "\n[new]\naws_access_key_id = AKNEW\naws_secret_access_key = newsecret\n",
        );

        let injected = std::cell::Cell::new(false);
        let err = persist_aws_export_pair(
            &cred_path,
            &config_path,
            &new_cfg,
            &new_cred,
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |kind, tmp, path| {
                if kind == "config" && !injected.replace(true) {
                    std::fs::write(&config_path, &raced_owned_cfg).unwrap();
                    return crate::bridge_shared::persist_staged_temp_noclobber(tmp, path);
                }
                if kind == "credentials" {
                    return Err(tempfile::PersistError {
                        error: std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "simulated credentials failure",
                        ),
                        file: tmp,
                    });
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .err()
        .expect("expected credentials failure");

        assert!(err.contains("restored"), "{err}");
        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            raced_owned_cfg,
            "config raced from Absent to Replace must be restored, not deleted"
        );
    }

    /// Cycle 2 #2: when credentials race from active to side file after active
    /// `config` was published, and rolling back active `config` fails (e.g.
    /// another writer edited `config` meanwhile), the error is propagated
    /// instead of discarded and the newer `config` edit is preserved.
    #[test]
    fn late_credentials_race_propagates_config_rollback_failure() {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        let config_path = dir.path().join("config");
        let marker = aws_config_marker_for(&cred_path);

        let new_cfg = aws_marked_file_bytes(&marker, "\n[profile lab]\nregion = eu-west-1\n");
        let new_cred = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred_path),
            "\n[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = labsecret\n",
        );
        let external_cfg_edit = b"[profile manual]\nregion = ap-south-1\n";
        let external_cred = b"[user]\naws_access_key_id = AKUSER\naws_secret_access_key = u\n";

        let raced_cred = std::cell::Cell::new(false);
        let err = persist_aws_export_pair(
            &cred_path,
            &config_path,
            &new_cfg,
            &new_cred,
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |kind, tmp, path| {
                if kind == "credentials" && path == cred_path && !raced_cred.replace(true) {
                    // External writer edits active config AND plants user credentials.
                    std::fs::write(&config_path, external_cfg_edit).unwrap();
                    std::fs::write(&cred_path, external_cred).unwrap();
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .err()
        .expect("rollback failure on late credentials race must return Err");

        assert!(
            err.contains("rolling back active config failed"),
            "error must report active config rollback failure: {err}"
        );
        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            external_cfg_edit,
            "external post-publication edit to active config must be preserved"
        );
        assert_eq!(std::fs::read(&cred_path).unwrap(), external_cred);
        assert!(
            !dir.path().join("credentials.aeroftp").exists(),
            "newly written side credentials must be rolled back even when active config rollback fails"
        );
    }

    /// Cycle 2 #3: when credentials persistence fails after a side-file config
    /// write (`config.aeroftp` newly created OR replaced), the side-file config
    /// is rolled back so no mismatched pair is left behind.
    #[test]
    fn side_file_config_is_rolled_back_when_credentials_persist_fails() {
        // Case A: newly created `config.aeroftp` is removed on credentials failure.
        let dir_a = tempfile::tempdir().unwrap();
        let cred_a = dir_a.path().join("credentials");
        let cfg_a = dir_a.path().join("config");
        let cfg_side_a = dir_a.path().join("config.aeroftp");
        std::fs::write(&cfg_a, b"[profile user]\nregion = us-east-1\n").unwrap();

        let marker = aws_config_marker_for(&cred_a);
        let new_cfg = aws_marked_file_bytes(&marker, "\n[profile lab]\nregion = eu-west-1\n");
        let new_cred = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred_a),
            "\n[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = labsecret\n",
        );

        let err_a = persist_aws_export_pair(
            &cred_a,
            &cfg_a,
            &new_cfg,
            &new_cred,
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |kind, tmp, path| {
                if kind == "credentials" {
                    return Err(tempfile::PersistError {
                        error: std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "simulated side credentials failure",
                        ),
                        file: tmp,
                    });
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .err()
        .expect("expected credentials side failure");
        assert!(err_a.contains("removed"), "{err_a}");
        assert!(
            !cfg_side_a.exists(),
            "newly created config.aeroftp must be removed when credentials fail"
        );

        // Case B: previously owned `config.aeroftp` is restored on credentials failure.
        let dir_b = tempfile::tempdir().unwrap();
        let cred_b = dir_b.path().join("credentials");
        let cfg_b = dir_b.path().join("config");
        let cfg_side_b = dir_b.path().join("config.aeroftp");
        let cred_side_b = dir_b.path().join("credentials.aeroftp");
        std::fs::write(&cfg_b, b"[profile user]\nregion = us-east-1\n").unwrap();
        let prev_side_cfg =
            aws_marked_file_bytes(&marker, "\n[profile prev]\nregion = us-west-1\n");
        let prev_side_cred = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred_b),
            "\n[prev]\naws_access_key_id = AKPREV\naws_secret_access_key = prevsecret\n",
        );
        std::fs::write(&cfg_side_b, &prev_side_cfg).unwrap();
        std::fs::write(&cred_side_b, &prev_side_cred).unwrap();

        let err_b = persist_aws_export_pair(
            &cred_b,
            &cfg_b,
            &new_cfg,
            &new_cred,
            |kind, tmp, path| {
                if kind == "credentials" {
                    return Err(tempfile::PersistError {
                        error: std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "simulated side replace failure",
                        ),
                        file: tmp,
                    });
                }
                crate::bridge_shared::persist_staged_temp(tmp, path)
            },
            |_, tmp, path| crate::bridge_shared::persist_staged_temp_noclobber(tmp, path),
        )
        .err()
        .expect("expected credentials side replace failure");
        assert!(err_b.contains("restored"), "{err_b}");
        assert_eq!(
            std::fs::read(&cfg_side_b).unwrap(),
            prev_side_cfg,
            "previous config.aeroftp must be restored when credentials replace fails"
        );
        assert_eq!(std::fs::read(&cred_side_b).unwrap(), prev_side_cred);
    }

    /// Cycle 2 #4: rollback checks that `config` still holds the exact bytes
    /// this export published before overwriting or deleting it, so a concurrent
    /// external edit after publication is preserved.
    #[test]
    fn rollback_preserves_post_publication_config_edit() {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        let config_path = dir.path().join("config");
        let marker = aws_config_marker_for(&cred_path);

        let old_cfg = aws_marked_file_bytes(&marker, "\n[profile old]\nregion = us-west-1\n");
        let old_cred = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred_path),
            "\n[old]\naws_access_key_id = AKOLD\naws_secret_access_key = oldsecret\n",
        );
        std::fs::write(&config_path, &old_cfg).unwrap();
        std::fs::write(&cred_path, &old_cred).unwrap();

        let new_cfg = aws_marked_file_bytes(&marker, "\n[profile new]\nregion = eu-west-1\n");
        let new_cred = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred_path),
            "\n[new]\naws_access_key_id = AKNEW\naws_secret_access_key = newsecret\n",
        );
        let concurrent_user_edit = b"[profile human]\nregion = eu-north-1\n";

        let err = persist_aws_export_pair(
            &cred_path,
            &config_path,
            &new_cfg,
            &new_cred,
            |kind, tmp, path| {
                if kind == "credentials" {
                    // Simulate another tool editing config after publication.
                    std::fs::write(&config_path, concurrent_user_edit).unwrap();
                    return Err(tempfile::PersistError {
                        error: std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "simulated credentials failure",
                        ),
                        file: tmp,
                    });
                }
                crate::bridge_shared::persist_staged_temp(tmp, path)
            },
            |_, tmp, path| crate::bridge_shared::persist_staged_temp_noclobber(tmp, path),
        )
        .err()
        .expect("expected credentials failure");

        assert!(err.contains("rollback failed"), "{err}");
        assert!(err.contains("modified after export wrote it"), "{err}");
        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            concurrent_user_edit,
            "rollback must never overwrite a newer external edit to config"
        );
    }

    /// Cycle 2 #5: exporting `credentials-a`, then `credentials-b`, then
    /// `credentials-c` in the same folder binds each `config` marker to its
    /// paired credentials filename so none overwrites another's `config`.
    #[test]
    fn two_credentials_files_in_same_dir_do_not_clobber_each_others_config() {
        let dir = tempfile::tempdir().unwrap();
        let cred_a = dir.path().join("credentials-a");
        let cred_b = dir.path().join("credentials-b");
        let cred_c = dir.path().join("credentials-c");

        let make_server = |name: &str, region: &str| AwsExportServer {
            id: format!("id-{name}"),
            name: name.to_string(),
            host: format!("{name}.example.com"),
            port: 443,
            username: format!("AK{name}"),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({ "region": region })),
            provider_id: Some("custom-s3".to_string()),
            initial_path: None,
        };
        let pw_a = HashMap::from([("id-alpha".to_string(), "sec-a".to_string())]);
        let pw_b = HashMap::from([("id-beta".to_string(), "sec-b".to_string())]);
        let pw_c = HashMap::from([("id-gamma".to_string(), "sec-c".to_string())]);

        let out_a =
            export_aws_credentials(&[make_server("alpha", "us-east-1")], &pw_a, &cred_a).unwrap();
        assert!(out_a.warnings.is_empty(), "{:?}", out_a.warnings);
        // Active write to the requested path: not a redirection.
        assert_eq!(out_a.written_path, None);
        let cfg_after_a = std::fs::read_to_string(dir.path().join("config")).unwrap();
        assert!(cfg_after_a.contains("[profile alpha]"), "{cfg_after_a}");

        // Second export to a different credentials filename in the same folder
        // must preserve `config` (bound to credentials-a) and emit an inactive pair.
        let out_b =
            export_aws_credentials(&[make_server("beta", "eu-west-1")], &pw_b, &cred_b).unwrap();
        assert_eq!(out_b.warnings.len(), 2, "{:?}", out_b.warnings);
        assert_eq!(
            out_b.written_path.as_deref(),
            Some(dir.path().join("credentials-b.aeroftp").as_path())
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config")).unwrap(),
            cfg_after_a,
            "credentials-b must never overwrite credentials-a's config"
        );
        let cfg_side_b = std::fs::read_to_string(dir.path().join("config.aeroftp")).unwrap();
        assert!(cfg_side_b.contains("[profile beta]"), "{cfg_side_b}");

        // Third export to credentials-c must preserve credentials-b's config.aeroftp too.
        let out_c =
            export_aws_credentials(&[make_server("gamma", "ap-south-1")], &pw_c, &cred_c).unwrap();
        assert_eq!(out_c.warnings.len(), 2, "{:?}", out_c.warnings);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.aeroftp")).unwrap(),
            cfg_side_b,
            "credentials-c must never overwrite credentials-b's config.aeroftp"
        );
        let cfg_side_c = std::fs::read_to_string(dir.path().join("config.aeroftp.1")).unwrap();
        assert!(cfg_side_c.contains("[profile gamma]"), "{cfg_side_c}");
    }

    /// Cycle 2 #6: a malformed line inside a nested block reports only the line
    /// number, never echoing the raw section name, block key, or line contents.
    #[test]
    fn malformed_nested_block_error_never_echoes_raw_key_or_section_name() {
        let ini = "[SECRET_SECTION_99]\nSECRET_BLOCK_KEY =\n    MALFORMED_SECRET_LINE\n";
        let err = parse_aws_ini(ini, "credentials").unwrap_err();
        assert!(err.contains("line 3"), "{err}");
        for forbidden in [
            "SECRET_SECTION_99",
            "SECRET_BLOCK_KEY",
            "MALFORMED_SECRET_LINE",
        ] {
            assert!(
                !err.contains(forbidden),
                "parse error leaked `{forbidden}`: {err}"
            );
        }
    }

    /// Cycle 2 #7: an unresolved `services` reference skip reason never echoes
    /// the raw config value.
    #[test]
    fn unresolved_services_skip_reason_never_echoes_raw_value() {
        let result = import_pair(
            "[svc]\naws_access_key_id = AKSVC\naws_secret_access_key = svcsecret\n",
            Some("[profile svc]\nservices = PRIVATE_SERVICES_TOKEN_77\n"),
        )
        .unwrap();
        assert!(result.servers.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert!(
            !result.skipped[0]
                .reason
                .contains("PRIVATE_SERVICES_TOKEN_77"),
            "skip reason leaked raw services value: {}",
            result.skipped[0].reason
        );
    }

    /// Cycle 2 #8: `[DEFAULT]` is stored once behind an `Arc` and resolved
    /// lazily on lookup rather than cloned into every section.
    #[test]
    fn large_default_section_is_shared_without_per_section_cloning() {
        let big_note = "x".repeat(256 * 1024);
        let mut ini = format!(
            "[DEFAULT]\nregion = eu-central-1\nblob = {big_note}\ns3 =\n    addressing_style = path\n\n"
        );
        for i in 0..500 {
            ini.push_str(&format!(
                "[s{i}]\naws_access_key_id = AK{i}\naws_secret_access_key = sec{i}\n"
            ));
        }
        ini.push_str(
            "[empty_override]\nregion =\naws_access_key_id = AKE\naws_secret_access_key = sece\n",
        );

        let sections = parse_aws_ini(&ini, "credentials").unwrap();
        assert_eq!(sections.len(), 501);
        let first_arc = sections[0].1.defaults.as_ref().expect("shared defaults");
        for (_, sec) in &sections[1..] {
            let other_arc = sec.defaults.as_ref().expect("shared defaults");
            assert!(
                std::sync::Arc::ptr_eq(first_arc, other_arc),
                "all sections must share one Arc<[DEFAULT]> instead of cloning per section"
            );
            assert!(
                !sec.values.contains_key("blob"),
                "[DEFAULT] values must not be copied eagerly into section maps"
            );
        }
        assert_eq!(sections[0].1.get("region"), Some("eu-central-1"));
        assert_eq!(
            sections[0].1.nested_get("s3", "addressing_style"),
            Some("path")
        );
        assert_eq!(
            sections.last().unwrap().1.get("region"),
            None,
            "an explicit empty value must still mask [DEFAULT]"
        );
    }

    /// Cycle 2 #9: a path-prefixed S3 endpoint (such as `/gateway`) is
    /// preserved across export and re-import instead of stripped to its origin.
    #[test]
    fn export_and_reimport_preserve_path_prefixed_s3_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        let servers = vec![AwsExportServer {
            id: "gw-1".to_string(),
            name: "gateway".to_string(),
            host: "s3.lab.example.com".to_string(),
            port: 9000,
            username: "AKGW".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({
                "endpoint": "https://s3.lab.example.com:9000/gateway",
                "region": "us-east-1",
                "pathStyle": true,
            })),
            provider_id: Some("custom-s3".to_string()),
            initial_path: None,
        }];
        let passwords = HashMap::from([("gw-1".to_string(), "gwsecret".to_string())]);

        export_aws_credentials(&servers, &passwords, &cred_path).unwrap();
        let cfg = std::fs::read_to_string(dir.path().join("config")).unwrap();
        assert!(
            cfg.contains("endpoint_url = https://s3.lab.example.com:9000/gateway\n"),
            "export must preserve path-prefixed endpoint URL:\n{cfg}"
        );

        let reimported =
            import_aws_credentials_with_config(&cred_path, Some(&dir.path().join("config")))
                .unwrap();
        assert_eq!(reimported.servers.len(), 1);
        assert_eq!(
            reimported.servers[0]
                .options
                .as_ref()
                .and_then(|o| o.get("endpoint"))
                .and_then(|v| v.as_str()),
            Some("https://s3.lab.example.com:9000/gateway")
        );
    }

    /// `BridgeExportOutcome.written_path` documents a REDIRECTION only: `None`
    /// for an ordinary active write, `Some(actual side path)` when user files
    /// were preserved and the credentials went to `credentials.aeroftp[.N]`.
    #[test]
    fn export_outcome_reports_written_path_only_on_redirection() {
        let dir_active = tempfile::tempdir().unwrap();
        let active = export_static(dir_active.path()).unwrap();
        assert_eq!(
            active.written_path, None,
            "an active write is not a redirection: {:?}",
            active.written_path
        );

        let dir_side = tempfile::tempdir().unwrap();
        std::fs::write(
            dir_side.path().join("credentials"),
            b"[user]\naws_access_key_id = AKUSER\naws_secret_access_key = u\n",
        )
        .unwrap();
        let side = export_static(dir_side.path()).unwrap();
        assert_eq!(
            side.written_path.as_deref(),
            Some(dir_side.path().join("credentials.aeroftp").as_path())
        );
    }

    /// Cycle 2 #18: AWS export lock files live inside AeroFTP's private data
    /// root (`<data_root>/locks`), never in shared `std::env::temp_dir()`.
    #[test]
    fn aws_export_lock_path_lives_in_private_aeroftp_data_root_not_temp_dir() {
        let lock_path = aws_export_lock_path("unit-test-destination-dir").unwrap();
        let lock_dir = lock_path.parent().expect("lock dir");
        let data_root = crate::portable::try_aeroftp_data_root().unwrap();
        assert_eq!(lock_dir, data_root.join("locks").as_path());
        assert!(
            !lock_path.starts_with(std::env::temp_dir()),
            "export lock must never be created in shared temp_dir: {}",
            lock_path.display()
        );
        let guard = acquire_aws_export_file_lock("unit-test-destination-dir").unwrap();
        drop(guard);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(lock_dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            let file_mode = std::fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(file_mode, 0o600);
        }
        let _ = std::fs::remove_file(&lock_path);
    }

    // ---- PR #1613 review round + NAS 2266 regression tests ----

    /// NAS 2266: importing a FIFO at either `credentials` or sibling `config`
    /// fails immediately via O_NONBLOCK + regular-file check instead of hanging
    /// forever in `open(2)`.
    #[cfg(unix)]
    #[test]
    fn import_refuses_fifo_for_credentials_and_config_without_blocking() {
        const CRED_ENV: &str = "AEROFTP_TEST_AWS_FIFO_CRED";
        const CFG_ENV: &str = "AEROFTP_TEST_AWS_FIFO_CFG";
        if let Ok(cred_path) = std::env::var(CRED_ENV) {
            let cfg_path = std::env::var(CFG_ENV).ok();
            let cfg_pb = cfg_path.as_deref().map(PathBuf::from);
            let err = import_aws_credentials_with_config(Path::new(&cred_path), cfg_pb.as_deref())
                .err()
                .expect("FIFO input must be refused");
            assert!(err.contains("regular file"), "{err}");
            return;
        }

        fn make_fifo(path: &Path) {
            let status = std::process::Command::new("mkfifo")
                .arg(path)
                .status()
                .expect("mkfifo command available");
            assert!(status.success(), "mkfifo failed for {}", path.display());
        }

        fn run_in_killable_child(cred: &Path, cfg: Option<&Path>, label: &str) {
            let exe = std::env::current_exe().expect("test binary path");
            let mut cmd = std::process::Command::new(exe);
            cmd.arg("--exact")
                .arg("aws_credentials_import::tests::import_refuses_fifo_for_credentials_and_config_without_blocking")
                .arg("--nocapture")
                .env(CRED_ENV, cred);
            if let Some(c) = cfg {
                cmd.env(CFG_ENV, c);
            } else {
                cmd.env_remove(CFG_ENV);
            }
            let mut child = cmd.spawn().expect("spawn fifo child");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let exit = loop {
                if let Some(status) = child.try_wait().expect("poll child") {
                    break status;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("import hung on {label} FIFO past 5s deadline");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            };
            assert!(
                exit.success(),
                "{label} fifo child failed with status {exit:?}"
            );
        }

        // Case 1: credentials path itself is a FIFO.
        let dir1 = tempfile::tempdir().unwrap();
        let cred_fifo = dir1.path().join("credentials");
        make_fifo(&cred_fifo);
        run_in_killable_child(&cred_fifo, None, "credentials");

        // Case 2: sibling config path is a FIFO while credentials is regular.
        let dir2 = tempfile::tempdir().unwrap();
        let cred_regular = dir2.path().join("credentials");
        let cfg_fifo = dir2.path().join("config");
        std::fs::write(
            &cred_regular,
            "[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = labsecret\n",
        )
        .unwrap();
        make_fifo(&cfg_fifo);
        run_in_killable_child(&cred_regular, Some(&cfg_fifo), "config");
    }

    /// NAS 2266: oversized credentials or config files are refused at read time
    /// by the bounded reader.
    #[test]
    fn import_refuses_oversized_credentials_and_config_at_read_time() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let cfg = dir.path().join("config");
        std::fs::write(
            &cred,
            "[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = labsecret\n",
        )
        .unwrap();
        let cfg_padding = "x".repeat(120);
        std::fs::write(
            &cfg,
            format!("[profile lab]\nregion = eu-west-1\n# {cfg_padding}\n"),
        )
        .unwrap();

        let err_cred = import_aws_credentials_with_config_limits(&cred, None, 16, 1024 * 1024)
            .err()
            .expect("oversized credentials must be refused");
        assert!(err_cred.contains("File too large"), "{err_cred}");

        let err_cfg =
            import_aws_credentials_with_config_limits(&cred, Some(&cfg), 100, 1024 * 1024)
                .err()
                .expect("oversized config must be refused");
        assert!(
            err_cfg.contains("read aws config") && err_cfg.contains("File too large"),
            "{err_cfg}"
        );
    }

    /// Greptile #2 / CodeRabbit #15: on case-sensitive filesystems, `credentials`
    /// and `CREDENTIALS` in the same folder bind distinct config markers so one
    /// never overwrites the other's `config`.
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn case_variant_credentials_filenames_keep_distinct_config_markers() {
        let dir = tempfile::tempdir().unwrap();
        let lower = dir.path().join("credentials");
        let upper = dir.path().join("CREDENTIALS");

        let make_server = |name: &str, region: &str| AwsExportServer {
            id: format!("id-{name}"),
            name: name.to_string(),
            host: format!("{name}.example.com"),
            port: 443,
            username: format!("AK{name}"),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({ "region": region })),
            provider_id: Some("custom-s3".to_string()),
            initial_path: None,
        };
        let pw_a = HashMap::from([("id-lower".to_string(), "sec-a".to_string())]);
        let pw_b = HashMap::from([("id-upper".to_string(), "sec-b".to_string())]);

        let out_a1 =
            export_aws_credentials(&[make_server("lower", "us-east-1")], &pw_a, &lower).unwrap();
        assert!(out_a1.warnings.is_empty(), "{:?}", out_a1.warnings);
        // Re-exporting the exact same `credentials` filename still replaces in place.
        let out_a2 =
            export_aws_credentials(&[make_server("lower", "eu-west-1")], &pw_a, &lower).unwrap();
        assert!(out_a2.warnings.is_empty(), "{:?}", out_a2.warnings);
        let cfg_lower = std::fs::read_to_string(dir.path().join("config")).unwrap();
        assert!(cfg_lower.contains("region = eu-west-1"), "{cfg_lower}");

        // Exporting `CREDENTIALS` in the same folder must NOT overwrite `config`.
        let out_b =
            export_aws_credentials(&[make_server("upper", "ap-south-1")], &pw_b, &upper).unwrap();
        assert_eq!(out_b.warnings.len(), 2, "{:?}", out_b.warnings);
        assert_eq!(
            out_b.written_path.as_deref(),
            Some(dir.path().join("CREDENTIALS.aeroftp").as_path())
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config")).unwrap(),
            cfg_lower,
            "CREDENTIALS export must preserve lower-case credentials's config"
        );
        let cfg_side = std::fs::read_to_string(dir.path().join("config.aeroftp")).unwrap();
        assert!(cfg_side.contains("region = ap-south-1"), "{cfg_side}");
    }

    /// CodeRabbit #14: credentials markers are bound to the active credentials
    /// filename so exporting to `credentials` after a prior direct export to
    /// `credentials.aeroftp` preserves BOTH `config` and `credentials.aeroftp`.
    #[test]
    fn export_to_credentials_preserves_prior_export_written_directly_to_credentials_aeroftp() {
        let dir = tempfile::tempdir().unwrap();
        let direct_side = dir.path().join("credentials.aeroftp");
        let active_cred = dir.path().join("credentials");

        let make_server = |name: &str, region: &str| AwsExportServer {
            id: format!("id-{name}"),
            name: name.to_string(),
            host: format!("{name}.example.com"),
            port: 443,
            username: format!("AK{name}"),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({ "region": region })),
            provider_id: Some("custom-s3".to_string()),
            initial_path: None,
        };
        let pw_a = HashMap::from([("id-alpha".to_string(), "sec-a".to_string())]);
        let pw_b = HashMap::from([("id-beta".to_string(), "sec-b".to_string())]);

        // Export A directly to `credentials.aeroftp`.
        let out_a =
            export_aws_credentials(&[make_server("alpha", "us-east-1")], &pw_a, &direct_side)
                .unwrap();
        assert!(out_a.warnings.is_empty(), "{:?}", out_a.warnings);
        let saved_direct_cred = std::fs::read_to_string(&direct_side).unwrap();
        let saved_active_cfg = std::fs::read_to_string(dir.path().join("config")).unwrap();

        // Export B to `credentials` in the same folder: `config` belongs to
        // `credentials.aeroftp`, so B goes to side files and MUST NOT overwrite
        // `credentials.aeroftp` either (must choose `credentials.aeroftp.1`).
        let out_b1 =
            export_aws_credentials(&[make_server("beta", "eu-west-1")], &pw_b, &active_cred)
                .unwrap();
        assert_eq!(out_b1.warnings.len(), 2, "{:?}", out_b1.warnings);
        assert_eq!(
            out_b1.written_path.as_deref(),
            Some(dir.path().join("credentials.aeroftp.1").as_path())
        );
        assert_eq!(
            std::fs::read_to_string(&direct_side).unwrap(),
            saved_direct_cred,
            "prior direct export at credentials.aeroftp must never be clobbered"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config")).unwrap(),
            saved_active_cfg,
            "prior direct export's config must never be clobbered"
        );
        assert!(std::fs::read_to_string(dir.path().join("config.aeroftp.1"))
            .unwrap()
            .contains("region = eu-west-1"));

        // Re-exporting B to `credentials` reuses its own matched slot 1
        // (`config.aeroftp.1` + `credentials.aeroftp.1`) idempotently while
        // still preserving `credentials.aeroftp`.
        let out_b2 =
            export_aws_credentials(&[make_server("beta", "ap-south-1")], &pw_b, &active_cred)
                .unwrap();
        assert_eq!(
            out_b2.written_path.as_deref(),
            Some(dir.path().join("credentials.aeroftp.1").as_path())
        );
        assert_eq!(
            std::fs::read_to_string(&direct_side).unwrap(),
            saved_direct_cred
        );
        assert!(std::fs::read_to_string(dir.path().join("config.aeroftp.1"))
            .unwrap()
            .contains("region = ap-south-1"));
    }

    /// Greptile #3: when `ignore_configured_endpoint_urls = true`, a missing
    /// `[services ...]` section is ignored rather than skipping the profile.
    #[test]
    fn ignore_configured_endpoint_urls_allows_missing_services_section() {
        let result = import_pair(
            "[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = labsecret\n",
            Some(
                "[profile lab]\nregion = eu-west-1\nignore_configured_endpoint_urls = true\n\
                 services = nonexistent-service\n",
            ),
        )
        .unwrap();
        assert_eq!(
            result.servers.len(),
            1,
            "unexpected skipped count: {}",
            result.skipped.len()
        );
        assert!(result.skipped.is_empty());
        assert_eq!(result.servers[0].host, "");
        assert_eq!(result.servers[0].provider_id.as_deref(), Some("amazon-s3"));
        assert_eq!(
            result.servers[0].options.as_ref().unwrap()["region"],
            serde_json::json!("eu-west-1")
        );
    }

    /// Greptile #4 / CodeRabbit #10: `aws_security_token` precedes
    /// `aws_session_token` (matching botocore `SharedCredentialProvider` /
    /// `ConfigProvider`), while each alias still works on its own.
    #[test]
    fn aws_security_token_takes_precedence_over_aws_session_token() {
        let result = import_pair(
            "[both]\naws_access_key_id = AK1\naws_secret_access_key = sec1\n\
             aws_security_token = SEC_TOKEN_WINS\naws_session_token = SESS_TOKEN_LOSES\n\n\
             [session_only]\naws_access_key_id = AK2\naws_secret_access_key = sec2\n\
             aws_session_token = SESS_ONLY\n\n\
             [security_only]\naws_access_key_id = AK3\naws_secret_access_key = sec3\n\
             aws_security_token = SEC_ONLY\n",
            None,
        )
        .unwrap();

        let token_of = |name: &str| {
            result
                .servers
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .options
                .as_ref()
                .unwrap()["sessionToken"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(token_of("both"), "SEC_TOKEN_WINS");
        assert_eq!(token_of("session_only"), "SESS_ONLY");
        assert_eq!(token_of("security_only"), "SEC_ONLY");
    }

    /// Greptile #5 / CodeRabbit #11: a credentials-file section carrying only
    /// `aws_session_token` or `aws_secret_access_key` (no `aws_access_key_id`)
    /// does not shadow a complete static credential set in `config`, and never
    /// mixes the shared file's stray token into the config credential set.
    #[test]
    fn token_only_credentials_section_falls_back_to_complete_config_credentials() {
        let result = import_pair(
            "[token_only]\naws_session_token = SHARED_ORPHAN_TOKEN\n\n\
             [secret_only]\naws_secret_access_key = SHARED_ORPHAN_SECRET\n\n\
             [key_only]\naws_access_key_id = AKPARTIAL\n",
            Some(
                "[profile token_only]\naws_access_key_id = AKCFG1\naws_secret_access_key = cfgsec1\n\
                 aws_session_token = CFG_TOKEN_1\nregion = eu-west-1\n\n\
                 [profile secret_only]\naws_access_key_id = AKCFG2\naws_secret_access_key = cfgsec2\n\
                 region = us-west-2\n\n\
                 [profile key_only]\naws_access_key_id = AKCFG3\naws_secret_access_key = cfgsec3\n",
            ),
        )
        .unwrap();

        let tok = result
            .servers
            .iter()
            .find(|s| s.name == "token_only")
            .expect("token-only credentials section must fall back to config credentials");
        assert_eq!(tok.username, "AKCFG1");
        assert_eq!(tok.credential.as_deref(), Some("cfgsec1"));
        assert_eq!(
            tok.options.as_ref().unwrap()["sessionToken"],
            serde_json::json!("CFG_TOKEN_1")
        );

        let sec = result
            .servers
            .iter()
            .find(|s| s.name == "secret_only")
            .expect("secret-only credentials section must fall back to config credentials");
        assert_eq!(sec.username, "AKCFG2");
        assert_eq!(sec.credential.as_deref(), Some("cfgsec2"));

        // Key-id-only credentials section still shadows config and is skipped.
        assert!(result.servers.iter().all(|s| s.name != "key_only"));
        assert!(result.skipped.iter().any(|s| s.name == "key_only"));
    }

    /// CodeRabbit #9: an explicit empty scalar `s3 =` masks both inherited
    /// `[DEFAULT]` `s3` nested blocks and a sibling `config` `s3` nested block,
    /// while an omitted `s3` key still inherits normally.
    #[test]
    fn explicit_empty_s3_scalar_masks_inherited_and_config_s3_block() {
        let result = import_pair(
            "[masked_default]\naws_access_key_id = AK1\naws_secret_access_key = sec1\n\n\
             [inherits_default]\naws_access_key_id = AK2\naws_secret_access_key = sec2\n\n\
             [masked_config]\naws_access_key_id = AK3\naws_secret_access_key = sec3\ns3 =\n",
            Some(
                "[DEFAULT]\ns3 =\n  addressing_style = path\n\n\
                 [profile masked_default]\ns3 =\n\n\
                 [profile inherits_default]\nregion = eu-west-1\n\n\
                 [profile masked_config]\ns3 =\n  addressing_style = path\n",
            ),
        )
        .unwrap();

        let path_style_of = |name: &str| {
            result
                .servers
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .options
                .as_ref()
                .and_then(|o| o.get("pathStyle"))
                .and_then(|v| v.as_bool())
        };
        assert_eq!(path_style_of("inherits_default"), Some(true));
        assert_eq!(
            path_style_of("masked_default"),
            None,
            "explicit empty `s3 =` in profile must mask [DEFAULT] s3 block"
        );
        assert_eq!(
            path_style_of("masked_config"),
            None,
            "explicit empty `s3 =` in credentials must mask config s3 block"
        );
    }

    /// CodeRabbit #12: static keys defined in `config` alongside
    /// `credential_process` warn that AWS CLI prefers `credential_process`,
    /// whereas static keys in `credentials` outrank `credential_process` and
    /// do not emit that warning.
    #[test]
    fn credential_process_warns_for_config_keys_but_not_shared_credentials_keys() {
        let result = import_pair(
            "[shared_wins]\naws_access_key_id = AKSHARED\naws_secret_access_key = secshared\n",
            Some(
                "[profile shared_wins]\ncredential_process = /usr/bin/helper\nregion = eu-west-1\n\n\
                 [profile config_loses]\naws_access_key_id = AKCFG\naws_secret_access_key = seccfg\n\
                 credential_process = /usr/bin/helper\nregion = eu-west-1\n",
            ),
        )
        .unwrap();

        assert_eq!(result.servers.len(), 2);
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.name == "config_loses" && w.reason.contains("credential_process")),
            "expected credential_process precedence warning for config_loses (warnings count={})",
            result.warnings.len()
        );
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.name == "shared_wins" && w.reason.contains("credential_process")),
            "shared credentials outrank credential_process and must not warn (warnings count={})",
            result.warnings.len()
        );
    }

    /// CodeRabbit #13: `[DEFAULT]` fan-out cannot amplify materialized imported
    /// data past `max_materialized_bytes`, and individual fields over
    /// `MAX_AWS_IMPORT_FIELD_BYTES` are refused.
    #[test]
    fn import_bounds_materialized_size_for_large_default_session_token() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");

        // Case 1: 32 KiB [DEFAULT] aws_session_token fanned out over 40 profiles
        // (~1.28 MiB materialized) exceeds a 512 KiB materialized budget even
        // though the input file itself is ~35 KiB.
        let tok_32k = "T".repeat(32 * 1024);
        let mut fanout = format!("[DEFAULT]\naws_session_token = {tok_32k}\n\n");
        for i in 0..40 {
            fanout.push_str(&format!(
                "[p{i}]\naws_access_key_id = AK{i}\naws_secret_access_key = sec{i}\n"
            ));
        }
        std::fs::write(&cred, &fanout).unwrap();
        let err_fanout = import_aws_credentials_with_config_limits(
            &cred,
            None,
            crate::bridge_shared::MAX_IMPORT_FILE_BYTES,
            512 * 1024,
        )
        .err()
        .expect("materialized fan-out over budget must fail");
        assert!(
            err_fanout.contains("maximum allowed output size"),
            "{err_fanout}"
        );

        // Case 2: a single field larger than MAX_AWS_IMPORT_FIELD_BYTES (64 KiB).
        let huge_tok = "X".repeat(MAX_AWS_IMPORT_FIELD_BYTES + 1);
        std::fs::write(
            &cred,
            format!(
                "[big]\naws_access_key_id = AKBIG\naws_secret_access_key = secbig\naws_session_token = {huge_tok}\n"
            ),
        )
        .unwrap();
        let err_field = import_aws_credentials_with_config(&cred, None)
            .err()
            .expect("single oversized field must fail");
        assert!(err_field.contains("maximum allowed length"), "{err_field}");
    }

    /// CodeRabbit #16: when active `config` races to a side file and the late
    /// `aws_side_output_path_with_limit` lookup for `credentials` exhausts side
    /// slots, the already-written side `config` is rolled back (deleted if new,
    /// restored if previously owned).
    #[test]
    fn late_credentials_side_slot_exhaustion_rolls_back_redirected_config() {
        // Case A: newly created `config.aeroftp` is deleted when credentials side
        // slots are exhausted.
        let dir_a = tempfile::tempdir().unwrap();
        let cred_a = dir_a.path().join("credentials");
        let cfg_a = dir_a.path().join("config");
        let cfg_side_a = dir_a.path().join("config.aeroftp");
        let cred_side_a = dir_a.path().join("credentials.aeroftp");
        // Pre-occupy `credentials.aeroftp` with an unowned file so max_side_slots = 1 fails.
        std::fs::write(&cred_side_a, b"unowned user side file\n").unwrap();

        let cfg_marker = aws_config_marker_for(&cred_a);
        let cred_marker = aws_credentials_marker_for(&cred_a);
        let new_cfg = aws_marked_file_bytes(&cfg_marker, "\n[profile lab]\nregion = eu-west-1\n");
        let new_cred = aws_marked_file_bytes(
            &cred_marker,
            "\n[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = labsecret\n",
        );

        let raced = std::cell::Cell::new(false);
        let err_a = persist_aws_export_pair_with_hooks(
            &cred_a,
            &cfg_a,
            &new_cfg,
            &new_cred,
            1,
            crate::bridge_shared::stage_export_temp_600,
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |kind, tmp, path| {
                if kind == "config" && path == cfg_a && !raced.replace(true) {
                    // External user creates active config, forcing config onto `config.aeroftp`.
                    std::fs::write(&cfg_a, b"[profile user]\nregion = us-east-1\n").unwrap();
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .err()
        .expect("late credentials side-slot exhaustion must fail");
        // The write-time redirect rolled BOTH writes of the attempt back and
        // replanned through the matched-pair selector; with no free pair slot
        // the selector's own error surfaces, and none of our files remain.
        assert!(
            err_a.contains("no free or reusable matched .aeroftp side-file pair"),
            "{err_a}"
        );
        assert!(
            !cfg_side_a.exists(),
            "newly created config.aeroftp must be removed when the replanned pair has no free slot"
        );
        assert!(
            !cred_a.exists(),
            "the active credentials write of the rolled-back attempt must be removed"
        );

        // Case B: previously owned `config.aeroftp` is restored when credentials
        // side slots are exhausted.
        let dir_b = tempfile::tempdir().unwrap();
        let cred_b = dir_b.path().join("credentials");
        let cfg_b = dir_b.path().join("config");
        let cfg_side_b = dir_b.path().join("config.aeroftp");
        let cred_side_b = dir_b.path().join("credentials.aeroftp");
        let prev_side_cfg =
            aws_marked_file_bytes(&cfg_marker, "\n[profile prev]\nregion = us-west-1\n");
        std::fs::write(&cfg_side_b, &prev_side_cfg).unwrap();
        std::fs::write(&cred_side_b, b"unowned user side file\n").unwrap();

        let raced_b = std::cell::Cell::new(false);
        let err_b = persist_aws_export_pair_with_hooks(
            &cred_b,
            &cfg_b,
            &new_cfg,
            &new_cred,
            1,
            crate::bridge_shared::stage_export_temp_600,
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |kind, tmp, path| {
                if kind == "config" && path == cfg_b && !raced_b.replace(true) {
                    std::fs::write(&cfg_b, b"[profile user]\nregion = us-east-1\n").unwrap();
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .err()
        .expect("late credentials side-slot exhaustion must fail");
        assert!(
            err_b.contains("no free or reusable matched .aeroftp side-file pair"),
            "{err_b}"
        );
        assert_eq!(
            std::fs::read(&cfg_side_b).unwrap(),
            prev_side_cfg,
            "previously owned config.aeroftp must be restored when the replanned pair has no free slot"
        );
    }

    /// CodeRabbit #17: when credentials race from active to an existing owned
    /// side file (`credentials.aeroftp`) after active `config` was published,
    /// and staging the companion side `config` fails, BOTH active `config` AND
    /// the replaced `credentials.aeroftp` are rolled back cleanly.
    #[test]
    fn late_side_config_failure_restores_replaced_side_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        let cfg_path = dir.path().join("config");
        let cred_side = dir.path().join("credentials.aeroftp");
        let cfg_side = dir.path().join("config.aeroftp");

        let cfg_marker = aws_config_marker_for(&cred_path);
        let cred_marker = aws_credentials_marker_for(&cred_path);

        let prev_side_cred = aws_marked_file_bytes(
            &cred_marker,
            "\n[prev]\naws_access_key_id = AKPREV\naws_secret_access_key = prevsecret\n",
        );
        let prev_side_cfg =
            aws_marked_file_bytes(&cfg_marker, "\n[profile prev]\nregion = us-west-1\n");
        std::fs::write(&cred_side, &prev_side_cred).unwrap();
        std::fs::write(&cfg_side, &prev_side_cfg).unwrap();

        let new_cfg = aws_marked_file_bytes(&cfg_marker, "\n[profile new]\nregion = eu-west-1\n");
        let new_cred = aws_marked_file_bytes(
            &cred_marker,
            "\n[new]\naws_access_key_id = AKNEW\naws_secret_access_key = NEWSECRET_LATE\n",
        );
        let external_cred = b"[user]\naws_access_key_id = AKUSER\naws_secret_access_key = u\n";

        let stage_calls = std::cell::Cell::new(0usize);
        let raced = std::cell::Cell::new(false);
        let err = persist_aws_export_pair_with_hooks(
            &cred_path,
            &cfg_path,
            &new_cfg,
            &new_cred,
            64,
            |parent, bytes| {
                let n = stage_calls.get() + 1;
                stage_calls.set(n);
                if n == 3 {
                    // Fail the third staging call (companion side config after credentials race).
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "simulated side config stage failure",
                    ));
                }
                crate::bridge_shared::stage_export_temp_600(parent, bytes)
            },
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |kind, tmp, path| {
                if kind == "credentials" && path == cred_path && !raced.replace(true) {
                    // External user creates active credentials, forcing our credentials
                    // onto the previously owned `credentials.aeroftp`.
                    std::fs::write(&cred_path, external_cred).unwrap();
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .err()
        .expect("side config staging failure must return Err");

        // The write-time redirect rolled both writes back and replanned as one
        // pair; the replan's own staging failure is the error that surfaces.
        assert!(err.contains("stage AWS config temp file"), "{err}");
        assert!(!err.contains("NEWSECRET_LATE"), "{err}");
        assert!(
            !cfg_path.exists(),
            "active config published before credentials race must be rolled back"
        );
        assert_eq!(std::fs::read(&cred_path).unwrap(), external_cred);
        assert_eq!(
            std::fs::read(&cred_side).unwrap(),
            prev_side_cred,
            "previous credentials.aeroftp must be restored when companion side config fails"
        );
        assert_eq!(
            std::fs::read(&cfg_side).unwrap(),
            prev_side_cfg,
            "previous config.aeroftp must remain intact"
        );
    }

    /// CodeRabbit #18: if the first worker fails before reaching its config
    /// persist callback, the mpsc handoff drops its sender and returns an error
    /// immediately rather than hanging on a barrier.
    #[test]
    fn persist_aws_export_pair_error_before_first_config_callback_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        let cfg_path = dir.path().join("config");
        let (tx, rx) = std::sync::mpsc::channel::<()>();

        let worker = std::thread::spawn(move || {
            let tx = std::cell::RefCell::new(Some(tx));
            persist_aws_export_pair_with_hooks(
                &cred_path,
                &cfg_path,
                b"cfg",
                b"cred",
                64,
                |_parent, _bytes| {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "fail before config callback",
                    ))
                },
                |kind, tmp, path| {
                    if kind == "config" {
                        if let Some(sender) = tx.borrow_mut().take() {
                            let _ = sender.send(());
                        }
                    }
                    crate::bridge_shared::persist_staged_temp(tmp, path)
                },
                |kind, tmp, path| {
                    if kind == "config" {
                        if let Some(sender) = tx.borrow_mut().take() {
                            let _ = sender.send(());
                        }
                    }
                    crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
                },
            )
        });

        let recv_res = rx.recv_timeout(std::time::Duration::from_secs(5));
        assert!(
            matches!(
                recv_res,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
            ),
            "channel must disconnect cleanly instead of blocking: {recv_res:?}"
        );
        let worker_err = worker
            .join()
            .unwrap()
            .err()
            .expect("worker must fail before config callback");
        assert!(
            worker_err.contains("fail before config callback"),
            "{worker_err}"
        );
    }

    // ---- CodeRabbit CLI Round 2 Cycle 1 regression tests ----

    /// CR-CLI #1: an explicitly empty `aws_access_key_id =` in `credentials`
    /// still claims the section (matching botocore field-presence selection)
    /// and skips as unusable instead of silently falling back to a different
    /// identity in `config`.
    #[test]
    fn explicit_empty_shared_access_key_does_not_fall_back_to_config() {
        let result = import_pair(
            "[empty_akid]\naws_access_key_id =\naws_secret_access_key = shared_sec\n",
            Some(
                "[profile empty_akid]\naws_access_key_id = AK_OTHER_IDENTITY\n\
                 aws_secret_access_key = other_sec\nregion = eu-west-1\n",
            ),
        )
        .unwrap();
        assert!(
            result.servers.is_empty(),
            "explicit empty aws_access_key_id in credentials must not fall back to config"
        );
        assert!(
            result.skipped.iter().any(|s| s.name == "empty_akid"),
            "expected empty_akid to be skipped"
        );
    }

    /// CR-CLI #3: an explicitly empty `aws_security_token =` takes precedence
    /// over `aws_session_token` and prevents importing a stale session token.
    #[test]
    fn explicit_empty_aws_security_token_masks_stale_session_token() {
        let result = import_pair(
            "[masked]\naws_access_key_id = AKM\naws_secret_access_key = secm\n\
             aws_security_token =\naws_session_token = STALE_TOKEN_MUST_NOT_IMPORT\n",
            None,
        )
        .unwrap();
        assert_eq!(result.servers.len(), 1);
        assert!(
            result.servers[0]
                .options
                .as_ref()
                .and_then(|o| o.get("sessionToken"))
                .is_none(),
            "explicit empty aws_security_token must mask aws_session_token: {:?}",
            result.servers[0].options
        );
    }

    /// CR-CLI #4: a scalar `s3 =` inside a referenced `[services ...]` section
    /// is refused when configured endpoints are active, and ignored when
    /// `ignore_configured_endpoint_urls = true`.
    #[test]
    fn scalar_s3_in_referenced_services_section_is_rejected_unless_endpoints_ignored() {
        let result = import_pair(
            "[strict]\naws_access_key_id = AK1\naws_secret_access_key = sec1\n\n\
             [ignored]\naws_access_key_id = AK2\naws_secret_access_key = sec2\n",
            Some(
                "[DEFAULT]\ns3 =\n  endpoint_url = https://default.example.com:9000\n\n\
                 [services bad-svc]\ns3 =\n\n\
                 [profile strict]\nservices = bad-svc\nregion = eu-west-1\n\n\
                 [profile ignored]\nservices = bad-svc\nignore_configured_endpoint_urls = true\n\
                 region = eu-west-1\n",
            ),
        )
        .unwrap();

        assert!(
            result.servers.iter().all(|s| s.name != "strict"),
            "scalar s3 in referenced services section must be skipped when endpoints are active"
        );
        let skip = result
            .skipped
            .iter()
            .find(|s| s.name == "strict")
            .expect("strict must be skipped");
        assert!(skip.reason.contains("scalar"), "{}", skip.reason);

        let ok = result.servers.iter().find(|s| s.name == "ignored").expect(
            "ignored profile must import cleanly when ignore_configured_endpoint_urls=true",
        );
        assert_eq!(ok.provider_id.as_deref(), Some("amazon-s3"));
    }

    /// An existing but EMPTY referenced `[services ...]` section is unresolved
    /// for the AWS CLI, which refuses the client: skip the profile instead of
    /// falling back to its own `endpoint_url`. A nonempty section that only
    /// defines another service still permits the fallback, and
    /// `ignore_configured_endpoint_urls = true` bypasses both checks.
    #[test]
    fn empty_referenced_services_section_is_rejected_but_other_service_still_falls_back() {
        let cred = "[empty]\naws_access_key_id = AK1\naws_secret_access_key = sec1\n\n\
                    [other]\naws_access_key_id = AK2\naws_secret_access_key = sec2\n\n\
                    [ignored]\naws_access_key_id = AK3\naws_secret_access_key = sec3\n";
        let config = "[services empty-svc]\n\n\
                      [services other-svc]\nec2 =\n  endpoint_url = https://ec2.example.com\n\n\
                      [profile empty]\nservices = empty-svc\nendpoint_url = https://fallback.example.com:9000\nregion = eu-west-1\n\n\
                      [profile other]\nservices = other-svc\nendpoint_url = https://fallback2.example.com:9000\nregion = eu-west-1\n\n\
                      [profile ignored]\nservices = empty-svc\nignore_configured_endpoint_urls = true\nregion = eu-west-1\n";
        let result = import_pair(cred, Some(config)).unwrap();

        assert!(
            result.servers.iter().all(|s| s.name != "empty"),
            "an empty referenced services section must skip the profile: {:?}",
            result.skipped
        );
        let skip = result
            .skipped
            .iter()
            .find(|s| s.name == "empty")
            .expect("empty must be skipped");
        assert!(
            skip.reason.contains("empty"),
            "unexpected skip reason: {}",
            skip.reason
        );

        let other = result
            .servers
            .iter()
            .find(|s| s.name == "other")
            .expect("a nonempty services section for another service still permits the endpoint_url fallback");
        assert_eq!(other.host, "fallback2.example.com:9000");

        let ignored = result
            .servers
            .iter()
            .find(|s| s.name == "ignored")
            .expect("ignore_configured_endpoint_urls must bypass the empty-section check too");
        assert_eq!(ignored.host, "");
        assert_eq!(ignored.provider_id.as_deref(), Some("amazon-s3"));
    }

    /// CR-CLI #2 & #10: `aws_pair_binding_id` preserves exact filename bytes
    /// on all platforms (distinguishing `credentials` vs `CREDENTIALS` even on
    /// macOS case-sensitive APFS) and never collides non-UTF-8 Unix filenames
    /// with literal `credentials`.
    #[test]
    fn aws_pair_binding_id_distinguishes_case_variants_on_all_platforms() {
        assert_ne!(
            aws_config_marker_for(Path::new("/tmp/credentials")),
            aws_config_marker_for(Path::new("/tmp/CREDENTIALS")),
            "case-variant credentials filenames must produce distinct config markers on all platforms"
        );
        assert_ne!(
            aws_credentials_marker_for(Path::new("/tmp/credentials")),
            aws_credentials_marker_for(Path::new("/tmp/CREDENTIALS")),
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_credentials_filename_does_not_collide_with_credentials_config_marker() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().unwrap();
        let non_utf8_name = OsString::from_vec(b"cred\xff".to_vec());
        let non_utf8_path = dir.path().join(&non_utf8_name);
        let literal_cred = dir.path().join("credentials");

        let make_server = |name: &str, region: &str| AwsExportServer {
            id: format!("id-{name}"),
            name: name.to_string(),
            host: format!("{name}.example.com"),
            port: 443,
            username: format!("AK{name}"),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({ "region": region })),
            provider_id: Some("custom-s3".to_string()),
            initial_path: None,
        };
        let pw_a = HashMap::from([("id-raw".to_string(), "sec-a".to_string())]);
        let pw_b = HashMap::from([("id-lit".to_string(), "sec-b".to_string())]);

        let out_a =
            export_aws_credentials(&[make_server("raw", "us-east-1")], &pw_a, &non_utf8_path)
                .unwrap();
        assert!(out_a.warnings.is_empty(), "{:?}", out_a.warnings);
        let cfg_a = std::fs::read_to_string(dir.path().join("config")).unwrap();
        assert!(cfg_a.contains("region = us-east-1"), "{cfg_a}");

        // Exporting to literal `credentials` in the same dir must NOT replace
        // the non-UTF-8 pair's `config`.
        let out_b =
            export_aws_credentials(&[make_server("lit", "eu-west-1")], &pw_b, &literal_cred)
                .unwrap();
        assert_eq!(out_b.warnings.len(), 2, "{:?}", out_b.warnings);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config")).unwrap(),
            cfg_a,
            "literal credentials export must preserve non-UTF-8 pair's config"
        );
    }

    /// CR-CLI #6: rollback verifies full payloads larger than the 10 MiB
    /// import cap, removing unchanged large files and still preserving
    /// externally modified large files.
    #[test]
    fn rollback_written_file_removes_unchanged_large_file_over_10_mib_and_preserves_modified() {
        let dir = tempfile::tempdir().unwrap();
        let large_size = (crate::bridge_shared::MAX_IMPORT_FILE_BYTES as usize) + 64 * 1024;
        let payload = vec![b'A'; large_size];

        // Case A: unchanged >10 MiB newly created file is cleanly removed.
        let path_unchanged = dir.path().join("large_cred.aeroftp");
        std::fs::write(&path_unchanged, &payload).unwrap();
        let outcome_unchanged = ProtectedWriteOutcome {
            path: path_unchanged.clone(),
            protected: true,
            existing_was_legacy: false,
            paired_inactive: true,
            action: ActualWriteAction::CreatedNew,
        };
        let note = rollback_written_file(&outcome_unchanged, &payload, "AWS credentials")
            .expect("rollback must succeed on unchanged >10 MiB file");
        assert!(note.contains("removed"), "{note}");
        assert!(!path_unchanged.exists());

        // Case B: externally modified >10 MiB file is preserved and reports error.
        let path_modified = dir.path().join("large_cred_mod.aeroftp");
        let mut modified = payload.clone();
        modified[large_size - 1] = b'B';
        std::fs::write(&path_modified, &modified).unwrap();
        let outcome_modified = ProtectedWriteOutcome {
            path: path_modified.clone(),
            protected: true,
            existing_was_legacy: false,
            paired_inactive: true,
            action: ActualWriteAction::CreatedNew,
        };
        let err = rollback_written_file(&outcome_modified, &payload, "AWS credentials")
            .expect_err("rollback must refuse modified >10 MiB file");
        assert!(err.contains("modified after export wrote it"), "{err}");
        assert_eq!(std::fs::read(&path_modified).unwrap().len(), large_size);
    }

    /// CR-CLI #12: skipped missing-secret profiles borrow `[DEFAULT]` fields
    /// without cloning them, so a large `[DEFAULT]` token across hundreds of
    /// skipped profiles never amplifies memory or charges the materialized cap.
    #[test]
    fn large_default_session_token_with_many_skipped_profiles_does_not_amplify_memory() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let big_default_token = "T".repeat(512 * 1024);
        let mut ini = format!("[DEFAULT]\naws_session_token = {big_default_token}\n\n");
        for i in 0..400 {
            ini.push_str(&format!("[skip_{i}]\naws_access_key_id = AKSKIP{i}\n"));
        }
        ini.push_str(
            "[valid]\naws_access_key_id = AKVALID\naws_secret_access_key = secvalid\n\
             aws_session_token = SMALL_OK\n",
        );
        std::fs::write(&cred, &ini).unwrap();

        // Tight 64 KiB materialized budget: if skipped profiles had been
        // materialized or charged, 400 * 512 KiB (~200 MiB) would explode.
        let res = import_aws_credentials_with_config_limits(
            &cred,
            None,
            crate::bridge_shared::MAX_IMPORT_FILE_BYTES,
            64 * 1024,
        )
        .expect("skipped profiles must not clone or charge large [DEFAULT] session token");
        assert_eq!(res.skipped.len(), 400);
        assert_eq!(res.servers.len(), 1);
        assert_eq!(res.servers[0].name, "valid");
        assert_eq!(
            res.servers[0].options.as_ref().unwrap()["sessionToken"],
            serde_json::json!("SMALL_OK")
        );
    }

    // ---- CodeRabbit CLI Round 2 Cycle 2 regression tests ----

    /// CR-CLI C2 #1: an oversized inherited `[DEFAULT]` endpoint URL with a
    /// query string is rejected by length BEFORE URL parsing when endpoints
    /// are active, and bypassed cleanly when `ignore_configured_endpoint_urls = true`.
    #[test]
    fn oversized_inherited_endpoint_is_rejected_before_url_parse_unless_endpoints_ignored() {
        let huge_query = "q".repeat(MAX_AWS_IMPORT_FIELD_BYTES + 1024);
        let huge_ep = format!("https://s3.lab.example.com:9000/?token={huge_query}");

        // Active endpoints: must return Err (length cap) before URL validation.
        let err = import_pair(
            "[lab]\naws_access_key_id = AK1\naws_secret_access_key = sec1\n",
            Some(&format!(
                "[DEFAULT]\nendpoint_url = {huge_ep}\n\n[profile lab]\nregion = eu-west-1\n"
            )),
        )
        .err()
        .expect("oversized inherited endpoint must fail length check before URL parsing");
        assert!(err.contains("maximum allowed length"), "{err}");

        // Ignored endpoints: bypasses the oversized endpoint cleanly.
        let ok = import_pair(
            "[lab]\naws_access_key_id = AK1\naws_secret_access_key = sec1\n",
            Some(&format!(
                "[DEFAULT]\nendpoint_url = {huge_ep}\n\n\
                 [profile lab]\nregion = eu-west-1\nignore_configured_endpoint_urls = true\n"
            )),
        )
        .expect("ignore_configured_endpoint_urls=true must bypass oversized inherited endpoint");
        assert_eq!(ok.servers.len(), 1);
        assert_eq!(ok.servers[0].provider_id.as_deref(), Some("amazon-s3"));
    }

    /// CR-CLI C2 #2: a malformed nested `aws_access_key_id` block in `credentials`
    /// still claims the section (matching botocore field presence), masks any
    /// inherited `[DEFAULT]` scalar, and is skipped instead of falling back to
    /// a different identity in `config`.
    #[test]
    fn nested_shared_access_key_does_not_fall_back_to_config_or_default() {
        let result = import_pair(
            "[DEFAULT]\naws_access_key_id = AKDEFAULT\n\n\
             [lab]\naws_access_key_id =\n  sub = nested\naws_secret_access_key = shared_sec\n",
            Some(
                "[profile lab]\naws_access_key_id = AKCONFIG_OTHER\n\
                 aws_secret_access_key = config_sec\nregion = eu-west-1\n",
            ),
        )
        .unwrap();
        assert!(
            result.servers.is_empty(),
            "nested aws_access_key_id block in credentials must never fall back to config or [DEFAULT]"
        );
        assert!(result.skipped.iter().any(|s| s.name == "lab"));
    }

    /// CR-CLI C2 #12: an empty `credential_process =` emits a warning when
    /// falling back to `config` static keys, and emits no warning when shared
    /// `credentials` take precedence.
    #[test]
    fn empty_credential_process_warns_for_config_keys_but_not_shared_keys() {
        let result = import_pair(
            "[shared_ok]\naws_access_key_id = AKSHARED\naws_secret_access_key = secshared\n",
            Some(
                "[profile shared_ok]\ncredential_process =\nregion = eu-west-1\n\n\
                 [profile cfg_empty_cp]\naws_access_key_id = AKCFG\naws_secret_access_key = seccfg\n\
                 credential_process =\nregion = eu-west-1\n",
            ),
        )
        .unwrap();
        assert_eq!(result.servers.len(), 2);
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.name == "cfg_empty_cp" && w.reason.contains("empty credential_process")),
            "empty credential_process with config static keys must warn"
        );
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| w.name == "shared_ok" && w.reason.contains("credential_process")),
            "shared credentials outrank credential_process and must not warn"
        );
    }

    /// CR-CLI C2 #11: the output budget charges retained warning strings and
    /// generated IDs in addition to profile fields, so a file whose profile
    /// fields alone would fit still fails when full output exceeds the budget.
    #[test]
    fn output_budget_charges_warnings_and_generated_ids() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let cfg = dir.path().join("config");
        let long_name = "p".repeat(1200);
        std::fs::write(
            &cred,
            format!("[{long_name}]\naws_access_key_id = AK1\naws_secret_access_key = sec1\n"),
        )
        .unwrap();
        std::fs::write(
            &cfg,
            format!(
                "[profile {long_name}]\nrole_arn = arn:aws:iam::123:role/r\n\
                 sso_start_url = https://sso.example.com\n"
            ),
        )
        .unwrap();

        // Profile fields alone are ~1210 bytes, but 2 warnings (~2600 bytes) +
        // generated ID (~1213 bytes) exceed a 2500-byte output budget.
        let err = import_aws_credentials_with_config_limits(
            &cred,
            Some(&cfg),
            crate::bridge_shared::MAX_IMPORT_FILE_BYTES,
            2500,
        )
        .err()
        .expect("warnings and generated IDs must be charged against output budget");
        assert!(err.contains("maximum allowed output size"), "{err}");
    }

    /// CR-CLI C2 #7: when `max_side_slots = 1` and slot 0 is claimed between
    /// planning and write time, the write retry loop honors `max_side_slots`
    /// and fails without creating slot 1 (`*.aeroftp.1`).
    #[test]
    fn write_time_side_slot_race_enforces_injected_max_side_slots() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let cfg = dir.path().join("config");
        std::fs::write(&cfg, b"[profile user]\nregion = us-east-1\n").unwrap();

        let cfg_marker = aws_config_marker_for(&cred);
        let cred_marker = aws_credentials_marker_for(&cred);
        let new_cfg = aws_marked_file_bytes(&cfg_marker, "\n[profile lab]\nregion = eu-west-1\n");
        let new_cred = aws_marked_file_bytes(
            &cred_marker,
            "\n[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = labsecret\n",
        );

        let raced = std::cell::Cell::new(false);
        let err = persist_aws_export_pair_with_hooks(
            &cred,
            &cfg,
            &new_cfg,
            &new_cred,
            1,
            crate::bridge_shared::stage_export_temp_600,
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |kind, tmp, path| {
                if kind == "config"
                    && path == dir.path().join("config.aeroftp")
                    && !raced.replace(true)
                {
                    // External writer claims slot 0 right before noclobber.
                    std::fs::write(path, b"external unowned slot 0\n").unwrap();
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .err()
        .expect("write-time slot-0 race with max_side_slots=1 must fail");
        assert!(err.contains("no free or reusable"), "{err}");
        assert!(
            !dir.path().join("config.aeroftp.1").exists(),
            "retry loop must respect max_side_slots=1 and never create config.aeroftp.1"
        );
    }

    /// CR-CLI C2 #8: when a lockless external writer edits active `config` AND
    /// creates active `credentials`, causing our credentials to replace an
    /// existing owned `credentials.aeroftp`, active `config` rollback fails
    /// (preserving the external edit) AND `credentials.aeroftp` is still
    /// independently restored so the pre-existing side pair stays matched.
    #[test]
    fn late_credentials_race_restores_replaced_side_credentials_even_when_active_config_rollback_fails(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        let cfg_path = dir.path().join("config");
        let cred_side = dir.path().join("credentials.aeroftp");
        let cfg_side = dir.path().join("config.aeroftp");

        let cfg_marker = aws_config_marker_for(&cred_path);
        let cred_marker = aws_credentials_marker_for(&cred_path);
        let prev_side_cfg =
            aws_marked_file_bytes(&cfg_marker, "\n[profile prev]\nregion = us-west-1\n");
        let prev_side_cred = aws_marked_file_bytes(
            &cred_marker,
            "\n[prev]\naws_access_key_id = AKPREV\naws_secret_access_key = prevsecret\n",
        );
        std::fs::write(&cfg_side, &prev_side_cfg).unwrap();
        std::fs::write(&cred_side, &prev_side_cred).unwrap();

        let new_cfg = aws_marked_file_bytes(&cfg_marker, "\n[profile new]\nregion = eu-west-1\n");
        let new_cred = aws_marked_file_bytes(
            &cred_marker,
            "\n[new]\naws_access_key_id = AKNEW\naws_secret_access_key = newsecret\n",
        );
        let external_cfg_edit = b"[profile human]\nregion = ap-south-1\n";
        let external_cred = b"[human]\naws_access_key_id = AKHUMAN\naws_secret_access_key = h\n";

        let raced = std::cell::Cell::new(false);
        let err = persist_aws_export_pair(
            &cred_path,
            &cfg_path,
            &new_cfg,
            &new_cred,
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |kind, tmp, path| {
                if kind == "credentials" && path == cred_path && !raced.replace(true) {
                    std::fs::write(&cfg_path, external_cfg_edit).unwrap();
                    std::fs::write(&cred_path, external_cred).unwrap();
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .err()
        .expect("late credentials race with active config edit must return Err");

        assert!(err.contains("rolling back active config failed"), "{err}");
        assert!(
            err.contains("previous AWS credentials was restored"),
            "{err}"
        );
        assert_eq!(std::fs::read(&cfg_path).unwrap(), external_cfg_edit);
        assert_eq!(std::fs::read(&cred_path).unwrap(), external_cred);
        assert_eq!(
            std::fs::read(&cred_side).unwrap(),
            prev_side_cred,
            "replaced credentials.aeroftp must be restored even when active config rollback fails"
        );
        assert_eq!(std::fs::read(&cfg_side).unwrap(), prev_side_cfg);
    }

    /// CR-CLI C2 #5: an existing regular file resolves its canonical stored
    /// filename spelling for marker binding, while non-existent distinct
    /// spellings keep their exact requested bytes.
    #[test]
    fn aws_pair_binding_id_preserves_canonical_spelling_for_existing_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let lower = dir.path().join("credentials");
        let upper = dir.path().join("CREDENTIALS");
        std::fs::write(&lower, b"placeholder").unwrap();

        let id_lower = aws_pair_binding_id(&lower);
        // On a case-insensitive filesystem, `upper` resolves to the existing
        // `lower` file and must share its marker ID; on a case-sensitive
        // filesystem, `upper` does not exist and keeps a distinct ID.
        if std::fs::symlink_metadata(&upper).is_ok() {
            assert_eq!(
                aws_pair_binding_id(&upper),
                id_lower,
                "case-only alias on a case-insensitive directory must reuse stored spelling marker"
            );
        } else {
            assert_ne!(
                aws_pair_binding_id(&upper),
                id_lower,
                "distinct spelling on a case-sensitive directory must keep a distinct marker"
            );
        }
    }

    // ---- PR #1613 Round 2 (NAS 2303) regression tests ----

    /// C1: side files are selected and reused as ONE matched pair. If slot 0 is
    /// occupied on one side during export 1 and later freed before export 2,
    /// export 2 reuses BOTH files of export 1's pair (slot 1) instead of
    /// splitting config and credentials across slots; and a legacy asymmetric
    /// pair (`config.aeroftp` + `credentials.aeroftp.1`) is left untouched while
    /// a fresh matched pair is allocated at slot 2.
    #[test]
    fn export_side_pair_stays_matched_when_lower_slot_freed_on_one_side_only() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let cfg = dir.path().join("config");
        std::fs::write(&cfg, b"[profile user]\nregion = us-east-1\n").unwrap();
        // Occupy `credentials.aeroftp` (slot 0) with an unrelated user file.
        let user_slot0 = dir.path().join("credentials.aeroftp");
        std::fs::write(&user_slot0, b"unrelated user file\n").unwrap();

        let make_server = |region: &str| AwsExportServer {
            id: "s1".to_string(),
            name: "lab".to_string(),
            host: "s3.lab.example.com".to_string(),
            port: 443,
            username: "AKLAB".to_string(),
            protocol: Some("s3".to_string()),
            options: Some(serde_json::json!({ "region": region })),
            provider_id: Some("custom-s3".to_string()),
            initial_path: None,
        };
        let pw = HashMap::from([("s1".to_string(), "sec1".to_string())]);

        let out1 = export_aws_credentials(&[make_server("eu-west-1")], &pw, &cred).unwrap();
        assert_eq!(
            out1.written_path.as_deref(),
            Some(dir.path().join("credentials.aeroftp.1").as_path())
        );
        assert!(dir.path().join("config.aeroftp.1").exists());
        assert!(!dir.path().join("config.aeroftp").exists());

        // User frees slot 0 on credentials only.
        std::fs::remove_file(&user_slot0).unwrap();

        // Export 2 MUST reuse BOTH slot-1 files as one matched pair rather than
        // writing a split pair across slot 0 and slot 1.
        let out2 = export_aws_credentials(&[make_server("ap-south-1")], &pw, &cred).unwrap();
        assert_eq!(
            out2.written_path.as_deref(),
            Some(dir.path().join("credentials.aeroftp.1").as_path())
        );
        assert!(!dir.path().join("credentials.aeroftp").exists());
        assert!(!dir.path().join("config.aeroftp").exists());
        assert!(std::fs::read_to_string(dir.path().join("config.aeroftp.1"))
            .unwrap()
            .contains("region = ap-south-1"));

        // Case B: a pre-existing asymmetric pair (`config.aeroftp` +
        // `credentials.aeroftp.1`) is preserved untouched and export allocates a
        // brand-new matched pair at slot 2 (`config.aeroftp.2` + `credentials.aeroftp.2`).
        let dir_b = tempfile::tempdir().unwrap();
        let cred_b = dir_b.path().join("credentials");
        let cfg_b = dir_b.path().join("config");
        std::fs::write(&cfg_b, b"[profile user]\nregion = us-east-1\n").unwrap();
        let old_cfg0 = aws_marked_file_bytes(
            &aws_config_marker_for(&cred_b),
            "\n[profile old]\nregion = us-west-1\n",
        );
        let old_cred1 = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred_b),
            "\n[old]\naws_access_key_id = AKOLD\naws_secret_access_key = oldsec\n",
        );
        std::fs::write(dir_b.path().join("config.aeroftp"), &old_cfg0).unwrap();
        std::fs::write(dir_b.path().join("credentials.aeroftp.1"), &old_cred1).unwrap();

        let out_b = export_aws_credentials(&[make_server("eu-central-1")], &pw, &cred_b).unwrap();
        assert_eq!(
            out_b.written_path.as_deref(),
            Some(dir_b.path().join("credentials.aeroftp.2").as_path())
        );
        assert_eq!(
            std::fs::read(dir_b.path().join("config.aeroftp")).unwrap(),
            old_cfg0,
            "legacy asymmetric config.aeroftp must not be clobbered"
        );
        assert_eq!(
            std::fs::read(dir_b.path().join("credentials.aeroftp.1")).unwrap(),
            old_cred1,
            "legacy asymmetric credentials.aeroftp.1 must not be clobbered"
        );
        assert!(
            std::fs::read_to_string(dir_b.path().join("config.aeroftp.2"))
                .unwrap()
                .contains("region = eu-central-1")
        );
    }

    /// C1 write-time collision: with an old
    /// asymmetric pair (`credentials.aeroftp` + `config.aeroftp.1`) present, the
    /// pair selector picks slot 2 and `config.aeroftp.2` is published; an
    /// external writer then claims `credentials.aeroftp.2`. The credentials
    /// write must NOT fall back to an independent per-side scan that replaces
    /// the owned `credentials.aeroftp` of the old pair: the published config
    /// side is rolled back and BOTH files are replanned together to the next
    /// free pair slot. The old pair is never split.
    #[test]
    fn export_side_pair_survives_write_time_collision_without_splitting_old_pair() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let cfg = dir.path().join("config");
        // Active targets are user files, so the export must go to side files.
        std::fs::write(&cfg, b"[profile user]\nregion = us-east-1\n").unwrap();
        std::fs::write(&cred, b"[user]\naws_access_key_id = AKUSER\n").unwrap();
        // Old asymmetric pair: owned `credentials.aeroftp` + owned `config.aeroftp.1`.
        let old_cred0 = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred),
            "\n[old]\naws_access_key_id = AKOLD\naws_secret_access_key = oldsec\n",
        );
        let old_cfg1 = aws_marked_file_bytes(
            &aws_config_marker_for(&cred),
            "\n[profile old]\nregion = us-west-1\n",
        );
        std::fs::write(dir.path().join("credentials.aeroftp"), &old_cred0).unwrap();
        std::fs::write(dir.path().join("config.aeroftp.1"), &old_cfg1).unwrap();

        let new_cfg = aws_marked_file_bytes(
            &aws_config_marker_for(&cred),
            "\n[profile new]\nregion = eu-west-1\n",
        );
        let new_cred = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred),
            "\n[new]\naws_access_key_id = AKNEW\naws_secret_access_key = newsec\n",
        );
        let external = b"[external]\naws_access_key_id = AKEXT\n";

        let collided = std::cell::Cell::new(false);
        let (cfg_out, cred_out) = persist_aws_export_pair(
            &cred,
            &cfg,
            &new_cfg,
            &new_cred,
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |kind, tmp, path| {
                // The external writer claims the pair slot for credentials
                // between the config publish and the credentials write.
                if kind == "credentials"
                    && path.file_name().and_then(|n| n.to_str()) == Some("credentials.aeroftp.2")
                    && !collided.replace(true)
                {
                    std::fs::write(path, external).unwrap();
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .unwrap();

        assert!(collided.get(), "the injected collision must have fired");
        // The old pair is untouched: never split, never replaced.
        assert_eq!(
            std::fs::read(dir.path().join("credentials.aeroftp")).unwrap(),
            old_cred0,
            "owned credentials.aeroftp of the old pair must not be replaced"
        );
        assert_eq!(
            std::fs::read(dir.path().join("config.aeroftp.1")).unwrap(),
            old_cfg1,
            "owned config.aeroftp.1 of the old pair must not be replaced"
        );
        // The external claim on the slot is preserved.
        assert_eq!(
            std::fs::read(dir.path().join("credentials.aeroftp.2")).unwrap(),
            external
        );
        // The published config side at slot 2 was rolled back...
        assert!(!dir.path().join("config.aeroftp.2").exists());
        // ...and both new outputs landed together at the next free pair slot (3).
        assert_eq!(cfg_out.path, dir.path().join("config.aeroftp.3"));
        assert_eq!(cred_out.path, dir.path().join("credentials.aeroftp.3"));
        assert_eq!(std::fs::read(&cfg_out.path).unwrap(), new_cfg);
        assert_eq!(std::fs::read(&cred_out.path).unwrap(), new_cred);
        assert!(aws_file_is_aeroftp_owned(
            &cfg_out.path,
            &aws_config_marker_for(&cred)
        ));
        assert!(aws_file_is_aeroftp_owned(
            &cred_out.path,
            &aws_credentials_marker_for(&cred)
        ));
    }

    /// An active-target write that redirects to a side file at write time (an
    /// external writer claimed the active `config`) must roll back BOTH writes
    /// of the attempt and replan through the matched-pair selector, so the new
    /// pair lands on ONE slot and an older pair is never split. Unequal slot
    /// occupancy: `credentials.aeroftp` is a user file, `credentials.aeroftp.1`
    /// holds an older owned side file.
    #[test]
    fn config_redirect_replans_both_files_as_one_pair() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let cfg = dir.path().join("config");
        // Both active targets start absent (planned as active writes).
        let user_slot0 = b"unrelated user file\n";
        std::fs::write(dir.path().join("credentials.aeroftp"), user_slot0).unwrap();
        let old_cred1 = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred),
            "\n[old]\naws_access_key_id = AKOLD\naws_secret_access_key = oldsec\n",
        );
        std::fs::write(dir.path().join("credentials.aeroftp.1"), &old_cred1).unwrap();

        let new_cfg = aws_marked_file_bytes(
            &aws_config_marker_for(&cred),
            "\n[profile new]\nregion = eu-west-1\n",
        );
        let new_cred = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred),
            "\n[new]\naws_access_key_id = AKNEW\naws_secret_access_key = newsec\n",
        );
        let external_cfg = b"[profile external]\nregion = ap-south-1\n";

        let raced = std::cell::Cell::new(false);
        let (cfg_out, cred_out) = persist_aws_export_pair(
            &cred,
            &cfg,
            &new_cfg,
            &new_cred,
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |kind, tmp, path| {
                if kind == "config" && path == cfg && !raced.replace(true) {
                    // External writer claims the active config target.
                    std::fs::write(path, external_cfg).unwrap();
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .unwrap();

        assert!(
            raced.get(),
            "the injected active-config race must have fired"
        );
        // The pair was replanned together: both new outputs on slot 2.
        assert_eq!(cfg_out.path, dir.path().join("config.aeroftp.2"));
        assert_eq!(cred_out.path, dir.path().join("credentials.aeroftp.2"));
        assert_eq!(std::fs::read(&cfg_out.path).unwrap(), new_cfg);
        assert_eq!(std::fs::read(&cred_out.path).unwrap(), new_cred);
        // The older pair and the user files are untouched.
        assert_eq!(
            std::fs::read(dir.path().join("credentials.aeroftp.1")).unwrap(),
            old_cred1,
            "older owned side file must not be replaced by a split write"
        );
        assert_eq!(
            std::fs::read(dir.path().join("credentials.aeroftp")).unwrap(),
            user_slot0
        );
        // The rolled-back attempt left none of OUR files behind: the active
        // credentials write was removed, the side config it fell back to was
        // removed, and the external writer's own `config` file stays as it is.
        assert!(!cred.exists());
        assert!(!dir.path().join("config.aeroftp").exists());
        assert_eq!(std::fs::read(&cfg).unwrap(), external_cfg);
    }

    /// The reverse redirect: an external writer claims the active `credentials`
    /// target after planning, so the credentials write moves to a side file.
    /// Both writes of the attempt are rolled back and replanned as one pair;
    /// `config.aeroftp` (user file) and the older owned `config.aeroftp.1`
    /// stay untouched.
    #[test]
    fn credentials_redirect_replans_both_files_as_one_pair() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let cfg = dir.path().join("config");
        let user_slot0 = b"unrelated user file\n";
        std::fs::write(dir.path().join("config.aeroftp"), user_slot0).unwrap();
        let old_cfg1 = aws_marked_file_bytes(
            &aws_config_marker_for(&cred),
            "\n[profile old]\nregion = us-west-1\n",
        );
        std::fs::write(dir.path().join("config.aeroftp.1"), &old_cfg1).unwrap();

        let new_cfg = aws_marked_file_bytes(
            &aws_config_marker_for(&cred),
            "\n[profile new]\nregion = eu-west-1\n",
        );
        let new_cred = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred),
            "\n[new]\naws_access_key_id = AKNEW\naws_secret_access_key = newsec\n",
        );
        let external_cred = b"[user]\naws_access_key_id = AKUSER\n";

        let raced = std::cell::Cell::new(false);
        let (cfg_out, cred_out) = persist_aws_export_pair(
            &cred,
            &cfg,
            &new_cfg,
            &new_cred,
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |kind, tmp, path| {
                if kind == "credentials" && path == cred && !raced.replace(true) {
                    std::fs::write(path, external_cred).unwrap();
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .unwrap();

        assert!(
            raced.get(),
            "the injected active-credentials race must have fired"
        );
        assert_eq!(cfg_out.path, dir.path().join("config.aeroftp.2"));
        assert_eq!(cred_out.path, dir.path().join("credentials.aeroftp.2"));
        assert_eq!(std::fs::read(&cfg_out.path).unwrap(), new_cfg);
        assert_eq!(std::fs::read(&cred_out.path).unwrap(), new_cred);
        assert_eq!(
            std::fs::read(dir.path().join("config.aeroftp.1")).unwrap(),
            old_cfg1,
            "older owned side file must not be replaced by a split write"
        );
        assert_eq!(
            std::fs::read(dir.path().join("config.aeroftp")).unwrap(),
            user_slot0
        );
        // The rolled-back attempt left none of OUR files behind: the active
        // config write was removed, the side credentials it fell back to was
        // removed, and the external writer's own `credentials` file stays.
        assert!(!cfg.exists());
        assert!(!dir.path().join("credentials.aeroftp").exists());
        assert_eq!(std::fs::read(&cred).unwrap(), external_cred);
    }

    /// An Amazon S3 host with a path prefix (a gateway URL) cannot be
    /// represented by an implicit AWS endpoint (the host drops the prefix and
    /// the endpoint option is not kept for amazon-s3): the profile is skipped
    /// with a reason instead of importing a different effective target. A
    /// pathless Amazon URL stays implicit, and a custom host keeps the URL.
    #[test]
    fn amazon_host_with_path_prefix_is_skipped_not_silently_stripped() {
        let cred = "[gw]\naws_access_key_id = AK1\naws_secret_access_key = sec1\n\n\
                    [plain]\naws_access_key_id = AK2\naws_secret_access_key = sec2\n\n\
                    [custom]\naws_access_key_id = AK3\naws_secret_access_key = sec3\n";
        let config = "[profile gw]\nendpoint_url = https://s3.us-east-1.amazonaws.com/gateway\nregion = us-east-1\n\n\
                      [profile plain]\nendpoint_url = https://s3.us-east-1.amazonaws.com\nregion = us-east-1\n\n\
                      [profile custom]\nendpoint_url = https://s3.lab.example.com:9000/gateway\nregion = eu-west-1\n";
        let result = import_pair(cred, Some(config)).unwrap();

        assert!(
            result.servers.iter().all(|s| s.name != "gw"),
            "an Amazon gateway URL must be skipped: {:?}",
            result.skipped
        );
        let skip = result
            .skipped
            .iter()
            .find(|s| s.name == "gw")
            .expect("gw must be skipped");
        assert!(skip.reason.contains("path prefix"), "{}", skip.reason);

        let plain = result
            .servers
            .iter()
            .find(|s| s.name == "plain")
            .expect("a pathless Amazon URL stays implicit");
        assert_eq!(plain.provider_id.as_deref(), Some("amazon-s3"));
        assert_eq!(plain.host, "s3.us-east-1.amazonaws.com");
        assert!(plain
            .options
            .as_ref()
            .and_then(|o| o.get("endpoint"))
            .is_none());

        let custom = result
            .servers
            .iter()
            .find(|s| s.name == "custom")
            .expect("a custom host keeps its full URL");
        assert_eq!(custom.host, "s3.lab.example.com:9000");
        assert_eq!(
            custom.options.as_ref().unwrap()["endpoint"],
            serde_json::json!("https://s3.lab.example.com:9000/gateway")
        );
    }

    /// A run of blank continuation lines is coalesced instead of retained one
    /// entry per line: mid-value blanks are preserved in the value, trailing
    /// blanks trim away, a multi-megabyte blank run after one key parses to
    /// the same result as the short form, and a blank line inside a nested
    /// block still marks the block.
    #[test]
    fn blank_continuation_runs_preserve_value_without_per_line_retention() {
        // Mid-value blank run, then an indented continuation line: the joined
        // value is the same the per-line retention produced.
        let parsed = parse_aws_ini("[s]\nkey = a\n\n\n  b\n", "t").unwrap();
        assert_eq!(
            parsed[0].1.values.get("key").map(String::as_str),
            Some("a\n\n\nb")
        );

        let mut big = String::from("[s]\nkey = a\n");
        big.push_str(&"\n".repeat(2_000_000));
        let parsed = parse_aws_ini(&big, "t").unwrap();
        assert_eq!(
            parsed[0].1.values.get("key").map(String::as_str),
            Some("a"),
            "trailing blank run must trim away, not be retained per line"
        );

        let parsed = parse_aws_ini(
            "[s]\ns3 =\n\n  endpoint_url = https://x.example.com:9000\n",
            "t",
        )
        .unwrap();
        assert_eq!(
            parsed[0].1.nested_get("s3", "endpoint_url"),
            Some("https://x.example.com:9000")
        );
    }

    /// `DEFAULT` is never emitted as a named credentials section (botocore
    /// reads `[DEFAULT]` as defaults, so a second secret-less profile could
    /// inherit the first profile's keys): the profile goes out under a safe
    /// name in BOTH files. Lowercase `default` stays a valid named profile.
    #[test]
    fn export_never_writes_a_named_default_section() {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        let server = |id: &str,
                      name: &str,
                      host: &str,
                      port: u32,
                      provider: &str,
                      options: serde_json::Value| AwsExportServer {
            id: id.to_string(),
            name: name.to_string(),
            host: host.to_string(),
            port,
            username: format!("AK-{id}"),
            protocol: Some("s3".to_string()),
            provider_id: Some(provider.to_string()),
            options: Some(options),
            initial_path: None,
        };
        let servers = vec![
            server(
                "d1",
                "DEFAULT",
                "s3.lab.example.com",
                9000,
                "custom-s3",
                serde_json::json!({"endpoint": "https://s3.lab.example.com:9000", "region": "eu-west-1"}),
            ),
            server(
                "d2",
                "nosecret",
                "s3.lab.example.com",
                9000,
                "custom-s3",
                serde_json::json!({"endpoint": "https://s3.lab.example.com:9000", "region": "eu-west-1"}),
            ),
            server(
                "d3",
                "default",
                "",
                443,
                "amazon-s3",
                serde_json::json!({"region": "us-east-1"}),
            ),
        ];
        let passwords = HashMap::from([("d1".to_string(), "dsecret".to_string())]);
        let outcome = export_aws_credentials(&servers, &passwords, &cred_path).unwrap();
        assert_eq!(outcome.exported, 3);

        let cred = std::fs::read_to_string(&cred_path).unwrap();
        let cfg = std::fs::read_to_string(dir.path().join("config")).unwrap();
        assert!(
            !cred.lines().any(|l| l == "[DEFAULT]"),
            "credentials must not contain a [DEFAULT] section:\n{cred}"
        );
        assert!(cred.contains("[DEFAULT-2]\n"), "{cred}");
        assert!(
            cred.contains("[default]\n"),
            "lowercase default stays a named profile: {cred}"
        );
        assert!(cfg.contains("[profile DEFAULT-2]"), "{cfg}");
        let nosecret = cred.split("[nosecret]").nth(1).expect("nosecret section");
        assert!(
            !nosecret.contains("aws_secret_access_key"),
            "the secret-less profile must not inherit DEFAULT's secret: {cred}"
        );
    }

    /// An AWS-implicit S3 profile (empty host, no endpoint) with an explicit
    /// `pathStyle: true` must still export `addressing_style = path`: the
    /// path-style flag must not be lost when the endpoint resolves to None.
    /// A `pathStyle: false` control writes no config section.
    #[test]
    fn export_keeps_path_style_for_endpoint_less_aws_profile() {
        let dir = tempfile::tempdir().unwrap();
        let cred_path = dir.path().join("credentials");
        let server = |id: &str, name: &str, path_style: bool| AwsExportServer {
            id: id.to_string(),
            name: name.to_string(),
            host: String::new(),
            port: 443,
            username: format!("AK-{id}"),
            protocol: Some("s3".to_string()),
            provider_id: Some("amazon-s3".to_string()),
            options: Some(serde_json::json!({ "pathStyle": path_style })),
            initial_path: None,
        };
        let servers = vec![server("p1", "pathy", true), server("p2", "virtual", false)];
        let passwords = HashMap::from([
            ("p1".to_string(), "s1".to_string()),
            ("p2".to_string(), "s2".to_string()),
        ]);
        export_aws_credentials(&servers, &passwords, &cred_path).unwrap();

        let cfg = std::fs::read_to_string(dir.path().join("config")).unwrap();
        assert!(
            cfg.contains("[profile pathy]\n"),
            "the path-style profile needs a config section: {cfg}"
        );
        assert!(
            cfg.contains("addressing_style = path"),
            "explicit pathStyle on an endpoint-less AWS profile must be exported: {cfg}"
        );
        assert!(!cfg.contains("endpoint_url"), "{cfg}");
        assert!(
            !cfg.contains("[profile virtual]"),
            "pathStyle=false with no region/endpoint writes no config section: {cfg}"
        );
    }

    /// C2: repeated `AlreadyExists` collisions on the `Absent` target path are
    /// charged to `retries` / `MAX_RETRIES` so the write loop terminates cleanly.
    #[test]
    fn absent_target_repeated_already_exists_exhausts_retry_budget() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config");
        let tmp = crate::bridge_shared::stage_export_temp_600(dir.path(), b"cfg").unwrap();
        let err = write_aws_protected_staged_with(
            &target,
            None,
            &test_config_marker(),
            "AWS config",
            tmp,
            AwsProtectedWriteMode::Absent,
            false,
            AWS_MAX_SIDE_SLOTS,
            false,
            |_, _| unreachable!("replace must not run"),
            |t, _| {
                Err(tempfile::PersistError {
                    error: std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "simulated transient entry",
                    ),
                    file: t,
                })
            },
        )
        .err()
        .expect("repeated AlreadyExists on Absent target must exhaust retry budget");
        assert!(err.contains("exhausted retry budget"), "{err}");
    }

    /// E1: `s3.addressing_style` matches `PATH` and `virtual` case-insensitively
    /// on the borrowed trimmed slice, and handles a large inherited `[DEFAULT]`
    /// invalid value across many profiles without per-profile lowercase copies.
    #[test]
    fn s3_addressing_style_matches_case_insensitively_without_allocating_for_large_default() {
        let big_invalid = "X".repeat(256 * 1024);
        let mut cred = String::from(
            "[upper_path]\naws_access_key_id = AK1\naws_secret_access_key = sec1\n\n\
             [lower_virtual]\naws_access_key_id = AK2\naws_secret_access_key = sec2\n",
        );
        for i in 0..50 {
            cred.push_str(&format!(
                "[p{i}]\naws_access_key_id = AKP{i}\naws_secret_access_key = secp{i}\n"
            ));
        }
        let cfg = format!(
            "[DEFAULT]\ns3 =\n  addressing_style = {big_invalid}\n\n\
             [profile upper_path]\ns3 =\n  addressing_style =  PATH \n\n\
             [profile lower_virtual]\ns3 =\n  addressing_style = virtual\n"
        );
        let res = import_pair(&cred, Some(&cfg)).unwrap();
        assert_eq!(res.servers.len(), 52);
        let up = res.servers.iter().find(|s| s.name == "upper_path").unwrap();
        assert_eq!(
            up.options.as_ref().unwrap()["pathStyle"],
            serde_json::json!(true)
        );
        let lv = res
            .servers
            .iter()
            .find(|s| s.name == "lower_virtual")
            .unwrap();
        assert_eq!(
            lv.options.as_ref().unwrap()["pathStyle"],
            serde_json::json!(false)
        );
        let p0 = res.servers.iter().find(|s| s.name == "p0").unwrap();
        assert!(p0
            .options
            .as_ref()
            .and_then(|o| o.get("pathStyle"))
            .is_none());
    }

    /// E2: `max_materialized_bytes` charges the derived `host` (and `provider_id`)
    /// retained on each `ServerProfileExport`, not only the raw `endpoint_url`.
    #[test]
    fn output_budget_charges_derived_host_and_provider_id() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let cfg = dir.path().join("config");
        let long_host = format!("{}.example.com", "a".repeat(200));
        let ep = format!("https://{long_host}:9000");
        std::fs::write(
            &cred,
            "[lab]\naws_access_key_id = AK1\naws_secret_access_key = sec1\n",
        )
        .unwrap();
        std::fs::write(&cfg, format!("[profile lab]\nendpoint_url = {ep}\n")).unwrap();

        // Without charging derived `host` (~217 bytes) + `provider_id` (~9 bytes),
        // raw fields + generated ID total ~255 bytes; charging derived `host`
        // pushes total to ~480 bytes and exceeds a 350-byte budget.
        let err = import_aws_credentials_with_config_limits(
            &cred,
            Some(&cfg),
            crate::bridge_shared::MAX_IMPORT_FILE_BYTES,
            350,
        )
        .err()
        .expect("derived host must be charged to max_materialized_bytes");
        assert!(err.contains("maximum allowed output size"), "{err}");
    }

    /// An empty `services =` value is a present-but-unresolved reference: the
    /// profile is skipped like any undefined services section and must NOT
    /// fall back to the profile-level `endpoint_url`. With
    /// `ignore_configured_endpoint_urls` the reference is bypassed instead.
    #[test]
    fn an_empty_services_value_skips_instead_of_falling_back_to_endpoint_url() {
        let result = import_pair(
            "[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = labsecret\n",
            Some(
                "[profile lab]\nregion = eu-west-1\nservices =\nendpoint_url = https://fallback.example.com:9000\n",
            ),
        )
        .unwrap();
        assert!(
            result.servers.iter().all(|s| s.name != "lab"),
            "empty services = must skip the profile: {:?}",
            result.skipped
        );
        let skip = result
            .skipped
            .iter()
            .find(|s| s.name == "lab")
            .expect("lab must be skipped");
        assert!(
            skip.reason.contains("services"),
            "unexpected skip reason: {}",
            skip.reason
        );

        let result = import_pair(
            "[lab]\naws_access_key_id = AKLAB\naws_secret_access_key = labsecret\n",
            Some(
                "[profile lab]\nregion = eu-west-1\nservices =\nignore_configured_endpoint_urls = true\n",
            ),
        )
        .unwrap();
        assert_eq!(result.servers.len(), 1, "skipped: {:?}", result.skipped);
        assert_eq!(result.servers[0].host, "");
        assert_eq!(result.servers[0].provider_id.as_deref(), Some("amazon-s3"));
    }

    /// The generated profile ID is charged by its actual lowercase UTF-8
    /// length: U+0130 expands 2 -> 3 bytes under to_lowercase, so a name that
    /// fits the old raw-length estimate must still fail the real charge.
    #[test]
    fn output_budget_charges_the_actual_lowercase_id_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let k = 100;
        let name = "\u{0130}".repeat(k);
        std::fs::write(
            &cred,
            format!("[{name}]\naws_access_key_id = AK1\naws_secret_access_key = sec1\n"),
        )
        .unwrap();
        // Old estimate charges name.len() = 200 for the slug; the real slug
        // is 300 bytes. Fields: name 200 + akid 3 + secret 4 + provider 9,
        // id prefix/suffix 13 -> old total 429, real total 529. A 450-byte
        // budget fits the OLD estimate (would pass) but not the real charge.
        let res = import_aws_credentials_with_config_limits(
            &cred,
            None,
            crate::bridge_shared::MAX_IMPORT_FILE_BYTES,
            450,
        );
        let err = match res {
            Err(e) => e,
            Ok(_) => {
                panic!("the real (expanded) id length must be charged, not the raw name length")
            }
        };
        assert!(err.contains("maximum allowed output size"), "{err}");
        let ok = import_aws_credentials_with_config_limits(
            &cred,
            None,
            crate::bridge_shared::MAX_IMPORT_FILE_BYTES,
            600,
        );
        assert!(
            ok.is_ok(),
            "a budget covering the real charge accepts the profile: {:?}",
            ok.err()
        );
    }

    /// The matched-pair selector enumerates existing directory entries once:
    /// an empty namespace allocates slot 0 without per-slot stats, and an
    /// existing owned matched pair wins over any lower free slot.
    #[test]
    fn a_matched_existing_pair_wins_over_a_lower_free_slot() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let cfg = dir.path().join("config");
        let (m1, m2) =
            aws_side_pair_modes_with_limit(&cfg, &cred, "cfgm", "credm", AWS_MAX_SIDE_SLOTS)
                .unwrap();
        assert!(
            matches!(m1, AwsProtectedWriteMode::SideAt(0))
                && matches!(m2, AwsProtectedWriteMode::SideAt(0)),
            "empty namespace must allocate slot 0: {m1:?} {m2:?}"
        );

        let cfg_m = aws_config_marker_for(&cred);
        let cred_m = aws_credentials_marker_for(&cred);
        std::fs::write(
            dir.path().join("config.aeroftp.3"),
            aws_marked_file_bytes(&cfg_m, "x"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("credentials.aeroftp.3"),
            aws_marked_file_bytes(&cred_m, "y"),
        )
        .unwrap();
        let (m1, m2) =
            aws_side_pair_modes_with_limit(&cfg, &cred, &cfg_m, &cred_m, AWS_MAX_SIDE_SLOTS)
                .unwrap();
        assert!(
            matches!(m1, AwsProtectedWriteMode::ReplaceSide(3))
                && matches!(m2, AwsProtectedWriteMode::ReplaceSide(3)),
            "an existing owned matched pair must win over free slot 0: {m1:?} {m2:?}"
        );
    }

    /// When the active-planned CONFIG write redirects to a side file at write
    /// time, the credentials persist hook must never run for the active
    /// credentials path: both files are rolled back and replanned as one
    /// matched pair before any credentials write.
    #[test]
    fn an_active_config_redirect_replans_before_any_active_credentials_write() {
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("credentials");
        let cfg = dir.path().join("config");
        let cfg_bytes = aws_marked_file_bytes(
            &aws_config_marker_for(&cred),
            "[profile x]\nregion = eu-west-1\n",
        );
        let cred_bytes = aws_marked_file_bytes(
            &aws_credentials_marker_for(&cred),
            "[x]\naws_access_key_id = AK1\naws_secret_access_key = s\n",
        );
        let raced = std::cell::Cell::new(false);
        let cred_active_calls = std::cell::Cell::new(0usize);
        let (cfg_out, cred_out) = persist_aws_export_pair(
            &cred,
            &cfg,
            &cfg_bytes,
            &cred_bytes,
            |_, tmp, path| crate::bridge_shared::persist_staged_temp(tmp, path),
            |kind, tmp, path| {
                if kind == "config" && path == cfg && !raced.replace(true) {
                    std::fs::write(path, b"external").unwrap();
                }
                if kind == "credentials" && path == cred {
                    cred_active_calls.set(cred_active_calls.get() + 1);
                }
                crate::bridge_shared::persist_staged_temp_noclobber(tmp, path)
            },
        )
        .unwrap();
        assert!(
            raced.get(),
            "the injected active-config race must have fired"
        );
        assert_eq!(
            cred_active_calls.get(),
            0,
            "the credentials hook must never run for the active path after a config redirect"
        );
        assert!(
            cfg_out.protected && cred_out.protected,
            "both files must land in one matched side pair"
        );
        assert_eq!(cfg_out.path, dir.path().join("config.aeroftp"));
        assert_eq!(cred_out.path, dir.path().join("credentials.aeroftp"));
        assert_eq!(std::fs::read(&cfg).unwrap(), b"external");
        assert!(
            !cred.exists(),
            "the active credentials path must stay untouched"
        );
    }
}
