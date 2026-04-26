// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Per-profile auth-readiness derivation from local vault state.
//!
//! Used by the CLI (`profiles --json`, `agent-bootstrap`, `agent-info`)
//! and by the MCP/agent tool surface (`aeroftp_list_servers`) so a single
//! source of truth answers "is this profile ready to connect right now,
//! or does it need user-side intervention before any operation will
//! succeed?". Pure local: never touches the network.

use crate::credential_store::CredentialStore;
use crate::providers::oauth2::StoredTokens;
use std::collections::HashSet;

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

/// Derive a profile's auth readiness from local vault state only — never
/// touches the network. Returns one of:
///   - `valid`           — credential present and (for OAuth) not expired
///   - `expired`         — OAuth token past `expires_at` and no refresh token
///   - `needs_refresh`   — OAuth token past `expires_at` but refresh token present
///   - `no_credentials`  — nothing stored; user has not signed in yet
///   - `unknown`         — vault entry present but value couldn't be parsed
///     (legacy/corrupt; treated as "agent should try anyway")
///
/// `accounts` is a pre-fetched set of vault keys to keep this O(1) per
/// profile when called in a loop. The store handle is used only to
/// decrypt OAuth blobs that need the `expires_at` check; password-based
/// protocols never trigger a decrypt.
pub fn derive_profile_auth_state(
    store: &CredentialStore,
    accounts: &HashSet<String>,
    profile_id: &str,
    protocol: &str,
) -> &'static str {
    let server_key = format!("server_{}", profile_id);
    let oauth_key = oauth_vault_key_for_protocol(protocol);

    let has_server = accounts.contains(&server_key);
    let has_oauth = oauth_key.is_some_and(|k| accounts.contains(k));

    if let Some(key) = oauth_key {
        if !has_oauth {
            return "no_credentials";
        }
        match store.get(key) {
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

/// Convenience: open the cached vault, snapshot the keyset once, and
/// derive auth state for a single profile. Returns `unknown` when the
/// vault isn't open (caller can't distinguish locked-vs-missing without
/// it, and "unknown" is the safe default that doesn't claim readiness).
pub fn auth_state_from_cache(profile_id: &str, protocol: &str) -> &'static str {
    let Some(store) = CredentialStore::from_cache() else {
        return "unknown";
    };
    let accounts: HashSet<String> = store.list_accounts().unwrap_or_default().into_iter().collect();
    derive_profile_auth_state(&store, &accounts, profile_id, protocol)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_key_mapping_covers_documented_protocols() {
        // Documented OAuth providers must each map to a stable vault key.
        // If the OAuth2Manager enum gets a new variant, this test should
        // grow with it.
        assert_eq!(oauth_vault_key_for_protocol("googledrive"), Some("oauth_google"));
        assert_eq!(oauth_vault_key_for_protocol("googlephotos"), Some("oauth_googlephotos"));
        assert_eq!(oauth_vault_key_for_protocol("dropbox"), Some("oauth_dropbox"));
        assert_eq!(oauth_vault_key_for_protocol("onedrive"), Some("oauth_onedrive"));
        assert_eq!(oauth_vault_key_for_protocol("box"), Some("oauth_box"));
        assert_eq!(oauth_vault_key_for_protocol("pcloud"), Some("oauth_pcloud"));
        assert_eq!(oauth_vault_key_for_protocol("zohoworkdrive"), Some("oauth_zohoworkdrive"));
        assert_eq!(oauth_vault_key_for_protocol("yandexdisk"), Some("oauth_yandexdisk"));
        assert_eq!(oauth_vault_key_for_protocol("fourshared"), Some("oauth_fourshared"));
        assert_eq!(oauth_vault_key_for_protocol("jottacloud"), Some("jottacloud_refresh"));
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
            "ftp", "ftps", "sftp", "webdav", "s3", "azure", "filen", "internxt", "filelu", "koofr",
            "kdrive", "opendrive", "drime", "github", "mega", "swift",
        ] {
            assert_eq!(
                oauth_vault_key_for_protocol(proto),
                None,
                "{} should NOT map to an OAuth vault key",
                proto
            );
        }
    }
}
