// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Per-profile auth-readiness derivation from local vault state.
//!
//! Used by the CLI (`profiles --json`, `agent-bootstrap`, `agent-info`)
//! and by the MCP/agent tool surface (`aeroftp_list_servers`) so a single
//! source of truth answers "is this profile ready to connect right now,
//! or does it need user-side intervention before any operation will
//! succeed?". Pure local: never touches the network.

use crate::credential_store::CredentialStore;
use crate::providers::oauth2::StoredTokens;
use serde_json::Value;
use std::collections::HashSet;

/// Where a profile's sign-in lives, read from the profile itself: its
/// protocol, its preset and the options that pick a mode.
///
/// The state was derived from the protocol alone, as if every profile kept a
/// password in `server_<id>` or a provider token: every profile that
/// connects without either read `no_credentials`, so an agent told "not
/// signed in, skip it" about an anonymous FTP site, an SFTP key, a local
/// bridge, a phone, or a Proton Drive signed in on its own page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthSource {
    /// A password or token in `server_<id>`, or the provider's token blob
    /// (OAuth, Jottacloud).
    Vault,
    /// The native Proton Drive session (`proton_native_session_<id>`, or the
    /// one saved before the profile was).
    ProtonSession,
    /// Nothing secret to hold: anonymous FTP and WebDAV, an SFTP key file, the
    /// local bridges of Filen Desktop (admin/admin unless changed), a portable
    /// device, a local folder, an AeroShare ticket kept in the profile.
    NoSecret,
    /// A session kept by another program, in a place this vault cannot read:
    /// Proton's official CLI and MEGAcmd.
    External,
}

impl AuthSource {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthSource::Vault => "vault",
            AuthSource::ProtonSession => "proton_session",
            AuthSource::NoSecret => "no_secret",
            AuthSource::External => "external",
        }
    }

    /// The inverse of [`AuthSource::as_str`], for a profile that carries the
    /// derived value instead of its options (`mcp::load_safe_profiles`).
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "vault" => AuthSource::Vault,
            "proton_session" => AuthSource::ProtonSession,
            "no_secret" => AuthSource::NoSecret,
            "external" => AuthSource::External,
            _ => return None,
        })
    }
}

/// A profile option by its canonical key, whichever spelling was saved
/// (`private_key_path` or `privateKeyPath`), trimmed, `None` when empty.
fn profile_option<'a>(profile: &'a Value, key: &str) -> Option<&'a str> {
    profile
        .get("options")?
        .as_object()?
        .iter()
        .find(|(k, _)| crate::profile_loader::canonicalize_profile_option_key(k) == key)
        .and_then(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn profile_option_is_true(profile: &Value, key: &str) -> bool {
    profile
        .get("options")
        .and_then(Value::as_object)
        .and_then(|options| {
            options
                .iter()
                .find(|(k, _)| crate::profile_loader::canonicalize_profile_option_key(k) == key)
        })
        .is_some_and(|(_, v)| v.as_bool() == Some(true) || v.as_str() == Some("true"))
}

/// Where this saved profile's sign-in lives. Pure: reads the profile JSON
/// only, never the vault or the network.
pub fn profile_auth_source(profile: &Value) -> AuthSource {
    let protocol = profile
        .get("protocol")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let provider_id = profile
        .get("providerId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let username = profile
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    match protocol.as_str() {
        // The GUI's Proton tab saves `proton_mode: "native"`; a profile
        // without it uses the official CLI, as the provider factory routes it.
        "proton" | "protondrive" if profile_option(profile, "proton_mode") == Some("native") => {
            AuthSource::ProtonSession
        }
        "proton" | "protondrive" => AuthSource::External,
        "mega" if profile_option(profile, "mega_mode") == Some("megacmd") => AuthSource::External,
        _ if matches!(provider_id, "megacmd") => AuthSource::External,
        // FtpConfig signs in as `anonymous` when the username is empty.
        "ftp" | "ftps" if username.is_empty() || username.eq_ignore_ascii_case("anonymous") => {
            AuthSource::NoSecret
        }
        "webdav" | "webdavs" if profile_option_is_true(profile, "anonymous") => {
            AuthSource::NoSecret
        }
        "sftp" if profile_option(profile, "private_key_path").is_some() => AuthSource::NoSecret,
        "mtp" | "wpd" | "portabledevice" | "portable" | "local" | "file" | "localdirectory" => {
            AuthSource::NoSecret
        }
        "peer" | "aeroshare" if profile_option(profile, "peer_ticket").is_some() => {
            AuthSource::NoSecret
        }
        // `profile_loader::apply_local_bridge_credential_defaults` signs in
        // as admin/admin when nothing is saved.
        _ if matches!(provider_id, "filen-desktop-webdav" | "filen-desktop-s3") => {
            AuthSource::NoSecret
        }
        _ => AuthSource::Vault,
    }
}

/// Vault account that actually holds this profile's OAuth / Jotta blob,
/// honouring the per-profile key first and the protocol singleton second.
/// `None` when neither key is in `accounts`.
fn auth_token_account(
    protocol: &str,
    profile_id: &str,
    accounts: &HashSet<String>,
) -> Option<String> {
    if protocol.eq_ignore_ascii_case("jottacloud") && !profile_id.is_empty() {
        let per_profile = format!("jottacloud_refresh_{}", profile_id);
        if accounts.contains(&per_profile) {
            return Some(per_profile);
        }
    }
    if let Some(slug) = oauth_vault_slug_for_oauth_protocol(protocol) {
        if !profile_id.is_empty() {
            let per_profile = format!("oauth_{}_{}", slug, profile_id);
            if accounts.contains(&per_profile) {
                return Some(per_profile);
            }
        }
    }
    let singleton = oauth_vault_key_for_protocol(protocol)?;
    accounts.contains(singleton).then(|| singleton.to_string())
}

fn oauth_vault_slug_for_oauth_protocol(protocol: &str) -> Option<&'static str> {
    // Same slugs `oauth_vault_key_for_protocol` uses, minus Jottacloud
    // (that blob is `jottacloud_refresh`, not `oauth_*`).
    match protocol.to_ascii_lowercase().as_str() {
        "googledrive" => Some("google"),
        "googlephotos" => Some("googlephotos"),
        "dropbox" => Some("dropbox"),
        "onedrive" => Some("onedrive"),
        "box" => Some("box"),
        "pcloud" => Some("pcloud"),
        "zohoworkdrive" => Some("zohoworkdrive"),
        "yandexdisk" => Some("yandexdisk"),
        "fourshared" => Some("fourshared"),
        _ => None,
    }
}

/// Map a profile's `protocol` to the vault key that holds its OAuth /
/// refresh-token blob (the per-protocol singleton, NOT the per-profile
/// credential blob). Returns `None` for password-based protocols where the
/// credential is stored in `server_<profile_id>` and there's no
/// provider-level token.
///
/// Keep this in lockstep with `format!("oauth_{:?}", provider).to_lowercase()`
/// at `providers/oauth2.rs::OAuth2Manager::store_tokens`.
pub fn oauth_vault_key_for_protocol(protocol: &str) -> Option<&'static str> {
    match protocol.to_ascii_lowercase().as_str() {
        "googledrive" => Some("oauth_google"),
        "googlephotos" => Some("oauth_googlephotos"),
        "dropbox" => Some("oauth_dropbox"),
        "onedrive" => Some("oauth_onedrive"),
        "box" => Some("oauth_box"),
        "pcloud" => Some("oauth_pcloud"),
        "zohoworkdrive" => Some("oauth_zohoworkdrive"),
        "yandexdisk" => Some("oauth_yandexdisk"),
        "fourshared" => Some("oauth_fourshared"),
        // Jottacloud uses a one-use Personal Login Token + custom JFS
        // refresh flow, not the OAuth2Manager. The persisted refresh
        // token, when present, lives under this key.
        "jottacloud" => Some("jottacloud_refresh"),
        _ => None,
    }
}

/// Derive a profile's auth readiness from local vault state only: never
/// touches the network. Returns one of:
///   - `valid`          : credential present and (for OAuth) not expired, or
///     none needed (`AuthSource::NoSecret`)
///   - `expired`        : OAuth token past `expires_at` and no refresh token
///   - `needs_refresh`  : OAuth token past `expires_at` but refresh token present
///   - `no_credentials` : nothing stored; user has not signed in yet
///   - `unknown`        : vault entry present but value couldn't be parsed
///     (legacy/corrupt), or a session another program keeps
///     (`AuthSource::External`): treated as "agent should try anyway"
///
/// `source` is [`profile_auth_source`] of the profile. `accounts` is a
/// pre-fetched set of vault keys to keep this O(1) per profile when called in
/// a loop. The store handle is used only to decrypt OAuth blobs that need the
/// `expires_at` check; password-based protocols never trigger a decrypt.
pub fn derive_profile_auth_state(
    store: &CredentialStore,
    accounts: &HashSet<String>,
    profile_id: &str,
    protocol: &str,
    source: AuthSource,
) -> &'static str {
    let server_key = format!("server_{}", profile_id);
    match source {
        AuthSource::NoSecret => return "valid",
        // A saved password signs MEGAcmd in again; otherwise only the other
        // program knows whether its session is alive.
        AuthSource::External if accounts.contains(&server_key) => return "valid",
        AuthSource::External => return "unknown",
        AuthSource::ProtonSession => {
            let own = format!("proton_native_session_{}", profile_id);
            return if accounts.contains(&own) || accounts.contains("proton_native_session") {
                "valid"
            } else {
                "no_credentials"
            };
        }
        AuthSource::Vault => {}
    }
    let oauth_key = oauth_vault_key_for_protocol(protocol);

    let has_server = accounts.contains(&server_key);
    // Jottacloud import/persist writes `jottacloud_refresh_<id>`; the
    // protocol map still names the legacy singleton. Presence of either
    // key is enough: an imported profile that only has the per-profile
    // blob must not report `no_credentials`.
    let token_account = auth_token_account(protocol, profile_id, accounts);

    if oauth_key.is_some() {
        let Some(key) = token_account else {
            return "no_credentials";
        };
        match store.get(&key) {
            Ok(json) => {
                if let Ok(tokens) = serde_json::from_str::<StoredTokens>(&json) {
                    if tokens.is_expired() {
                        if tokens.refresh_token.is_some() {
                            return "needs_refresh";
                        }
                        return "expired";
                    }
                    return "valid";
                }
                // Jottacloud's `jottacloud_refresh` is a different shape
                // (raw refresh token JSON, no expires_at). Presence ==
                // valid until the next request proves otherwise.
                "valid"
            }
            Err(_) => "unknown",
        }
    } else if has_server {
        "valid"
    } else {
        "no_credentials"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Each way a profile signs in, read from the profile as saved.
    #[test]
    fn where_a_sign_in_lives_is_read_from_the_profile() {
        let cases = [
            (
                json!({"protocol": "proton", "options": {"proton_mode": "native"}}),
                AuthSource::ProtonSession,
            ),
            (json!({"protocol": "proton"}), AuthSource::External),
            (
                json!({"protocol": "proton", "options": {"proton_mode": "cli"}}),
                AuthSource::External,
            ),
            (
                json!({"protocol": "mega", "options": {"mega_mode": "megacmd"}}),
                AuthSource::External,
            ),
            (json!({"protocol": "mega"}), AuthSource::Vault),
            (
                json!({"protocol": "ftp", "username": ""}),
                AuthSource::NoSecret,
            ),
            (
                json!({"protocol": "ftp", "username": "Anonymous"}),
                AuthSource::NoSecret,
            ),
            (
                json!({"protocol": "ftp", "username": "alice"}),
                AuthSource::Vault,
            ),
            (
                json!({"protocol": "webdav", "options": {"anonymous": true}}),
                AuthSource::NoSecret,
            ),
            (
                json!({"protocol": "webdav", "options": {"anonymous": false}}),
                AuthSource::Vault,
            ),
            (
                json!({"protocol": "sftp", "options": {"private_key_path": "/k"}}),
                AuthSource::NoSecret,
            ),
            (
                json!({"protocol": "sftp", "options": {"privateKeyPath": "/k"}}),
                AuthSource::NoSecret,
            ),
            (
                json!({"protocol": "sftp", "options": {"private_key_path": " "}}),
                AuthSource::Vault,
            ),
            (
                json!({"protocol": "webdav", "providerId": "filen-desktop-webdav", "username": "u"}),
                AuthSource::NoSecret,
            ),
            (
                json!({"protocol": "s3", "providerId": "filen-desktop-s3"}),
                AuthSource::NoSecret,
            ),
            (json!({"protocol": "mtp"}), AuthSource::NoSecret),
            (json!({"protocol": "local"}), AuthSource::NoSecret),
            (
                json!({"protocol": "peer", "options": {"peerTicket": "t"}}),
                AuthSource::NoSecret,
            ),
            (json!({"protocol": "peer"}), AuthSource::Vault),
            (json!({"protocol": "googledrive"}), AuthSource::Vault),
            (
                json!({"protocol": "s3", "username": "key"}),
                AuthSource::Vault,
            ),
        ];
        for (profile, want) in cases {
            assert_eq!(profile_auth_source(&profile), want, "{profile}");
            assert_eq!(AuthSource::parse(want.as_str()), Some(want));
        }
    }

    /// A profile that connects without `server_<id>` is not "not signed in":
    /// these all read `no_credentials` while they connected, so an agent
    /// skipped them.
    #[test]
    fn a_profile_that_signs_in_elsewhere_is_not_reported_unsigned() {
        let dir = tempfile::tempdir().unwrap();
        // Never opened: none of these sources decrypts anything.
        let store = CredentialStore::from_verified_key(&dir.path().join("vault.db"), &[7u8; 32]);
        let id = "srv_1";
        let state = |accounts: &HashSet<String>, protocol: &str, source: AuthSource| {
            derive_profile_auth_state(&store, accounts, id, protocol, source)
        };
        let none = HashSet::new();
        assert_eq!(state(&none, "ftp", AuthSource::NoSecret), "valid");
        assert_eq!(state(&none, "proton", AuthSource::External), "unknown");
        assert_eq!(state(&none, "mega", AuthSource::External), "unknown");
        assert_eq!(
            state(&none, "proton", AuthSource::ProtonSession),
            "no_credentials"
        );
        assert_eq!(state(&none, "ftp", AuthSource::Vault), "no_credentials");

        let own: HashSet<String> = ["proton_native_session_srv_1".to_string()].into();
        assert_eq!(state(&own, "proton", AuthSource::ProtonSession), "valid");
        // Signed in before the profile was saved: the session is bound to it
        // at its first connect.
        let unbound: HashSet<String> = ["proton_native_session".to_string()].into();
        assert_eq!(
            state(&unbound, "proton", AuthSource::ProtonSession),
            "valid"
        );
        // Another profile's session is not this one's.
        let other: HashSet<String> = ["proton_native_session_srv_2".to_string()].into();
        assert_eq!(
            state(&other, "proton", AuthSource::ProtonSession),
            "no_credentials"
        );

        let password: HashSet<String> = ["server_srv_1".to_string()].into();
        assert_eq!(state(&password, "mega", AuthSource::External), "valid");
        assert_eq!(state(&password, "ftp", AuthSource::Vault), "valid");
    }

    #[test]
    fn oauth_key_mapping_covers_documented_protocols() {
        // Documented OAuth providers must each map to a stable vault key.
        // If the OAuth2Manager enum gets a new variant, this test should
        // grow with it.
        assert_eq!(
            oauth_vault_key_for_protocol("googledrive"),
            Some("oauth_google")
        );
        assert_eq!(
            oauth_vault_key_for_protocol("googlephotos"),
            Some("oauth_googlephotos")
        );
        assert_eq!(
            oauth_vault_key_for_protocol("dropbox"),
            Some("oauth_dropbox")
        );
        assert_eq!(
            oauth_vault_key_for_protocol("onedrive"),
            Some("oauth_onedrive")
        );
        assert_eq!(oauth_vault_key_for_protocol("box"), Some("oauth_box"));
        assert_eq!(oauth_vault_key_for_protocol("pcloud"), Some("oauth_pcloud"));
        assert_eq!(
            oauth_vault_key_for_protocol("zohoworkdrive"),
            Some("oauth_zohoworkdrive")
        );
        assert_eq!(
            oauth_vault_key_for_protocol("yandexdisk"),
            Some("oauth_yandexdisk")
        );
        assert_eq!(
            oauth_vault_key_for_protocol("fourshared"),
            Some("oauth_fourshared")
        );
        assert_eq!(
            oauth_vault_key_for_protocol("jottacloud"),
            Some("jottacloud_refresh")
        );
    }

    #[test]
    fn jottacloud_auth_account_prefers_per_profile_key() {
        let id = "srv_1771799399856_swqija1mi";
        let per = format!("jottacloud_refresh_{id}");
        let mut accounts = HashSet::new();
        accounts.insert(per.clone());
        assert_eq!(
            auth_token_account("jottacloud", id, &accounts).as_deref(),
            Some(per.as_str())
        );
        accounts.insert("jottacloud_refresh".to_string());
        assert_eq!(
            auth_token_account("jottacloud", id, &accounts).as_deref(),
            Some(per.as_str()),
            "per-profile key wins over the legacy singleton"
        );
        accounts.remove(&per);
        assert_eq!(
            auth_token_account("jottacloud", id, &accounts).as_deref(),
            Some("jottacloud_refresh")
        );
        accounts.clear();
        assert_eq!(auth_token_account("jottacloud", id, &accounts), None);
    }

    #[test]
    fn oauth_key_mapping_is_case_insensitive() {
        assert_eq!(
            oauth_vault_key_for_protocol("GoogleDrive"),
            Some("oauth_google")
        );
        assert_eq!(
            oauth_vault_key_for_protocol("DROPBOX"),
            Some("oauth_dropbox")
        );
    }

    #[test]
    fn oauth_key_mapping_returns_none_for_password_protocols() {
        // Password-based protocols use `server_<id>` per profile and
        // have no protocol-level OAuth blob.
        for proto in [
            "ftp",
            "ftps",
            "sftp",
            "webdav",
            "s3",
            "azure",
            "filen",
            "internxt",
            "filelu",
            "koofr",
            "kdrive",
            "opendrive",
            "drime",
            "github",
            "mega",
            "swift",
            // Twake keeps its sign-in blob in the profile password.
            "twake",
        ] {
            assert_eq!(
                oauth_vault_key_for_protocol(proto),
                None,
                "{} should NOT map to an OAuth vault key",
                proto
            );
        }
    }

    /// The three maps that decide, for one provider, whether it has a stored
    /// auth blob, whether that blob travels with the profile, and whether the
    /// app behind it travels too, must agree provider by provider.
    ///
    /// They did not, twice, and both times the symptom was an export that
    /// reconnected nowhere: Zoho WorkDrive was missing from the client-cred map,
    /// so an imported profile could not refresh, and 4shared was missing from
    /// both the token map and the client-cred map, so nothing at all travelled.
    /// A provider added to one map and forgotten in the others now fails here
    /// instead of shipping.
    #[test]
    fn the_three_provider_maps_agree() {
        for proto in [
            "googledrive",
            "googlephotos",
            "dropbox",
            "onedrive",
            "box",
            "pcloud",
            "zohoworkdrive",
            "yandexdisk",
            "fourshared",
        ] {
            assert!(
                crate::oauth_vault_slug_for_protocol(proto).is_some(),
                "{proto} has an auth blob but its token would not travel with an export"
            );
            assert!(
                crate::bridge_commands::oauth_client_cred_key(proto).is_some(),
                "{proto} has an auth blob but the app that refreshes it would not travel"
            );
        }

        // Jottacloud is the one legitimate asymmetry: it authenticates with a
        // personal login token and has no OAuth app at all, so it carries a
        // refresh blob (handled by its own `jottacloud_refresh_<id>` key) and
        // must stay out of both OAuth maps rather than be "fixed" into them.
        assert_eq!(
            oauth_vault_key_for_protocol("jottacloud"),
            Some("jottacloud_refresh")
        );
        assert_eq!(crate::oauth_vault_slug_for_protocol("jottacloud"), None);
        assert_eq!(
            crate::bridge_commands::oauth_client_cred_key("jottacloud"),
            None
        );

        // Every provider whose app credentials travel must also have somewhere
        // to put the token, so the client-cred map can never grow a provider
        // the token map does not know about. Google Photos is the deliberate
        // many-to-one: it rides on Google Drive's app.
        for proto in ["googledrive", "googlephotos", "fourshared"] {
            assert!(
                crate::oauth_vault_slug_for_protocol(proto).is_some(),
                "{proto} carries app credentials but has no per-profile token key"
            );
        }
        assert_eq!(
            crate::bridge_commands::oauth_client_cred_key("googlephotos"),
            Some("googledrive")
        );

        // The rclone-facing view is a strict subset: it drops the providers
        // rclone has no backend for, and must never add one of its own.
        // Zoho WorkDrive used to live here on a false premise (rclone has had
        // a `zoho` backend for years); 4shared is the remaining exception.
        assert_eq!(
            crate::bridge_commands::rclone_oauth_client_cred_key("fourshared"),
            None,
            "rclone has no fourshared backend, so it must not be offered one"
        );
        assert_eq!(
            crate::bridge_commands::rclone_oauth_client_cred_key("zohoworkdrive"),
            Some("zohoworkdrive"),
            "rclone's zoho backend must be offered for WorkDrive profiles"
        );
    }
}
