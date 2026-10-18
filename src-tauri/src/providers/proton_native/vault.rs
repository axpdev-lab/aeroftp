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
//! [`super::api::SessionStore`]). Profiles of one session (a duplicate shares
//! its original's) take one lease, keyed on the session, and a refresh writes
//! the new pair to every key that holds it, or the others keep a refresh
//! token Proton has already rotated away. Two copies of one session on two devices
//! (an export imported elsewhere) still rotate apart: the first to refresh
//! keeps the session and the other must sign in again.
//!
//! A sign-in made before its profile is saved (Quick Connect) has no profile
//! id yet: it goes under the unbound key `proton_native_session`, like the
//! legacy OAuth singleton, and the first connect of a saved profile without a
//! session of its own takes it over (`ProtonNativeProvider::connect` checks
//! the account first). The unbound key belongs to no profile, so it is not in
//! `per_profile_vault_keys` and is never exported.

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

/// Key of a session signed in before its profile was saved.
pub const UNBOUND_SESSION_KEY: &str = "proton_native_session";

/// `credential_type` of the partition mirror.
const PARTITION_TYPE: &str = "proton_native_session";

/// How long a profile delete waits for the revoke before it gives up.
const REVOKE_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a refresh, a sign-in, a sign-out or a delete waits for a
/// session's lease. Longer than a refresh can hold it (its request times out
/// after 30 s) and than a lease left by a crashed holder lives (30 s): one
/// not taken by then is held by something stuck, and a change of the key is
/// refused rather than made without it.
const EXCLUSIVE_WAIT: Duration = Duration::from_secs(45);

/// Serializes, inside this process, every look-and-write of a session key:
/// a refresh writes a key only if it still holds the session, and nothing
/// may change it between that look and that write. Across processes the
/// session's lease does the same. Held over synchronous code only.
static SESSION_WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn session_writes() -> std::sync::MutexGuard<'static, ()> {
    SESSION_WRITES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The vault key of the session saved for `profile_id`, or the unbound key.
pub fn session_key(profile_id: Option<&str>) -> String {
    match profile_id {
        Some(id) => format!("{SESSION_KEY_PREFIX}{id}"),
        None => UNBOUND_SESSION_KEY.to_string(),
    }
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
                concat!(
                    "the saved Proton session is unreadable: ",
                    crate::providers::proton_sign_in_hint!(),
                    " again"
                )
                .into(),
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

/// The session saved for `profile_id` (the unbound one for `None`), `None`
/// when there is none.
pub fn load_session(profile_id: Option<&str>) -> Result<Option<ProtonSession>, ProviderError> {
    read(&open_store()?, &session_key(profile_id))
}

/// Saves `session` for `profile_id` in the vault and, for a profile, in the
/// active user's partition (the unbound key stays vault-only, like the legacy
/// OAuth singleton).
pub fn store_session(
    profile_id: Option<&str>,
    session: &ProtonSession,
) -> Result<(), ProviderError> {
    let store = open_store()?;
    let key = session_key(profile_id);
    let json = Zeroizing::new(
        serde_json::to_string(&StoredSession::from(session))
            .map_err(|error| ProviderError::Other(format!("session encoding: {error}")))?,
    );
    store
        .store(&key, &json)
        .map_err(|error| ProviderError::Other(format!("the vault refused the session: {error}")))?;
    if profile_id.is_some() {
        crate::user_partitions::mirror_active_credential(&store, &key, PARTITION_TYPE, &json);
    }
    Ok(())
}

/// Removes the session saved for `profile_id` from the vault and the active
/// user's partition. Nothing saved is not an error.
pub fn delete_session(profile_id: Option<&str>) -> Result<(), ProviderError> {
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

/// The lease of `slug`, waited for up to [`EXCLUSIVE_WAIT`].
async fn acquire_exclusive(slug: &str) -> Option<RefreshLease> {
    let started = std::time::Instant::now();
    loop {
        if let Some(lease) = RefreshLease::acquire(slug).await {
            return Some(lease);
        }
        if started.elapsed() >= EXCLUSIVE_WAIT {
            return None;
        }
    }
}

/// Whether a change of a key may go ahead: where the key holds no session
/// there is nothing to collide with; where it does, only with that session's
/// lease, since the refresh that holds it would write the old session back
/// over the change.
fn exclusive<L>(holds_a_session: bool, lease: Option<L>) -> Result<Option<L>, ProviderError> {
    match (holds_a_session, lease) {
        (false, _) => Ok(None),
        (true, Some(lease)) => Ok(Some(lease)),
        (true, None) => Err(ProviderError::Other(
            "the Proton session is being refreshed elsewhere and did not let go; try again in a moment"
                .into(),
        )),
    }
}

/// The refresh lease of the session saved under `key`, for a sign-in, a
/// sign-out or a delete to hold while it changes the key (see [`exclusive`]).
async fn lease_of_saved(key: &str) -> Result<Option<RefreshLease>, ProviderError> {
    let store = open_store()?;
    let Some(saved) = read(&store, key).ok().flatten() else {
        return exclusive(false, None);
    };
    let lease = acquire_exclusive(&refresh_lease_slug(&saved.tokens.uid)).await;
    exclusive(true, lease)
}

/// Moves the unbound session to `profile_id`, the profile that just connected
/// with it.
pub async fn bind_unbound_session(
    profile_id: &str,
    session: &ProtonSession,
) -> Result<(), ProviderError> {
    let _lease = lease_of_saved(UNBOUND_SESSION_KEY).await?;
    let _writes = session_writes();
    store_session(Some(profile_id), session)?;
    delete_session(None)
}

/// Saves a new sign-in for `profile_id`, after ending on Proton the session
/// it replaces (unless another saved profile still uses that one).
pub async fn replace_session(
    profile_id: Option<&str>,
    session: &ProtonSession,
) -> Result<(), ProviderError> {
    let _lease = lease_of_saved(&session_key(profile_id)).await?;
    revoke_saved_session(profile_id).await;
    let _writes = session_writes();
    store_session(profile_id, session)
}

/// Resolve the session a status or explicit sign-out sees. Profile deletion
/// keeps using end_session directly and never consumes an unbound session.
pub fn saved_session_target(
    profile_id: Option<&str>,
) -> Result<(Option<&str>, bool), ProviderError> {
    resolve_session_target(profile_id, |id| {
        load_session(id).map(|session| session.is_some())
    })
}

fn resolve_session_target(
    profile_id: Option<&str>,
    mut has_session: impl FnMut(Option<&str>) -> Result<bool, ProviderError>,
) -> Result<(Option<&str>, bool), ProviderError> {
    if has_session(profile_id)? {
        return Ok((profile_id, true));
    }
    if profile_id.is_some() && has_session(None)? {
        return Ok((None, true));
    }
    Ok((profile_id, false))
}

/// End exactly the indicated vault entry, for sign-out or profile deletion.
pub async fn end_session(profile_id: Option<&str>) -> Result<(), ProviderError> {
    let _lease = lease_of_saved(&session_key(profile_id)).await?;
    revoke_saved_session(profile_id).await;
    let _writes = session_writes();
    delete_session(profile_id)
}

/// [`end_session`] for a synchronous caller (the CLI profile delete): it
/// runs on a thread of its own with its own runtime, so it works whether or
/// not the caller is inside a Tokio runtime.
pub fn end_session_blocking(profile_id: Option<&str>) {
    let profile_id = profile_id.map(str::to_string);
    let joined = std::thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => {
                if let Err(error) = runtime.block_on(end_session(profile_id.as_deref())) {
                    log::warn!("Proton: session not removed: {error}");
                }
            }
            Err(error) => log::warn!("Proton: session not ended, no runtime: {error}"),
        }
    })
    .join();
    if joined.is_err() {
        log::warn!("Proton: ending the session stopped unexpectedly");
    }
}

/// Revokes on Proton the session saved for `profile_id`, best effort, before
/// a profile delete (or a new sign-in) replaces the key. A session that
/// another key on this device also holds is left alive. Never fails the
/// caller: errors are logged.
pub async fn revoke_saved_session(profile_id: Option<&str>) {
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
        .filter(|account| {
            (account.starts_with(SESSION_KEY_PREFIX) || account == UNBOUND_SESSION_KEY)
                && *account != key
        })
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

/// The lease slug of a session's refresh: one per session UID, so the
/// profiles that share a session refresh it one at a time, across processes
/// too. Hashed, since it becomes a file name and a UID is not one.
fn refresh_lease_slug(uid: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(uid.as_bytes());
    format!("proton_native_{}", hex::encode(&digest[..12]))
}

/// The other copies of a session among `saved` (keys other than `own_key`
/// whose session has the UID of `tokens`), each with `tokens` in place of the
/// pair it holds and its own key password kept.
fn copies_to_update(
    own_key: &str,
    tokens: &SessionTokens,
    saved: impl IntoIterator<Item = (String, ProtonSession)>,
) -> Vec<(String, ProtonSession)> {
    saved
        .into_iter()
        .filter(|(key, session)| key != own_key && session.tokens.uid == tokens.uid)
        .map(|(key, session)| {
            let updated = ProtonSession {
                tokens: tokens.clone(),
                key_password: session.key_password,
            };
            (key, updated)
        })
        .collect()
}

/// The session keys as the sharing of a refresh sees them: the vault in the
/// app, a map in the tests.
trait SessionKeys {
    fn keys(&self) -> Vec<String>;
    fn read(&self, key: &str) -> Option<ProtonSession>;
    fn write(&self, key: &str, session: &ProtonSession) -> Result<(), ProviderError>;
}

struct VaultKeys(CredentialStore);

impl SessionKeys for VaultKeys {
    fn keys(&self) -> Vec<String> {
        self.0
            .list_accounts()
            .unwrap_or_default()
            .into_iter()
            .filter(|account| {
                account.starts_with(SESSION_KEY_PREFIX) || account == UNBOUND_SESSION_KEY
            })
            .collect()
    }

    fn read(&self, key: &str) -> Option<ProtonSession> {
        read(&self.0, key).ok().flatten()
    }

    fn write(&self, key: &str, session: &ProtonSession) -> Result<(), ProviderError> {
        store_session(key.strip_prefix(SESSION_KEY_PREFIX), session)
    }
}

/// Writes refreshed `tokens` to every other key that holds the same session.
/// Proton rotates the refresh token, so a duplicate left with the old one
/// could no longer refresh, even after the other's refresh was long done,
/// and ended with an expired session.
///
/// A key is written only if it still holds the session when it is written:
/// a sign-in, a sign-out or a delete of that profile since the look changed
/// it, and writing the looked-at copy would bring the old session back over
/// the new one. Those hold the session's lease while they change the key
/// (see [`lease_of_saved`]), and the refresh holds it too; inside a process
/// [`SESSION_WRITES`] keeps the look again and the write together. `between`
/// runs after the first look, for a test to change a key there.
fn share_with(
    keys: &dyn SessionKeys,
    own_key: &str,
    tokens: &SessionTokens,
    between: impl FnOnce(),
) {
    let saved = keys
        .keys()
        .into_iter()
        .filter_map(|key| keys.read(&key).map(|session| (key, session)))
        .collect::<Vec<_>>();
    between();
    for (key, session) in copies_to_update(own_key, tokens, saved) {
        let _writes = session_writes();
        let still_shared = keys
            .read(&key)
            .is_some_and(|now| now.tokens.uid == tokens.uid);
        if !still_shared {
            log::info!("Proton: a profile's session changed during a refresh and is left as it is");
            continue;
        }
        if let Err(error) = keys.write(&key, &session) {
            log::warn!("Proton: a profile sharing the session kept the old tokens: {error}");
        }
    }
}

/// Saves refreshed `tokens` under `own_key`, the key the session `uid` was
/// opened from, then in every other key that holds that session. The own key
/// too is written only if it still holds that session: a sign-in or a
/// sign-out made while the refresh was on its way changed it, and the old
/// session written back would undo it.
fn save_refreshed(
    keys: &dyn SessionKeys,
    own_key: &str,
    uid: &str,
    key_password: &secrecy::SecretString,
    tokens: &SessionTokens,
) {
    {
        let _writes = session_writes();
        let still_ours = keys.read(own_key).is_some_and(|now| now.tokens.uid == uid);
        if still_ours {
            let session = ProtonSession {
                tokens: tokens.clone(),
                key_password: key_password.clone(),
            };
            if let Err(error) = keys.write(own_key, &session) {
                // The session stays usable in this process; the next process
                // to start will find the old pair and sign in again.
                log::warn!("Proton: refreshed session not saved: {error}");
            }
        } else {
            log::info!(
                "Proton: the profile's session changed while it was refreshed; the refreshed tokens are not saved"
            );
        }
    }
    share_with(keys, own_key, tokens, || {});
}

/// The [`SessionStore`] of one vault key: refreshes are serialized across
/// processes and across the profiles of one session with the OAuth
/// [`RefreshLease`] keyed on the session, and the refreshed tokens are saved
/// with the unchanged key password, to this key and to every key that holds
/// the same session.
pub struct VaultSession {
    profile_id: Option<String>,
    uid: String,
    key_password: secrecy::SecretString,
}

impl VaultSession {
    pub fn new(profile_id: Option<&str>, session: &ProtonSession) -> Self {
        Self {
            profile_id: profile_id.map(str::to_string),
            uid: session.tokens.uid.clone(),
            key_password: session.key_password.clone(),
        }
    }
}

#[async_trait::async_trait]
impl SessionStore for VaultSession {
    async fn lock(&self) -> Option<Box<dyn Send>> {
        acquire_exclusive(&refresh_lease_slug(&self.uid))
            .await
            .map(|lease| Box::new(lease) as Box<dyn Send>)
    }

    fn load(&self) -> Option<SessionTokens> {
        match load_session(self.profile_id.as_deref()) {
            Ok(session) => session.map(|session| session.tokens),
            Err(error) => {
                log::warn!("Proton: saved session not readable: {error}");
                None
            }
        }
    }

    fn save(&self, tokens: &SessionTokens) {
        match open_store() {
            Ok(store) => save_refreshed(
                &VaultKeys(store),
                &session_key(self.profile_id.as_deref()),
                &self.uid,
                &self.key_password,
                tokens,
            ),
            Err(error) => log::warn!("Proton: refreshed session not saved: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::{ExposeSecret, SecretString};

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
    fn status_and_sign_out_choose_the_same_saved_session() {
        for (own, unbound, expected) in [
            (true, true, (Some("profile"), true)),
            (true, false, (Some("profile"), true)),
            (false, true, (None, true)),
            (false, false, (Some("profile"), false)),
        ] {
            let resolved = resolve_session_target(Some("profile"), |id| {
                Ok(if id.is_some() { own } else { unbound })
            })
            .unwrap();
            assert_eq!(resolved, expected);
        }
        assert_eq!(
            resolve_session_target(None, |_| Ok(true)).unwrap(),
            (None, true)
        );
        assert_eq!(
            resolve_session_target(None, |_| Ok(false)).unwrap(),
            (None, false)
        );
        let error = resolve_session_target(Some("profile"), |id| {
            assert!(
                id.is_some(),
                "an unreadable profile must not sign out another session"
            );
            Err(ProviderError::Other("unreadable synthetic session".into()))
        });
        assert!(error.is_err());
    }

    #[test]
    fn the_key_is_per_profile_and_matches_the_export_list() {
        assert_eq!(session_key(Some("srv_1")), "proton_native_session_srv_1");
        assert_eq!(session_key(None), UNBOUND_SESSION_KEY);
        assert!(!crate::per_profile_vault_keys("srv_1", Some("proton"))
            .contains(&UNBOUND_SESSION_KEY.to_string()));
        assert!(crate::per_profile_vault_keys("srv_1", Some("proton"))
            .contains(&session_key(Some("srv_1"))));
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

    /// Codex on #1293: a duplicate shares its original's session, but the
    /// refresh lease and the save were per profile, so a refresh of one left
    /// the other with a rotated refresh token and an expired session.
    #[test]
    fn a_refresh_reaches_every_profile_of_the_session() {
        assert_eq!(refresh_lease_slug("uid-1"), refresh_lease_slug("uid-1"));
        assert_ne!(refresh_lease_slug("uid-1"), refresh_lease_slug("uid-2"));
        assert!(refresh_lease_slug("a/b\\c")
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'));

        let mut duplicate = session("uid-1");
        duplicate.key_password = SecretString::from("duplicate key".to_string());
        let refreshed = SessionTokens {
            uid: "uid-1".into(),
            access_token: SecretString::from("a2".to_string()),
            refresh_token: Some(SecretString::from("r2".to_string())),
        };
        let own = session_key(Some("srv_a"));
        let updated = copies_to_update(
            &own,
            &refreshed,
            [
                (own.clone(), session("uid-1")),
                (session_key(Some("srv_b")), duplicate),
                (session_key(Some("srv_c")), session("uid-2")),
            ],
        );
        assert_eq!(updated.len(), 1, "only the other copy of the session");
        let (key, copy) = &updated[0];
        assert_eq!(key, &session_key(Some("srv_b")));
        assert_eq!(copy.tokens.access_token.expose_secret(), "a2");
        assert_eq!(
            copy.tokens
                .refresh_token
                .as_ref()
                .map(|r| r.expose_secret().to_string()),
            Some("r2".to_string())
        );
        assert_eq!(copy.key_password.expose_secret(), "duplicate key");
    }

    struct MapKeys(std::sync::Mutex<std::collections::HashMap<String, ProtonSession>>);

    impl SessionKeys for MapKeys {
        fn keys(&self) -> Vec<String> {
            self.0.lock().unwrap().keys().cloned().collect()
        }
        fn read(&self, key: &str) -> Option<ProtonSession> {
            self.0.lock().unwrap().get(key).map(|s| ProtonSession {
                tokens: s.tokens.clone(),
                key_password: s.key_password.clone(),
            })
        }
        fn write(&self, key: &str, session: &ProtonSession) -> Result<(), ProviderError> {
            self.0.lock().unwrap().insert(
                key.to_string(),
                ProtonSession {
                    tokens: session.tokens.clone(),
                    key_password: session.key_password.clone(),
                },
            );
            Ok(())
        }
    }

    fn refreshed(uid: &str) -> SessionTokens {
        SessionTokens {
            uid: uid.into(),
            access_token: SecretString::from("a2".to_string()),
            refresh_token: Some(SecretString::from("r2".to_string())),
        }
    }

    /// Codex on 2c8cfe3fc: the copies were looked at, then written whatever
    /// they held by then, so a new sign-in of the duplicate made in between
    /// was replaced by the old shared session.
    #[test]
    fn a_refresh_does_not_write_over_a_sign_in_made_since_its_look() {
        let a = session_key(Some("srv_a"));
        let b = session_key(Some("srv_b"));
        let keys = MapKeys(std::sync::Mutex::new(
            [(a.clone(), session("uid-1")), (b.clone(), session("uid-1"))]
                .into_iter()
                .collect(),
        ));
        share_with(&keys, &a, &refreshed("uid-1"), || {
            keys.write(&b, &session("uid-new")).unwrap();
        });
        assert_eq!(keys.read(&b).unwrap().tokens.uid, "uid-new");

        // Unchanged since the look, the copy takes the refreshed pair.
        let keys = MapKeys(std::sync::Mutex::new(
            [(a.clone(), session("uid-1")), (b.clone(), session("uid-1"))]
                .into_iter()
                .collect(),
        ));
        share_with(&keys, &a, &refreshed("uid-1"), || {});
        assert_eq!(
            keys.read(&b).unwrap().tokens.access_token.expose_secret(),
            "a2"
        );
    }

    /// Codex on 764193046: a sign-in waited 10 s for the lease of a slow
    /// refresh, went ahead without it, and the refresh then saved the old
    /// session over it. A key that holds a session changes only under its
    /// lease, and the refresh writes its own key back only if that key still
    /// holds the session it refreshed.
    #[test]
    fn a_session_key_changes_only_under_its_lease() {
        assert!(matches!(exclusive::<()>(false, None), Ok(None)));
        assert!(matches!(exclusive(true, Some(())), Ok(Some(()))));
        assert!(exclusive::<()>(true, None).is_err());
    }

    #[test]
    fn a_refresh_does_not_save_over_a_sign_in_of_its_own_profile() {
        let a = session_key(Some("srv_a"));
        let keys = MapKeys(std::sync::Mutex::new(
            [(a.clone(), session("uid-new"))].into_iter().collect(),
        ));
        save_refreshed(
            &keys,
            &a,
            "uid-1",
            &SecretString::from("k".to_string()),
            &refreshed("uid-1"),
        );
        assert_eq!(keys.read(&a).unwrap().tokens.uid, "uid-new");

        // Signed out meanwhile: the key is not brought back.
        let keys = MapKeys(std::sync::Mutex::new(std::collections::HashMap::new()));
        save_refreshed(
            &keys,
            &a,
            "uid-1",
            &SecretString::from("k".to_string()),
            &refreshed("uid-1"),
        );
        assert!(keys.read(&a).is_none());

        // Still its own session: saved.
        let keys = MapKeys(std::sync::Mutex::new(
            [(a.clone(), session("uid-1"))].into_iter().collect(),
        ));
        save_refreshed(
            &keys,
            &a,
            "uid-1",
            &SecretString::from("k".to_string()),
            &refreshed("uid-1"),
        );
        assert_eq!(
            keys.read(&a).unwrap().tokens.access_token.expose_secret(),
            "a2"
        );
    }

    #[test]
    fn an_unreadable_saved_session_asks_for_a_new_sign_in() {
        let error = parse("{not json").expect_err("refused");
        assert!(
            matches!(
                error,
                ProviderError::AuthenticationFailed(ref text)
                    if text.contains(crate::providers::PROTON_SIGN_IN_HINT)
            ),
            "{error:?}"
        );
        let ok = parse(r#"{"uid":"u","access_token":"a","key_password":"k"}"#).expect("parsed");
        assert_eq!(ok.tokens.uid, "u");
        assert!(ok.tokens.refresh_token.is_none());
    }
}
