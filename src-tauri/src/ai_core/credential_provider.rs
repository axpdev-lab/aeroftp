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
    /// Where the profile's sign-in lives
    /// ([`crate::profile_auth_state::profile_auth_source`]): derived here for
    /// the same reason as `crypt_overlay`, since the options it reads do not
    /// survive `mcp::load_safe_profiles`.
    #[serde(default = "default_auth_source", skip_serializing)]
    pub auth_source: crate::profile_auth_state::AuthSource,
}

fn default_auth_source() -> crate::profile_auth_state::AuthSource {
    crate::profile_auth_state::AuthSource::Vault
}

impl ServerProfile {
    /// What the profile connects to, and nothing that only names or labels
    /// it: an approval is bound to this as well as to the id, because a
    /// profile edited to another host keeps its id.
    pub fn connection_identity(&self) -> String {
        serde_json::json!([
            self.protocol,
            self.host,
            self.port,
            self.username,
            self.initial_path,
            self.provider_id,
            self.crypt_overlay,
        ])
        .to_string()
    }

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

        let auth_source = p
            .get("authSource")
            .and_then(|v| v.as_str())
            .and_then(crate::profile_auth_state::AuthSource::parse)
            .unwrap_or_else(|| crate::profile_auth_state::profile_auth_source(p));

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
            auth_source,
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

/// The saved profile `query` names exactly, as its index in `profiles`: a
/// profile whose id is `query` first, else one whose name is `query` ignoring
/// case. Every surface resolves a server in this one order (the desktop
/// tools, the MCP pool behind the CLI agent, and the approval that binds a
/// change to profile ids), so a profile named like another profile's id is
/// never approved as one profile and run as the other. `None` when nothing is
/// named exactly; a caller may then try a part of a name, as reads do.
/// What a saved profile's JSON connects to, as an approval sees it: the same
/// value [`ServerProfile::connection_identity`] gives, read from the JSON the
/// connection is then opened with.
pub fn profile_identity(profile: &serde_json::Value) -> Option<String> {
    ServerProfile::from_profile_json(profile).map(|profile| profile.connection_identity())
}

/// An error when a call was approved for a profile that connects elsewhere
/// now; nothing is opened then. `expected` is the identity the approval saw,
/// `current` the identity of the profile about to be connected; no
/// `expected` (nothing was approved for it) passes.
pub fn approved_target_unchanged(
    server_query: &str,
    expected: Option<&str>,
    current: Option<&str>,
) -> Result<(), String> {
    match expected {
        Some(expected) if Some(expected) != current => Err(format!(
            "The saved server '{server_query}' changed after the call was approved; nothing was done"
        )),
        _ => Ok(()),
    }
}

pub fn exact_profile_index<T>(
    profiles: &[T],
    query: &str,
    id_and_name: impl Fn(&T) -> (&str, &str),
) -> Option<usize> {
    profiles
        .iter()
        .position(|profile| id_and_name(profile).0 == query)
        .or_else(|| {
            let lower = query.to_lowercase();
            profiles
                .iter()
                .position(|profile| id_and_name(profile).1.to_lowercase() == lower)
        })
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

#[cfg(test)]
mod tests {
    use super::exact_profile_index;

    /// Greptile on #1522: a profile named like another profile's id was
    /// approved as one and run as the other, because surfaces resolved in
    /// different orders. An id wins over a name everywhere.
    #[test]
    fn an_id_wins_over_a_profile_named_like_it_and_then_a_name_ignoring_case() {
        let profiles = [("srv_1", "srv_2"), ("srv_2", "Lab")];
        fn id_and_name<'a>(p: &'a (&'static str, &'static str)) -> (&'a str, &'a str) {
            (p.0, p.1)
        }
        assert_eq!(
            exact_profile_index(&profiles, "srv_2", id_and_name),
            Some(1)
        );
        assert_eq!(exact_profile_index(&profiles, "LAB", id_and_name), Some(1));
        assert_eq!(
            exact_profile_index(&profiles, "srv_1", id_and_name),
            Some(0)
        );
        assert_eq!(exact_profile_index(&profiles, "la", id_and_name), None);
        // With the id profile gone, the same query names the other one.
        assert_eq!(
            exact_profile_index(&profiles[..1], "srv_2", id_and_name),
            Some(0)
        );
    }
}
