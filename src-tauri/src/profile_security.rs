// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Saved-profile options that turn off a check the connection would otherwise
//! make (a certificate check, a host-key check, the PASV address, cleartext
//! consent), read from `src/shared/profile-security-relaxations.json`, the same
//! table the frontend reads (`src/utils/profileSecurity.ts`).
//!
//! Every import path brings these in their safe state unless the user keeps
//! them on purpose, and CLI strict mode refuses a profile that carries one
//! (#1506 Requested 188 and 198): an imported file is untrusted input, and an
//! option it sets can point a saved credential at a connection that no longer
//! checks who it is talking to.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelaxationRule {
    key: String,
    /// Other spellings the frontend reader matches; the backend matches every
    /// spelling the connection loader reads, through
    /// `canonicalize_profile_option_key`, and a test checks the two agree.
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(rename = "unsafe")]
    unsafe_values: Vec<Value>,
    /// The value the safe state stores explicitly, so neither a preset default
    /// nor an auto rule (a WebDAV host on port 80 picks HTTP without a scheme)
    /// can bring the relaxed value back when the connection is built.
    safe: Value,
    loopback_exempt: bool,
}

impl RelaxationRule {
    /// Whether `option_key` is a spelling of this rule's option, as the
    /// connection loader reads it (`verifyCert` and `verify_cert` alike).
    fn names(&self, option_key: &str) -> bool {
        option_key == self.key
            || self.aliases.iter().any(|alias| alias == option_key)
            || crate::profile_loader::canonicalize_profile_option_key(option_key)
                == crate::profile_loader::canonicalize_profile_option_key(&self.key)
    }
}

fn rule(key: &str) -> Option<&'static RelaxationRule> {
    table().relaxations.iter().find(|rule| rule.key == key)
}

#[derive(Debug, Deserialize)]
struct RelaxationTable {
    relaxations: Vec<RelaxationRule>,
}

fn table() -> &'static RelaxationTable {
    static TABLE: LazyLock<RelaxationTable> = LazyLock::new(|| {
        serde_json::from_str(include_str!(
            "../../src/shared/profile-security-relaxations.json"
        ))
        .expect("profile-security-relaxations.json is valid")
    });
    &TABLE
}

/// One option of a profile that relaxes a security check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relaxation {
    /// The saved option key (`verifyCert`, `trustPasvAddress`, ...).
    pub key: String,
    /// The value the profile carries, as text.
    pub value: String,
}

fn value_matches(actual: &Value, unsafe_value: &Value) -> bool {
    match (actual, unsafe_value) {
        (Value::String(a), Value::String(u)) => a.trim().eq_ignore_ascii_case(u),
        (a, u) => a == u,
    }
}

fn shown(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The relaxing options of a profile given by its parts: the host it connects
/// to, its preset id, its protocol and its saved options. The loopback
/// exemption holds only when every address the profile names stays on this
/// machine, its `host` and its `options.endpoint` alike: a loopback endpoint
/// beside a remote host exempts nothing, since WebDAV, FTP and SFTP connect to
/// the host (Greptile on #1506 Requested 198). Every spelling the connection
/// loader reads counts, and a WebDAV host written as `http://...` is plain
/// HTTP whatever the scheme option says, since the host prefix wins in
/// `WebDavConfig`.
pub fn security_relaxations_of(
    host: &str,
    provider_id: Option<&str>,
    protocol: Option<&str>,
    options: Option<&Value>,
) -> Vec<Relaxation> {
    let empty = serde_json::Map::new();
    let options = options.and_then(Value::as_object).unwrap_or(&empty);
    let preset = profile_preset(protocol, provider_id);
    let addresses = profile_addresses(host, options);
    // Swift's catalog can send the storage traffic to another host than the
    // auth URL, so a loopback auth host proves nothing about where the token
    // goes (CodeRabbit on #1524).
    let swift = protocol
        .and_then(crate::connection_manifest::connection_spec)
        .is_some_and(|spec| spec.token == "swift");
    let on_loopback = !swift
        && !addresses.is_empty()
        && addresses
            .iter()
            .all(|address| crate::util::endpoint_stays_on_this_machine(address));
    let preset_owns_the_address = preset.is_some_and(|preset| preset_owns(preset, &addresses));
    let exempt = |rule: &RelaxationRule| {
        (preset_owns_the_address
            && preset.is_some_and(|preset| preset.defaults.contains_key(&rule.key)))
            || (rule.loopback_exempt && on_loopback)
    };
    let mut found: Vec<Relaxation> = table()
        .relaxations
        .iter()
        .filter(|rule| !exempt(rule))
        .filter_map(|rule| {
            let actual = options
                .iter()
                .filter(|(key, _)| rule.names(key))
                .map(|(_, value)| value)
                .find(|value| rule.unsafe_values.iter().any(|u| value_matches(value, u)))?;
            Some(Relaxation {
                key: rule.key.clone(),
                value: shown(actual),
            })
        })
        .collect();
    let webdav = protocol
        .and_then(crate::connection_manifest::connection_spec)
        .is_some_and(|spec| spec.token == "webdav");
    if webdav
        && host_is_plain_http(host)
        && !found.iter().any(|r| r.key == WEBDAV_SCHEME_KEY)
        && rule(WEBDAV_SCHEME_KEY).is_some_and(|rule| !exempt(rule))
    {
        found.push(Relaxation {
            key: WEBDAV_SCHEME_KEY.to_string(),
            value: "http (host)".to_string(),
        });
    }
    found
}

/// The addresses a profile names: its host and its `options.endpoint`.
fn profile_addresses<'a>(
    host: &'a str,
    options: &'a serde_json::Map<String, Value>,
) -> Vec<&'a str> {
    [Some(host), options.get("endpoint").and_then(Value::as_str)]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .collect()
}

/// A preset owns its defaults only at the address it fixes: an imported file
/// can name a local-bridge preset and point it at a remote server (CodeRabbit
/// on #1506 Requested 198), so every address of the profile has to be the
/// preset's own host (its `server` or `endpoint` default) or loopback. A
/// profile that names no address connects where the preset says.
fn preset_owns(preset: &crate::connection_manifest::ConnectionPreset, addresses: &[&str]) -> bool {
    let own: Vec<String> = ["server", "endpoint"]
        .iter()
        .filter_map(|key| preset.defaults.get(*key).and_then(Value::as_str))
        .filter_map(crate::util::endpoint::endpoint_host)
        .collect();
    addresses.iter().all(|address| {
        crate::util::endpoint_stays_on_this_machine(address)
            || crate::util::endpoint::endpoint_host(address).is_some_and(|host| own.contains(&host))
    })
}

/// Whether a saved profile's preset defaults may be applied when its
/// connection is built: the preset owns the profile's address, or the profile
/// names no preset.
pub fn preset_owns_profile_address(profile: &Value) -> bool {
    let Some(preset) = profile_preset(
        profile.get("protocol").and_then(Value::as_str),
        profile.get("providerId").and_then(Value::as_str),
    ) else {
        return true;
    };
    let empty = serde_json::Map::new();
    let options = profile
        .get("options")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let host = profile.get("host").and_then(Value::as_str).unwrap_or("");
    preset_owns(preset, &profile_addresses(host, options))
}

/// The preset a profile connects through, derived as the connection loader
/// derives it: its `providerId`, or the one a protocol alias names (a bare
/// `blomp` protocol is the Blomp preset, CodeRabbit on #1524).
fn profile_preset(
    protocol: Option<&str>,
    provider_id: Option<&str>,
) -> Option<&'static crate::connection_manifest::ConnectionPreset> {
    let (token, provider_id) = match protocol {
        Some(protocol) => {
            let (token, provider_id) =
                crate::connection_manifest::canonical_protocol_and_provider(protocol, provider_id);
            (Some(token), provider_id)
        }
        None => (None, provider_id),
    };
    // A preset applies only to the protocol it belongs to, as the loader
    // checks (`profile_connection_extra`): an S3 preset named on a WebDAV
    // profile lends it nothing (CodeRabbit on #1524).
    provider_id
        .and_then(crate::connection_manifest::preset)
        .filter(|preset| token.is_none_or(|token| preset.protocol == token))
}

/// Whether `value` for option `key` is one the table calls relaxed, the test a
/// preset default has to pass before it is applied off the preset's address.
pub fn is_relaxed_value(key: &str, value: &Value) -> bool {
    table()
        .relaxations
        .iter()
        .any(|rule| rule.names(key) && rule.unsafe_values.iter().any(|u| value_matches(value, u)))
}

/// The rule a WebDAV host's `http://` prefix belongs to.
const WEBDAV_SCHEME_KEY: &str = "webdavScheme";

fn host_is_plain_http(host: &str) -> bool {
    host.trim()
        .get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http://"))
}

/// One relaxing option the user chose to keep on an import: the profile's
/// place in the file and the option key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeptRelaxation {
    pub file_index: usize,
    pub key: String,
}

/// What an import does with the relaxing options it finds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RelaxationPolicy {
    /// Every one comes in its safe state (the default, and the GUI's when the
    /// user ticks nothing).
    #[default]
    ResetAll,
    /// Every one is kept (CLI `--keep-insecure-options`).
    KeepAll,
    /// Only the listed ones are kept (the GUI preview checkboxes).
    Keep(Vec<KeptRelaxation>),
}

impl RelaxationPolicy {
    /// The keys kept for the profile at `file_index`, out of `relaxations`.
    pub fn kept_keys(&self, file_index: usize, relaxations: &[Relaxation]) -> Vec<String> {
        match self {
            Self::ResetAll => Vec::new(),
            Self::KeepAll => relaxations.iter().map(|r| r.key.clone()).collect(),
            Self::Keep(kept) => kept
                .iter()
                .filter(|k| k.file_index == file_index)
                .map(|k| k.key.clone())
                .collect(),
        }
    }
}

/// [`security_relaxations_of`] for a profile stored as JSON (`host`,
/// `providerId`, `protocol`, `options`).
pub fn security_relaxations(profile: &Value) -> Vec<Relaxation> {
    security_relaxations_of(
        profile.get("host").and_then(Value::as_str).unwrap_or(""),
        profile.get("providerId").and_then(Value::as_str),
        profile.get("protocol").and_then(Value::as_str),
        profile.get("options"),
    )
}

/// Bring a profile's host and options to their safe state: for every relaxing
/// option found by [`security_relaxations_of`] whose key is not in `keep`,
/// every spelling of it is removed and the rule's safe value is stored
/// explicitly, so a preset default cannot bring the relaxed value back when
/// the connection is built (Greptile on #1506 Requested 198); a WebDAV host's
/// `http://` prefix becomes `https://`. Returns the keys reset.
pub fn make_parts_safe(
    host: &mut String,
    options: &mut Option<Value>,
    relaxations: &[Relaxation],
    keep: &[String],
) -> Vec<String> {
    let reset: Vec<&Relaxation> = relaxations
        .iter()
        .filter(|relaxation| !keep.contains(&relaxation.key))
        .collect();
    if reset.is_empty() {
        return Vec::new();
    }
    if !options.as_ref().is_some_and(Value::is_object) {
        *options = Some(Value::Object(serde_json::Map::new()));
    }
    let Some(map) = options.as_mut().and_then(Value::as_object_mut) else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    for relaxation in reset {
        let Some(rule) = rule(&relaxation.key) else {
            continue;
        };
        let spellings: Vec<String> = map.keys().filter(|key| rule.names(key)).cloned().collect();
        for spelling in &spellings {
            map.remove(spelling);
        }
        map.insert(rule.key.clone(), rule.safe.clone());
        if rule.key == WEBDAV_SCHEME_KEY && host_is_plain_http(host) {
            *host = format!("https://{}", &host.trim()[7..]);
        }
        if !keys.contains(&rule.key) {
            keys.push(rule.key.clone());
        }
    }
    keys
}

/// The options of one profile that an import or a restore put back to their
/// safe state, reported to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileReset {
    pub name: String,
    pub keys: Vec<String>,
}

/// Bring every profile of `list` to its safe state ([`make_profile_safe`] with
/// nothing kept), and return what was reset, per profile.
pub fn make_profile_list_safe(list: &mut [Value]) -> Vec<ProfileReset> {
    list.iter_mut()
        .filter_map(|profile| {
            let keys = make_profile_safe(profile, &[]);
            (!keys.is_empty()).then(|| ProfileReset {
                name: profile
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                keys,
            })
        })
        .collect()
}

/// Set once at startup when a run is in strict mode (`--strict` /
/// `AEROFTP_STRICT`), for every path that loads a saved profile long after the
/// flags were checked: the CLI's profile commands, the agent's connections and
/// the MCP pool (Greptile on #1524: `agent --mcp --strict` connected relaxed
/// profiles).
static STRICT_PROFILES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turn strict mode on for saved profiles.
pub fn set_strict_profiles(on: bool) {
    STRICT_PROFILES.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// In strict mode, the refusal for a saved profile whose options turn a
/// security check off (#1506 Requested 198): the flags that do the same are
/// refused before the run, a profile option must not be the way around them.
pub fn strict_profile_refusal(profile: &Value) -> Option<String> {
    strict_profile_refusal_when(
        STRICT_PROFILES.load(std::sync::atomic::Ordering::Relaxed),
        profile,
    )
}

/// [`strict_profile_refusal`] with the strict setting given, for tests.
pub fn strict_profile_refusal_when(strict: bool, profile: &Value) -> Option<String> {
    if !strict {
        return None;
    }
    strict_profile_violation(profile)
}

/// The strict-mode message for `profile`, or `None` when it relaxes nothing.
pub fn strict_profile_violation(profile: &Value) -> Option<String> {
    let relaxations = security_relaxations(profile);
    if relaxations.is_empty() {
        return None;
    }
    let name = profile.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let options = relaxations
        .iter()
        .map(|r| format!("{}={}", r.key, r.value))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "strict mode (--strict / AEROFTP_STRICT) refuses the saved profile \"{name}\": its options turn a security check off ({options}). Turn them off in the profile, or drop --strict to run without the safety gate."
    ))
}

/// [`make_parts_safe`] for a profile stored as JSON. Returns the keys reset.
pub fn make_profile_safe(profile: &mut Value, keep: &[String]) -> Vec<String> {
    let relaxations = security_relaxations(profile);
    let Some(object) = profile.as_object_mut() else {
        return Vec::new();
    };
    let mut host = object
        .get("host")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut options = object.remove("options");
    let reset = make_parts_safe(&mut host, &mut options, &relaxations, keep);
    if let Some(options) = options {
        object.insert("options".to_string(), options);
    }
    if object.contains_key("host") || !host.is_empty() {
        object.insert("host".to_string(), Value::String(host));
    }
    reset
}

/// The relaxations of `incoming` that `existing` does not already carry with
/// the same value: an overwrite that keeps a setting the user already has
/// imports nothing new (Greptile on #1506 Requested 198).
pub fn new_relaxations(incoming: Vec<Relaxation>, existing: &Value) -> Vec<Relaxation> {
    let already = security_relaxations(existing);
    incoming
        .into_iter()
        .filter(|relaxation| !already.contains(relaxation))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Deserialize)]
    struct Case {
        name: String,
        profile: Value,
        relaxed: Vec<String>,
    }

    #[derive(Deserialize)]
    struct Cases {
        cases: Vec<Case>,
    }

    /// The shared cases, the same ones `src/utils/profileSecurity.test.ts`
    /// runs, so the two readers of the table cannot disagree.
    #[test]
    fn the_shared_cases_hold() {
        let cases: Cases = serde_json::from_str(include_str!(
            "../../src/shared/profile-security-relaxations.json"
        ))
        .unwrap();
        assert!(cases.cases.len() >= 10);
        for case in cases.cases {
            let keys: Vec<String> = security_relaxations(&case.profile)
                .into_iter()
                .map(|r| r.key)
                .collect();
            assert_eq!(keys, case.relaxed, "{}", case.name);
        }
    }

    #[test]
    fn every_rule_names_a_key_the_profile_loader_reads() {
        for rule in &table().relaxations {
            let backend = crate::profile_loader::normalize_profile_option_key(&rule.key);
            assert!(!backend.is_empty(), "{}", rule.key);
            assert!(!rule.unsafe_values.is_empty(), "{}", rule.key);
        }
    }

    #[test]
    fn making_a_profile_safe_removes_only_what_is_not_kept() {
        let mut profile = json!({
            "host": "files.example.com",
            "options": {
                "verifyCert": false,
                "trustPasvAddress": true,
                "trustUnknownHosts": true,
                "timeout": 30
            }
        });
        let removed = make_profile_safe(&mut profile, &["trustPasvAddress".to_string()]);
        assert_eq!(removed, vec!["verifyCert", "trustUnknownHosts"]);
        assert_eq!(
            profile["options"],
            json!({"trustPasvAddress": true, "timeout": 30, "verifyCert": true, "trustUnknownHosts": false})
        );
        assert_eq!(
            security_relaxations(&profile),
            vec![Relaxation {
                key: "trustPasvAddress".into(),
                value: "true".into()
            }]
        );
    }

    /// Every alias the frontend reader matches is a spelling the loader reads
    /// as the same option, and the snake_case form of each key is listed, so
    /// the two readers cannot drift apart.
    #[test]
    fn the_frontend_aliases_are_the_spellings_the_loader_reads() {
        use crate::profile_loader::canonicalize_profile_option_key as canonical;
        for rule in &table().relaxations {
            let key = canonical(&rule.key);
            assert!(
                rule.aliases.contains(&key) || key == rule.key,
                "{}",
                rule.key
            );
            // Compared through the loader's canonical form directly: `names`
            // accepts a listed alias by itself, which would make this check
            // pass for any string (CodeRabbit on #1524).
            for alias in &rule.aliases {
                assert_eq!(canonical(alias), key, "{} -> {alias}", rule.key);
            }
        }
    }

    /// Greptile on #1506 Requested 198: the snake_case spelling, an unused
    /// loopback endpoint and a WebDAV reset on port 80 were all ways around
    /// the reset.
    #[test]
    fn spellings_unused_endpoints_and_port_80_do_not_bypass_the_reset() {
        let mut snake = json!({"host": "dav.example.com", "options": {"verify_cert": false}});
        assert_eq!(security_relaxations(&snake)[0].key, "verifyCert");
        assert_eq!(make_profile_safe(&mut snake, &[]), vec!["verifyCert"]);
        assert_eq!(snake["options"], json!({"verifyCert": true}));

        let decoy = json!({
            "host": "dav.example.com",
            "options": {"verifyCert": false, "endpoint": "http://127.0.0.1"}
        });
        assert_eq!(security_relaxations(&decoy).len(), 1);

        let mut port_80 = json!({
            "host": "dav.example.com", "port": 80, "protocol": "webdav",
            "options": {"webdavScheme": "http", "tls_mode": "HTTP"}
        });
        assert_eq!(make_profile_safe(&mut port_80, &[]), vec!["webdavScheme"]);
        assert_eq!(port_80["options"], json!({"webdavScheme": "https"}));
    }

    /// The class rule for #1506 Requested 198 (Greptile round 1): a reset is
    /// proven by the connection config the providers build, not by the saved
    /// options. One relaxed profile per rule goes through the reset, then
    /// `profile_connection_extra`, then the provider's own config, including
    /// a preset default that would come back and a WebDAV host prefix.
    #[test]
    fn every_reset_holds_in_the_effective_connection_config() {
        use crate::providers::swift::SwiftConfig;
        use crate::providers::{
            FtpConfig, ProviderConfig, ProviderType, S3Config, SftpConfig, WebDavConfig,
        };
        fn effective(profile: &Value, provider_type: ProviderType) -> ProviderConfig {
            let host = profile.get("host").and_then(Value::as_str).unwrap_or("");
            let (extra, host) =
                crate::profile_loader::profile_connection_extra(profile, provider_type, host);
            ProviderConfig {
                name: "probe".into(),
                provider_type,
                host,
                port: profile
                    .get("port")
                    .and_then(Value::as_u64)
                    .and_then(|port| u16::try_from(port).ok()),
                username: Some("user".into()),
                password: Some("secret".into()),
                initial_path: None,
                extra,
            }
        }
        let safe = |mut profile: Value| {
            assert!(!security_relaxations(&profile).is_empty(), "{profile}");
            make_profile_safe(&mut profile, &[]);
            assert!(security_relaxations(&profile).is_empty(), "{profile}");
            profile
        };

        let dav = safe(
            json!({"host": "dav.example.com", "port": 443, "protocol": "webdav",
            "options": {"verifyCert": false}}),
        );
        assert!(
            WebDavConfig::from_provider_config(&effective(&dav, ProviderType::WebDav))
                .unwrap()
                .verify_cert
        );
        // The Filen Desktop S3 preset declares verifyCert=false: the profile's
        // explicit true has to beat it once the address is not the bridge.
        let filen = safe(
            json!({"host": "", "protocol": "s3", "providerId": "filen-desktop-s3",
            "options": {"endpoint": "https://s3.attacker.example", "bucket": "b", "verifyCert": false}}),
        );
        assert!(
            S3Config::from_provider_config(&effective(&filen, ProviderType::S3))
                .unwrap()
                .verify_cert
        );
        let s3 = safe(json!({"host": "", "protocol": "s3",
            "options": {"endpoint": "http://minio.example.com:9000", "bucket": "b", "allowCleartextEndpoint": true}}));
        assert!(
            !S3Config::from_provider_config(&effective(&s3, ProviderType::S3))
                .unwrap()
                .allow_cleartext_endpoint
        );
        let blomp = safe(
            json!({"host": "https://keystone.attacker.example", "protocol": "swift",
            "providerId": "blomp", "options": {"allowCleartextStorage": true}}),
        );
        assert!(
            !SwiftConfig::from_provider_config(&effective(&blomp, ProviderType::Swift))
                .unwrap()
                .allow_cleartext_storage_endpoint
        );
        let port_80 = safe(
            json!({"host": "dav.example.com", "port": 80, "protocol": "webdav",
            "options": {"webdavScheme": "http"}}),
        );
        assert!(
            WebDavConfig::from_provider_config(&effective(&port_80, ProviderType::WebDav))
                .unwrap()
                .url
                .starts_with("https://")
        );
        let prefixed = safe(
            json!({"host": "http://dav.example.com:8080", "protocol": "webdav",
            "options": {"webdavScheme": "https"}}),
        );
        assert!(
            WebDavConfig::from_provider_config(&effective(&prefixed, ProviderType::WebDav))
                .unwrap()
                .url
                .starts_with("https://")
        );
        let ftp = safe(json!({"host": "ftp.example.com", "protocol": "ftp",
            "options": {"trustPasvAddress": true}}));
        assert!(
            !FtpConfig::from_provider_config(&effective(&ftp, ProviderType::Ftp))
                .unwrap()
                .trust_pasv_address
        );
        let sftp = safe(json!({"host": "sftp.example.com", "protocol": "sftp",
            "options": {"trustUnknownHosts": true}}));
        assert!(
            !SftpConfig::from_provider_config(&effective(&sftp, ProviderType::Sftp))
                .unwrap()
                .trust_unknown_hosts
        );

        // Every rule of the table has a case above.
        let covered = [
            "verifyCert",
            "allowCleartextEndpoint",
            "allowCleartextStorage",
            "webdavScheme",
            "trustPasvAddress",
            "trustUnknownHosts",
        ];
        for rule in &table().relaxations {
            assert!(
                covered.contains(&rule.key.as_str()),
                "{} has no effective-config case",
                rule.key
            );
        }
    }

    /// CodeRabbit on #1524: a profile that names a local-bridge or cleartext
    /// preset but points elsewhere, and omits the option, inherited the
    /// preset's relaxed default when its connection was built. At the
    /// preset's own address the default still applies.
    #[test]
    fn an_off_address_preset_lends_no_relaxed_default() {
        use crate::providers::swift::SwiftConfig;
        use crate::providers::{ProviderConfig, ProviderType, S3Config};
        fn effective(profile: &Value, provider_type: ProviderType) -> ProviderConfig {
            let host = profile.get("host").and_then(Value::as_str).unwrap_or("");
            let (extra, host) =
                crate::profile_loader::profile_connection_extra(profile, provider_type, host);
            ProviderConfig {
                name: "probe".into(),
                provider_type,
                host,
                port: None,
                username: Some("user".into()),
                password: Some("secret".into()),
                initial_path: None,
                extra,
            }
        }
        let verify = |endpoint: &str| {
            let profile = json!({"host": "", "protocol": "s3", "providerId": "filen-desktop-s3",
                "options": {"endpoint": endpoint, "bucket": "b"}});
            S3Config::from_provider_config(&effective(&profile, ProviderType::S3))
                .unwrap()
                .verify_cert
        };
        assert!(verify("https://s3.attacker.example"));
        assert!(!verify("https://local.s3.filen.io:1800"));
        let cleartext = |host: &str| {
            let profile =
                json!({"host": host, "protocol": "swift", "providerId": "blomp", "options": {}});
            SwiftConfig::from_provider_config(&effective(&profile, ProviderType::Swift))
                .unwrap()
                .allow_cleartext_storage_endpoint
        };
        assert!(!cleartext("https://keystone.attacker.example"));
        assert!(cleartext("https://authenticate.blomp.com"));
        // A bare `blomp` protocol names the preset without a providerId.
        let bare = json!({"host": "https://keystone.attacker.example", "protocol": "blomp", "options": {}});
        assert!(
            !SwiftConfig::from_provider_config(&effective(&bare, ProviderType::Swift))
                .unwrap()
                .allow_cleartext_storage_endpoint
        );
    }

    /// CodeRabbit on #1524: a loopback Swift auth host exempts nothing, and
    /// the effective config shows it: reset, the certificate check is on for
    /// whatever host the catalog names; kept, it stays off as asked.
    #[test]
    fn a_loopback_swift_profile_resets_its_certificate_check() {
        use crate::providers::swift::SwiftConfig;
        use crate::providers::{ProviderConfig, ProviderType};
        let effective = |profile: &Value| {
            let (extra, host) = crate::profile_loader::profile_connection_extra(
                profile,
                ProviderType::Swift,
                profile.get("host").and_then(Value::as_str).unwrap_or(""),
            );
            SwiftConfig::from_provider_config(&ProviderConfig {
                name: "probe".into(),
                provider_type: ProviderType::Swift,
                host,
                port: None,
                username: Some("user".into()),
                password: Some("secret".into()),
                initial_path: None,
                extra,
            })
            .unwrap()
            .verify_cert
        };
        let relaxed = json!({"host": "http://127.0.0.1:5000", "protocol": "swift",
            "options": {"verifyCert": false}});
        let mut reset = relaxed.clone();
        assert_eq!(make_profile_safe(&mut reset, &[]), vec!["verifyCert"]);
        assert!(effective(&reset));
        let mut kept = relaxed.clone();
        assert!(make_profile_safe(&mut kept, &["verifyCert".to_string()]).is_empty());
        assert!(!effective(&kept));
    }

    /// An overwrite that brings a setting the existing profile already has,
    /// same key and value, imports nothing new.
    #[test]
    fn a_relaxation_the_profile_already_has_is_not_new() {
        let existing = json!({"host": "dav.example.com", "options": {"verifyCert": false}});
        let incoming = security_relaxations(&json!({"host": "dav.example.com",
            "options": {"verifyCert": false, "trustPasvAddress": true}}));
        let fresh = new_relaxations(incoming, &existing);
        let keys: Vec<&str> = fresh.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["trustPasvAddress"]);
    }

    #[test]
    fn a_profile_without_options_is_left_alone() {
        let mut profile = json!({"host": "files.example.com"});
        assert!(make_profile_safe(&mut profile, &[]).is_empty());
        assert_eq!(profile, json!({"host": "files.example.com"}));
    }
}
