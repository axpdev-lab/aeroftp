//! The signed-in session in AeroFTP's encrypted vault, one per saved profile.
//!
//! Key `proton_native_session_<profile_id>`, value the [`StoredSession`] JSON.
//! It is written at sign-in and after every token refresh, mirrored into the
//! active user's partition like the OAuth tokens (MUV-4), and listed in
//! `per_profile_vault_keys`, so a profile's delete removes it, its duplicate
//! copies it and its `.aeroftp` export carries it (owner decision
//! 2026-10-17, the house rule for every per-profile secret).
//!
//! Proton rotates the refresh token at every refresh. The desktop app, the
//! MCP server and the CLI can hold the same session, so the refresh runs
//! under the cross-process [`RefreshLease`] the OAuth providers use, and a
//! process that finds a newer pair in the vault adopts it (see
//! [`super::api::SessionStore`]). Two copies of one session on two devices
//! (an export imported elsewhere) still rotate apart: the first to refresh
//! keeps the session and the other must sign in again.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::time::Duration;

use zeroize::Zeroizing;

use super::api::{ProtonApi, SessionStore};
use super::session::{ProtonSession, SessionTokens, StoredSession};
use crate::credential_store::CredentialStore;
use crate::providers::oauth2::RefreshLease;
use crate::providers::ProviderError;

/// Prefix of every per-profile session key.
pub const SESSION_KEY_PREFIX: &str = "proton_native_session_";

/// `credential_type` of the partition mirror.
const PARTITION_TYPE: &str = "proton_native_session";

/// How long a profile delete waits for the revoke before it gives up.
const REVOKE_TIMEOUT: Duration = Duration::from_secs(15);

/// The vault key of the session saved for `profile_id`.
pub fn session_key(profile_id: &str) -> String {
    format!("{SESSION_KEY_PREFIX}{profile_id}")
}

/// The open vault, initialising it when it needs no master password.
fn open_store() -> Result<CredentialStore, ProviderError> {
    if let Some(store) = CredentialStore::from_cache() {
        return Ok(store);
    }
    if CredentialStore::init().is_ok() {
        if let Some(store) = CredentialStore::from_cache() {
            return Ok(store);
        }
    }
    Err(ProviderError::AuthenticationFailed(
        "AeroFTP's vault is locked: unlock it, then connect to Proton Drive again".into(),
    ))
}

fn parse(json: &str) -> Result<ProtonSession, ProviderError> {
    serde_json::from_str::<StoredSession>(json)
        .map(ProtonSession::from)
        .map_err(|_| {
            ProviderError::AuthenticationFailed(
                "the saved Proton session is unreadable; sign in to Proton again".into(),
            )
        })
}

fn read(store: &CredentialStore, key: &str) -> Result<Option<ProtonSession>, ProviderError> {
    match crate::user_partitions::resolve_active_credential(store, key) {
        Ok(Some(json)) => parse(&json).map(Some),
        Ok(None) => Ok(None),
        Err(error) => Err(ProviderError::Other(format!(
            "the saved Proton session could not be read: {error}"
        ))),
    }
}

/// The session saved for `profile_id`, `None` when it never signed in.
pub fn load_session(profile_id: &str) -> Result<Option<ProtonSession>, ProviderError> {
    read(&open_store()?, &session_key(profile_id))
}

/// Saves `session` for `profile_id` in the vault and in the active user's
/// partition.
pub fn store_session(profile_id: &str, session: &ProtonSession) -> Result<(), ProviderError> {
    let store = open_store()?;
    let key = session_key(profile_id);
    let json = Zeroizing::new(
        serde_json::to_string(&StoredSession::from(session))
            .map_err(|error| ProviderError::Other(format!("session encoding: {error}")))?,
    );
    store
        .store(&key, &json)
        .map_err(|error| ProviderError::Other(format!("the vault refused the session: {error}")))?;
    crate::user_partitions::mirror_active_credential(&store, &key, PARTITION_TYPE, &json);
    Ok(())
}

/// Removes the session saved for `profile_id` from the vault and the active
/// user's partition. Nothing saved is not an error.
pub fn delete_session(profile_id: &str) -> Result<(), ProviderError> {
    let store = open_store()?;
    let key = session_key(profile_id);
    store
        .delete(&key)
        .map_err(|error| ProviderError::Other(format!("the vault refused the delete: {error}")))?;
    crate::user_partitions::unmirror_active_credential(&key);
    Ok(())
}

/// True when another key among `others` holds a session with `uid`: a
/// duplicated profile shares its original's session, and revoking it for
/// one would sign the other out.
fn session_shared(uid: &str, others: impl IntoIterator<Item = ProtonSession>) -> bool {
    others.into_iter().any(|other| other.tokens.uid == uid)
}

/// Revokes on Proton the session saved for `profile_id`, best effort, before
/// a profile delete removes the key. A session that another saved profile on
/// this device also holds is left alive. Never fails the delete: errors are
/// logged.
pub async fn revoke_saved_session(profile_id: &str) {
    let store = match open_store() {
        Ok(store) => store,
        Err(_) => return,
    };
    let key = session_key(profile_id);
    let session = match read(&store, &key) {
        Ok(Some(session)) => session,
        Ok(None) => return,
        Err(error) => {
            log::warn!("Proton: saved session not revoked: {error}");
            return;
        }
    };
    let others = store
        .list_accounts()
        .unwrap_or_default()
        .into_iter()
        .filter(|account| account.starts_with(SESSION_KEY_PREFIX) && *account != key)
        .filter_map(|account| read(&store, &account).ok().flatten());
    if session_shared(&session.tokens.uid, others) {
        log::info!("Proton: session kept, another saved profile uses it");
        return;
    }
    let api = ProtonApi::new(super::app_version::app_version());
    api.set_tokens(Some(session.tokens)).await;
    match tokio::time::timeout(REVOKE_TIMEOUT, super::auth::sign_out(&api)).await {
        Ok(Ok(())) => log::info!("Proton: session revoked with its profile"),
        Ok(Err(error)) => log::warn!("Proton: session revoke failed: {error}"),
        Err(_) => log::warn!("Proton: session revoke timed out"),
    }
}

/// [`revoke_saved_session`] for a synchronous caller (the CLI profile delete):
/// it runs on a thread of its own with its own runtime, so it works whether
/// or not the caller is inside a Tokio runtime, and returns when the revoke
/// is done or has timed out.
pub fn revoke_saved_session_blocking(profile_id: &str) {
    let profile_id = profile_id.to_string();
    let joined = std::thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(revoke_saved_session(&profile_id)),
            Err(error) => log::warn!("Proton: session not revoked, no runtime: {error}"),
        }
    })
    .join();
    if joined.is_err() {
        log::warn!("Proton: the session revoke stopped unexpectedly");
    }
}

/// The [`SessionStore`] of one saved profile: refreshes are serialized
/// across processes with the OAuth [`RefreshLease`] keyed on the session's
/// vault key, and the refreshed tokens are saved with the unchanged key
/// password.
pub struct VaultSession {
    profile_id: String,
    key_password: secrecy::SecretString,
}

impl VaultSession {
    pub fn new(profile_id: &str, session: &ProtonSession) -> Self {
        Self {
            profile_id: profile_id.to_string(),
            key_password: session.key_password.clone(),
        }
    }
}

#[async_trait::async_trait]
impl SessionStore for VaultSession {
    async fn lock(&self) -> Option<Box<dyn Send>> {
        RefreshLease::acquire(&session_key(&self.profile_id))
            .await
            .map(|lease| Box::new(lease) as Box<dyn Send>)
    }

    fn load(&self) -> Option<SessionTokens> {
        match load_session(&self.profile_id) {
            Ok(session) => session.map(|session| session.tokens),
            Err(error) => {
                log::warn!("Proton: saved session not readable: {error}");
                None
            }
        }
    }

    fn save(&self, tokens: &SessionTokens) {
        let session = ProtonSession {
            tokens: tokens.clone(),
            key_password: self.key_password.clone(),
        };
        if let Err(error) = store_session(&self.profile_id, &session) {
            // The session stays usable in this process; the next process to
            // start will find the old pair and sign in again.
            log::warn!("Proton: refreshed session not saved: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    fn session(uid: &str) -> ProtonSession {
        ProtonSession {
            tokens: SessionTokens {
                uid: uid.into(),
                access_token: SecretString::from("a".to_string()),
                refresh_token: Some(SecretString::from("r".to_string())),
            },
            key_password: SecretString::from("k".to_string()),
        }
    }

    #[test]
    fn the_key_is_per_profile_and_matches_the_export_list() {
        assert_eq!(session_key("srv_1"), "proton_native_session_srv_1");
        assert!(
            crate::per_profile_vault_keys("srv_1", Some("proton")).contains(&session_key("srv_1"))
        );
    }

    #[test]
    fn a_session_held_by_another_profile_is_not_revoked() {
        assert!(session_shared(
            "uid-1",
            [session("uid-2"), session("uid-1")]
        ));
        assert!(!session_shared("uid-1", [session("uid-2")]));
        assert!(!session_shared("uid-1", []));
    }

    #[test]
    fn an_unreadable_saved_session_asks_for_a_new_sign_in() {
        let error = parse("{not json").expect_err("refused");
        assert!(
            matches!(error, ProviderError::AuthenticationFailed(ref text) if text.contains("sign in")),
            "{error:?}"
        );
        let ok = parse(r#"{"uid":"u","access_token":"a","key_password":"k"}"#).expect("parsed");
        assert_eq!(ok.tokens.uid, "u");
        assert!(ok.tokens.refresh_token.is_none());
    }
}
