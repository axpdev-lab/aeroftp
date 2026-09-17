//! CredentialProvider trait: abstracts credential/vault access
//!
//! Tauri implementation reads from vault.db (AES-256-GCM + Argon2id).
//! CLI implementation reads from vault cache (if open) or env vars
//! (AEROFTP_HOST / AEROFTP_USER / AEROFTP_PASS).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/// Server profile metadata (no secrets).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// G27: the crypt-overlay lane this profile is bound to (`aerocrypt` or
    /// `rclone-crypt`), or `None` when it carries no enabled binding. Carried
    /// on the profile rather than recomputed by each surface, because the
    /// binding is dropped on the way here: `mcp::load_safe_profiles` reduces a
    /// profile to a handful of keys, so a surface downstream of it has nothing
    /// left to derive from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crypt_overlay: Option<String>,
    /// The profile-aware protocol class (`Crypt` for a bound profile, otherwise
    /// the transport family). Empty when it was not derived.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub protocol_class: String,
}

impl ServerProfile {
    /// The one place a saved profile's JSON becomes a [`ServerProfile`].
    ///
    /// G27: the Tauri, CLI and MCP providers each built this record by hand
    /// from the same JSON, which is how the two crypt fields came to exist on
    /// one surface and not the others. The defaults are declared here rather
    /// than repeated: a profile with no `id` is not a profile (hence `None`),
    /// and the rest fall back to the values all three already used — empty
    /// strings, port 0, and `ftp` as the protocol.
    ///
    /// Reading a profile that has already been through
    /// `mcp::load_safe_profiles` is lossless for these fields, because that
    /// function now carries the two derived values forward.
    pub fn from_profile_json(p: &serde_json::Value) -> Option<Self> {
        let str_or = |key: &str, fallback: &str| -> String {
            p.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or(fallback)
                .to_string()
        };
        // A profile that arrives pre-derived (via `load_safe_profiles`) keeps
        // its answer; a raw one is classified here. Both routes run the same
        // library rule, so the two can never disagree.
        let crypt_overlay = match p.get("cryptOverlay") {
            Some(v) => v.as_str().map(String::from),
            None => crate::crypt_overlay_provider::profile_crypt_overlay_kind(p).map(String::from),
        };
        let protocol_class = p
            .get("protocolClass")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                crate::crypt_overlay_provider::profile_protocol_class(p).to_string()
            });

        Some(Self {
            id: p.get("id")?.as_str()?.to_string(),
            name: str_or("name", ""),
            host: str_or("host", ""),
            port: p.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16,
            username: str_or("username", ""),
            protocol: str_or("protocol", "ftp"),
            initial_path: p
                .get("initialPath")
                .and_then(|v| v.as_str())
                .map(String::from),
            provider_id: p
                .get("providerId")
                .and_then(|v| v.as_str())
                .map(String::from),
            crypt_overlay,
            protocol_class,
        })
    }
}

/// Server credentials (secrets).
#[derive(Clone)]
pub struct ServerCredentials {
    pub server: String,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for ServerCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerCredentials")
            .field("server", &self.server)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// Extra provider-specific options (region, bucket, endpoint, etc.)
pub type ProviderExtraOptions = std::collections::HashMap<String, String>;

/// Abstraction over credential storage.
pub trait CredentialProvider: Send + Sync {
    /// List all saved server profiles (no passwords).
    fn list_servers(&self) -> Result<Vec<ServerProfile>, String>;

    /// Load credentials for a specific server (by ID or fuzzy name match).
    fn get_credentials(&self, server_id: &str) -> Result<ServerCredentials, String>;

    /// Load provider-specific extra options for a server.
    fn get_extra_options(&self, server_id: &str) -> Result<ProviderExtraOptions, String>;
}
