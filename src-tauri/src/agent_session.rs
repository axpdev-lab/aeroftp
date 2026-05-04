// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Agent-facing single-shot connect surface.
//!
//! Replaces the agent boilerplate of `connect → about → df → ls /` with
//! one call (`aeroftp-cli agent-connect`, MCP `aeroftp_agent_connect`)
//! that returns a payload with a per-block status. The agent reads
//! `connect.status` for the go/no-go decision and gracefully degrades on
//! `unsupported` / `unavailable` / `error` blocks (e.g. it skips
//! quota-aware suggestions when `quota.status != "ok"` but still kicks
//! off the transfer).
//!
//! Block status values: `ok`, `unsupported`, `unavailable`, `error`.

use crate::credential_store::CredentialStore;
use crate::providers::{
    ProviderConfig, ProviderError, ProviderFactory, ProviderType, StorageProvider,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Instant;

/// Minimal profile metadata exposed to agents: all fields safe to log
/// (no secrets). Everything else stays in the vault.
#[derive(Debug, Clone)]
pub struct ProfileSummary {
    pub id: String,
    pub name: String,
    pub protocol: String,
    pub host: String,
    pub username: String,
    pub port: Option<u16>,
    pub initial_path: String,
    /// Provider-specific options pulled from `profile.options` and
    /// flattened into the `ProviderConfig.extra` shape (snake_case
    /// string keys/values). S3 (`bucket`, `region`, `endpoint`),
    /// FTP/FTPS (`tls_mode`, `verify_cert`), SFTP
    /// (`private_key_path`, `key_passphrase`, `trust_unknown_hosts`),
    /// Azure (`container`, `endpoint`), MEGA (`mega_mode`,
    /// `save_session`), etc. Empty when the profile has no options
    /// blob.
    pub extras: HashMap<String, String>,
}

/// Convert camelCase JSON keys (the desktop frontend's `ProviderOptions`
/// shape) into the snake_case keys the Rust `ProviderConfig.extra`
/// builders expect. Pure ASCII: non-alpha chars are preserved as-is.
fn camel_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Flatten the profile's `options` JSON object into the snake_case
/// string map providers consume via `ProviderConfig.extra`. Skips
/// `null` and any non-scalar values; arrays/objects don't currently
/// have a representation in the `extra` map and would silently confuse
/// the provider if forwarded.
fn flatten_options(options: Option<&Value>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(obj) = options.and_then(|v| v.as_object()) else {
        return out;
    };
    for (k, v) in obj {
        let key = camel_to_snake(k);
        let value = match v {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            // Skip nulls / arrays / nested objects: the legacy
            // builders never accepted those shapes either.
            _ => continue,
        };
        out.insert(key, value);
    }
    out
}

#[derive(Debug)]
pub enum LookupError {
    /// Vault isn't open in the current process. Caller should ask the
    /// user to unlock before retrying.
    VaultClosed,
    /// Vault is open but the profiles blob couldn't be read or parsed.
    ProfilesUnavailable(String),
    /// Query didn't match any saved profile.
    NotFound(String),
    /// Query matched more than one profile by substring; agent should
    /// pick one of `candidates` and retry with an exact name.
    Ambiguous {
        query: String,
        candidates: Vec<String>,
    },
}

impl std::fmt::Display for LookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VaultClosed => write!(f, "Vault not open. Unlock the vault before connecting."),
            Self::ProfilesUnavailable(e) => write!(f, "Profiles unavailable: {e}"),
            Self::NotFound(q) => write!(f, "Server '{q}' not found in saved profiles"),
            Self::Ambiguous { query, candidates } => write!(
                f,
                "Server '{query}' is ambiguous. Use an exact profile name. Matches: {}",
                candidates.join(", ")
            ),
        }
    }
}

/// Resolve a profile by exact name/id, falling back to a unique
/// substring match. Mirrors the lookup rules in
/// `bin/aeroftp_cli.rs::create_and_connect_for_agent` so CLI and MCP
/// agree on what "server X" means.
pub fn lookup_profile(query: &str) -> Result<ProfileSummary, LookupError> {
    let store = CredentialStore::from_cache().ok_or(LookupError::VaultClosed)?;
    let profiles_json = store
        .get("config_server_profiles")
        .map_err(|e| LookupError::ProfilesUnavailable(e.to_string()))?;
    let profiles: Vec<Value> = serde_json::from_str(&profiles_json)
        .map_err(|e| LookupError::ProfilesUnavailable(e.to_string()))?;

    let query_lower = query.to_lowercase();
    let exact_match = profiles.iter().find(|p| {
        let name = p
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
        name == query_lower || id == query
    });

    let matched = if let Some(p) = exact_match {
        p
    } else {
        let partials: Vec<&Value> = profiles
            .iter()
            .filter(|p| {
                let name = p
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                name.contains(&query_lower)
            })
            .collect();
        match partials.as_slice() {
            [single] => *single,
            [] => return Err(LookupError::NotFound(query.to_string())),
            many => {
                let candidates = many
                    .iter()
                    .filter_map(|p| p.get("name").and_then(|v| v.as_str()).map(str::to_string))
                    .collect();
                return Err(LookupError::Ambiguous {
                    query: query.to_string(),
                    candidates,
                });
            }
        }
    };

    Ok(ProfileSummary {
        id: matched
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        name: matched
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        protocol: matched
            .get("protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        host: matched
            .get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        username: matched
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        port: matched
            .get("port")
            .and_then(|v| v.as_u64())
            .map(|p| p as u16),
        initial_path: matched
            .get("initialPath")
            .and_then(|v| v.as_str())
            .unwrap_or("/")
            .to_string(),
        extras: flatten_options(matched.get("options")),
    })
}

/// Collapse `//`, drop trailing `/`, normalise empty → `/`.
/// Pure string op: no provider call.
pub fn canonicalize_path(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    let mut out = String::with_capacity(path.len() + 1);
    let mut last_was_slash = false;
    for ch in path.chars() {
        if ch == '/' {
            if last_was_slash {
                continue;
            }
            last_was_slash = true;
        } else {
            last_was_slash = false;
        }
        out.push(ch);
    }
    if !out.starts_with('/') {
        out.insert(0, '/');
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// Conservative per-protocol capability set. Static heuristic so CLI
/// and MCP agree without requiring the connection pool to surface
/// per-instance flags. Names mirror the `StorageProvider` trait
/// `supports_*` methods: agents that already know those flags can
/// reuse the same vocabulary.
///
/// When in doubt, a feature is OMITTED. Agents should treat this list
/// as a lower bound: if a feature is here, the protocol family
/// reliably supports it; absence means "ask the provider directly or
/// fall back to a portable code path".
pub fn capabilities_for_protocol(protocol: &str) -> Vec<&'static str> {
    match protocol.to_ascii_lowercase().as_str() {
        "ftp" | "ftps" => vec!["resume", "find"],
        "sftp" => vec!["resume", "find", "chmod", "symlinks", "permissions"],
        "webdav" => vec!["server_copy", "find"],
        "s3" => vec!["server_copy", "checksum"],
        "azure" => vec!["server_copy", "checksum"],
        "googledrive" | "googlephotos" => vec![
            "server_copy",
            "share_links",
            "versions",
            "change_tracking",
            "thumbnails",
        ],
        "dropbox" => vec![
            "server_copy",
            "share_links",
            "versions",
            "change_tracking",
            "thumbnails",
        ],
        "onedrive" => vec![
            "server_copy",
            "share_links",
            "versions",
            "change_tracking",
            "thumbnails",
        ],
        "box" => vec!["server_copy", "share_links", "versions"],
        "pcloud" => vec!["server_copy", "share_links", "versions", "thumbnails"],
        "mega" => vec!["server_copy", "share_links", "thumbnails"],
        "filen" => vec!["server_copy", "share_links"],
        "internxt" => vec!["share_links"],
        "kdrive" => vec!["server_copy", "share_links", "thumbnails"],
        "jottacloud" => vec!["server_copy", "share_links", "versions"],
        "zohoworkdrive" => vec!["server_copy", "share_links", "versions"],
        "yandexdisk" => vec!["server_copy", "share_links", "thumbnails"],
        "koofr" => vec!["server_copy", "share_links", "thumbnails"],
        "opendrive" => vec!["server_copy", "share_links"],
        "drime" => vec!["server_copy", "share_links"],
        "filelu" => vec!["share_links", "versions"],
        "fourshared" => vec!["share_links"],
        "swift" => vec!["server_copy", "checksum"],
        "immich" => vec!["thumbnails"],
        "github" | "gitlab" => vec!["share_links", "versions"],
        _ => vec![],
    }
}

/// Profile block: always present, summarises which profile the agent
/// is talking about. Never carries `status` (it's metadata, not a step
/// that can fail).
pub fn profile_block(profile: &ProfileSummary) -> Value {
    let mut obj = json!({
        "id": profile.id,
        "name": profile.name,
        "protocol": profile.protocol,
    });
    if !profile.host.is_empty() {
        obj["host"] = json!(profile.host);
    }
    if !profile.username.is_empty() {
        obj["username"] = json!(profile.username);
    }
    if let Some(p) = profile.port {
        obj["port"] = json!(p);
    }
    obj
}

pub fn path_block(profile: &ProfileSummary) -> Value {
    json!({
        "status": "ok",
        "value": profile.initial_path,
        "canonical": canonicalize_path(&profile.initial_path),
    })
}

pub fn capabilities_block(protocol: &str) -> Value {
    let features = capabilities_for_protocol(protocol);
    json!({
        "status": "ok",
        "features": features,
    })
}

/// Quota block builder. Some WebDAV servers return `total=0, used=0`
/// when their backend doesn't expose quota (e.g. Koofr's WebDAV, some
/// InfiniCloud configs); that shape is indistinguishable from "account
/// genuinely empty". Downgrade those to `unsupported` so agents don't
/// branch on misleading zeros: discovered via the 4-Sonnet
/// agent-friendliness audit (2026-04-26, Battery B).
pub fn quota_block_ok(used: u64, total: u64, available: u64) -> Value {
    if total == 0 {
        return json!({
            "status": "unsupported",
            "reason": "server_returned_zero_total",
        });
    }
    json!({
        "status": "ok",
        "used_bytes": used,
        "total_bytes": total,
        "free_bytes": available,
    })
}

pub fn quota_block_unsupported(protocol: &str) -> Value {
    json!({
        "status": "unsupported",
        "provider": protocol,
    })
}

pub fn quota_block_unavailable(reason: &str) -> Value {
    json!({
        "status": "unavailable",
        "reason": reason,
    })
}

pub fn connect_block_ok(session_token: &str, elapsed_ms: u128) -> Value {
    json!({
        "status": "ok",
        "session_token": session_token,
        "elapsed_ms": elapsed_ms,
    })
}

pub fn connect_block_error(message: &str) -> Value {
    json!({
        "status": "error",
        "message": message,
    })
}

/// Protocol is outside agent-connect's live-connect allowlist (e.g.
/// pCloud, Filen, Koofr native, MEGA, Yandex) but the rest of the
/// payload: capabilities, path, profile metadata: is still useful.
/// Distinct status from `error` so agents can distinguish "we never
/// tried because protocol unsupported" from "we tried and it failed".
/// Maps to exit code 0 in the CLI (capabilities are still actionable).
pub fn connect_block_unsupported(protocol: &str) -> Value {
    json!({
        "status": "unsupported",
        "reason": format!(
            "Protocol '{protocol}' is not in agent-connect's live-connect allowlist. \
             capabilities/path/profile blocks are still valid; use protocol-specific commands to connect."
        ),
        "supported_protocols": ["ftp", "ftps", "sftp", "webdav", "s3", "github", "gitlab"],
    })
}

/// Build a top-level error payload for the case where profile lookup
/// itself failed (no profile block possible). Keeps the same shape as
/// successful payloads so agents don't need a separate error path.
pub fn lookup_error_payload(query: &str, err: &LookupError) -> Value {
    let kind = match err {
        LookupError::VaultClosed => "vault_closed",
        LookupError::ProfilesUnavailable(_) => "profiles_unavailable",
        LookupError::NotFound(_) => "not_found",
        LookupError::Ambiguous { .. } => "ambiguous",
    };
    let mut obj = json!({
        "query": query,
        "lookup": {
            "status": "error",
            "kind": kind,
            "message": err.to_string(),
        }
    });
    if let LookupError::Ambiguous { candidates, .. } = err {
        obj["lookup"]["candidates"] = json!(candidates);
    }
    obj
}

/// Map a profile's protocol string onto the small set of providers
/// `agent-connect` will actually drive. Returns `None` for protocols
/// outside the supported set (the caller surfaces that as
/// `connect.status = "unsupported"`).
fn resolve_provider_type(protocol: &str) -> Option<ProviderType> {
    match protocol.to_uppercase().as_str() {
        "FTP" => Some(ProviderType::Ftp),
        "FTPS" => Some(ProviderType::Ftps),
        "SFTP" => Some(ProviderType::Sftp),
        "WEBDAV" | "WEBDAVS" => Some(ProviderType::WebDav),
        "S3" => Some(ProviderType::S3),
        "GITHUB" => Some(ProviderType::GitHub),
        "GITLAB" => Some(ProviderType::GitLab),
        _ => None,
    }
}

/// Outcome of attempting to bring a profile up to a live connection.
/// `Unsupported` is distinct from `Err` so the caller can populate the
/// `connect.status: "unsupported"` block (capabilities/path stay
/// valid) instead of the harsher `error` shape reserved for tried-and-
/// failed attempts.
enum ConnectOutcome {
    Connected(Box<dyn StorageProvider>),
    Unsupported,
    Err(String),
}

/// Attempt to instantiate + connect a provider for the given profile.
/// Reads the per-profile credential blob from the open vault.
async fn connect_provider(profile: &ProfileSummary) -> ConnectOutcome {
    let Some(store) = CredentialStore::from_cache() else {
        return ConnectOutcome::Err("Vault not open".to_string());
    };
    let password = store
        .get(&format!("server_{}", profile.id))
        .unwrap_or_default();

    let Some(provider_type) = resolve_provider_type(&profile.protocol) else {
        return ConnectOutcome::Unsupported;
    };

    let mut extra = profile.extras.clone();

    // Azure stores its container under `options.bucket` in the desktop
    // schema (the UI shares the bucket field across S3/Azure), but the
    // provider expects it under `container`. Mirror what
    // `provider_commands::to_provider_config` does so agent-connect
    // doesn't fail with a "container required" error on profiles that
    // work fine in the GUI.
    if matches!(provider_type, ProviderType::Azure) {
        if let Some(bucket) = extra.remove("bucket") {
            extra.entry("container".to_string()).or_insert(bucket);
        }
    }

    // FTPS without an explicit tls_mode in the profile: default to
    // implicit (port 990 convention). Matches the GUI default.
    if matches!(provider_type, ProviderType::Ftps) && !extra.contains_key("tls_mode") {
        extra.insert("tls_mode".to_string(), "implicit".to_string());
    }

    let config = ProviderConfig {
        name: profile.name.clone(),
        provider_type,
        host: profile.host.clone(),
        port: profile.port,
        username: if profile.username.is_empty() {
            None
        } else {
            Some(profile.username.clone())
        },
        password: if password.is_empty() {
            None
        } else {
            Some(password)
        },
        initial_path: Some(profile.initial_path.clone()),
        extra,
    };

    let mut provider = match ProviderFactory::create(&config) {
        Ok(p) => p,
        Err(e) => return ConnectOutcome::Err(format!("Failed to create provider: {e}")),
    };
    if let Err(e) = provider.connect().await {
        return ConnectOutcome::Err(format!("Connection failed: {e}"));
    }
    ConnectOutcome::Connected(provider)
}

/// Build the full agent-connect payload for the CLI surface. Opens the
/// vault, locates the profile, attempts to connect, then collects
/// per-block status. Always returns a payload: failures surface as
/// `status: error` / `unavailable` on the offending block.
pub async fn build_agent_connect_payload(query: &str) -> Value {
    let profile = match lookup_profile(query) {
        Ok(p) => p,
        Err(e) => return lookup_error_payload(query, &e),
    };

    let path = path_block(&profile);
    let capabilities = capabilities_block(&profile.protocol);

    let connect_started = Instant::now();
    let connect_result = connect_provider(&profile).await;
    let elapsed_ms = connect_started.elapsed().as_millis();

    let (connect, quota) = match connect_result {
        ConnectOutcome::Connected(mut provider) => {
            let connect = connect_block_ok(&profile.id, elapsed_ms);
            let quota = match provider.storage_info().await {
                Ok(info) => quota_block_ok(info.used, info.total, info.free),
                Err(ProviderError::NotSupported(_)) => quota_block_unsupported(&profile.protocol),
                Err(e) => quota_block_unavailable(&e.to_string()),
            };
            (connect, quota)
        }
        ConnectOutcome::Unsupported => {
            // Capabilities/path are still actionable for the agent -
            // they can choose protocol-specific commands. Quota is
            // marked unsupported (matches what FTP/S3 produce when the
            // protocol legitimately has no quota API).
            (
                connect_block_unsupported(&profile.protocol),
                quota_block_unsupported(&profile.protocol),
            )
        }
        ConnectOutcome::Err(msg) => (
            connect_block_error(&msg),
            quota_block_unavailable("connect failed"),
        ),
    };

    json!({
        "profile": profile_block(&profile),
        "connect": connect,
        "capabilities": capabilities,
        "quota": quota,
        "path": path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_collapses_slashes_and_strips_trailing() {
        assert_eq!(canonicalize_path(""), "/");
        assert_eq!(canonicalize_path("/"), "/");
        assert_eq!(canonicalize_path("//"), "/");
        assert_eq!(canonicalize_path("/foo"), "/foo");
        assert_eq!(canonicalize_path("/foo/"), "/foo");
        assert_eq!(canonicalize_path("/foo//bar"), "/foo/bar");
        assert_eq!(canonicalize_path("foo/bar"), "/foo/bar");
        assert_eq!(canonicalize_path("/foo/bar/"), "/foo/bar");
    }

    #[test]
    fn capabilities_known_protocols_have_features() {
        // Spot-check that the lookup table doesn't silently regress to
        // empty for the headline protocols.
        for proto in ["ftp", "sftp", "s3", "webdav", "googledrive", "dropbox"] {
            let feats = capabilities_for_protocol(proto);
            assert!(
                !feats.is_empty(),
                "{proto} should have at least one feature"
            );
        }
    }

    #[test]
    fn capabilities_is_case_insensitive() {
        assert_eq!(
            capabilities_for_protocol("S3"),
            capabilities_for_protocol("s3")
        );
        assert_eq!(
            capabilities_for_protocol("GoogleDrive"),
            capabilities_for_protocol("googledrive")
        );
    }

    #[test]
    fn capabilities_unknown_protocol_is_empty() {
        assert!(capabilities_for_protocol("xyzzy").is_empty());
    }

    #[test]
    fn block_helpers_carry_status() {
        // Status is the agent's primary read: make sure each helper
        // sets it to the documented value.
        assert_eq!(connect_block_ok("srv_x", 100)["status"], "ok");
        assert_eq!(connect_block_error("boom")["status"], "error");
        assert_eq!(connect_block_unsupported("pcloud")["status"], "unsupported");
        assert_eq!(quota_block_ok(1, 100, 99)["status"], "ok");
        assert_eq!(quota_block_unsupported("ftp")["status"], "unsupported");
        assert_eq!(quota_block_unavailable("x")["status"], "unavailable");
    }

    #[test]
    fn quota_block_ok_zero_total_downgrades_to_unsupported() {
        // Some WebDAV servers (Koofr, InfiniCloud variants) return 0/0
        // when their backend has no quota: surface as `unsupported`
        // so agents don't branch on misleading zeros. Caught by the
        // 4-Sonnet agent audit (Battery B, 2026-04-26).
        let v = quota_block_ok(0, 0, 0);
        assert_eq!(v["status"], "unsupported");
        assert_eq!(v["reason"], "server_returned_zero_total");
        assert!(
            v.get("used_bytes").is_none(),
            "should NOT carry zero counters"
        );
    }

    #[test]
    fn quota_block_ok_nonzero_total_stays_ok() {
        let v = quota_block_ok(10, 100, 90);
        assert_eq!(v["status"], "ok");
        assert_eq!(v["used_bytes"], 10);
        assert_eq!(v["total_bytes"], 100);
        assert_eq!(v["free_bytes"], 90);
    }

    #[test]
    fn connect_block_unsupported_lists_supported_protocols() {
        let v = connect_block_unsupported("pcloud");
        assert_eq!(v["status"], "unsupported");
        let supported = v["supported_protocols"].as_array().unwrap();
        for proto in ["ftp", "ftps", "sftp", "webdav", "s3", "github", "gitlab"] {
            assert!(
                supported.iter().any(|x| x == proto),
                "supported_protocols must include {proto}"
            );
        }
    }

    #[test]
    fn lookup_error_payload_carries_kind() {
        let err = LookupError::NotFound("ghost".to_string());
        let payload = lookup_error_payload("ghost", &err);
        assert_eq!(payload["lookup"]["status"], "error");
        assert_eq!(payload["lookup"]["kind"], "not_found");
        assert_eq!(payload["query"], "ghost");
    }

    #[test]
    fn camel_to_snake_handles_typical_keys() {
        assert_eq!(camel_to_snake("bucket"), "bucket");
        assert_eq!(camel_to_snake("tlsMode"), "tls_mode");
        assert_eq!(camel_to_snake("verifyCert"), "verify_cert");
        assert_eq!(camel_to_snake("privateKeyPath"), "private_key_path");
        assert_eq!(camel_to_snake("trustUnknownHosts"), "trust_unknown_hosts");
        assert_eq!(camel_to_snake("sseKmsKeyId"), "sse_kms_key_id");
    }

    #[test]
    fn flatten_options_skips_complex_values() {
        let v = json!({
            "bucket": "my-bucket",
            "verifyCert": false,
            "port": 9000,
            "tlsMode": "implicit",
            "ignoredArr": [1, 2],
            "ignoredObj": {"k": "v"},
            "ignoredNull": null,
        });
        let m = flatten_options(Some(&v));
        assert_eq!(m.get("bucket"), Some(&"my-bucket".to_string()));
        assert_eq!(m.get("verify_cert"), Some(&"false".to_string()));
        assert_eq!(m.get("port"), Some(&"9000".to_string()));
        assert_eq!(m.get("tls_mode"), Some(&"implicit".to_string()));
        assert!(!m.contains_key("ignored_arr"));
        assert!(!m.contains_key("ignored_obj"));
        assert!(!m.contains_key("ignored_null"));
    }

    #[test]
    fn flatten_options_handles_missing_blob() {
        assert!(flatten_options(None).is_empty());
        assert!(flatten_options(Some(&Value::Null)).is_empty());
    }

    #[test]
    fn ambiguous_lookup_includes_candidates() {
        let err = LookupError::Ambiguous {
            query: "prod".to_string(),
            candidates: vec!["prod-a".to_string(), "prod-b".to_string()],
        };
        let payload = lookup_error_payload("prod", &err);
        assert_eq!(payload["lookup"]["kind"], "ambiguous");
        assert_eq!(payload["lookup"]["candidates"], json!(["prod-a", "prod-b"]));
    }
}
