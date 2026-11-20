//! The signed-in session in AeroFTP's encrypted vault, one per saved profile.
//!
//! Key `proton_native_session_<profile_id>`, value the [`StoredSession`] JSON.
//! It is written at sign-in and after every token refresh, mirrored into the
//! captured user's partition like the OAuth tokens (MUV-4), and listed in
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
//! id yet: it goes under `proton_native_unbound_user_<user_id>`, and the
//! first connect of a saved profile without a
//! session of its own takes it over (`ProtonNativeProvider::connect` checks
//! the account first). The unbound key belongs to no profile, so it is not in
//! `per_profile_vault_keys` and is never exported. The legacy unbound key
//! `proton_native_session` remains readable by its original account only.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::time::Duration;

use zeroize::Zeroizing;

use super::api::{ProtonApi, SessionStore};
use super::session::{ProtonSession, SessionTokens, StoredSession};
use crate::credential_store::{CredentialError, CredentialStore};
use crate::providers::oauth2::RefreshLease;
use crate::providers::ProviderError;

/// Prefix of every per-profile session key.
pub const SESSION_KEY_PREFIX: &str = "proton_native_session_";

/// Key of a session signed in before its profile was saved.
pub const UNBOUND_SESSION_KEY: &str = "proton_native_session";

/// `credential_type` of the partition mirror.
const PARTITION_TYPE: &str = "proton_native_session";
const USER_UNBOUND_PREFIX: &str = "proton_native_unbound_user_";

/// Immutable account ownership. `None` is the pre-partition vault, never an
/// instruction to look up the active account again after an await or switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionOwner {
    user_id: Option<i64>,
}

impl SessionOwner {
    pub const fn for_user(user_id: Option<i64>) -> Self {
        Self { user_id }
    }

    pub fn current() -> Result<Self, ProviderError> {
        let store = open_store()?;
        let user =
            crate::user_partitions::cli_get_active_user(&store).map_err(ProviderError::Other)?;
        Ok(Self::for_user(user.map(|user| user.id)))
    }

    fn key(self, profile_id: Option<&str>) -> String {
        match (profile_id, self.user_id) {
            (None, Some(user)) => format!("{USER_UNBOUND_PREFIX}{user}"),
            _ => session_key(profile_id),
        }
    }
}

/// How long a profile delete waits for the revoke before it gives up.
const REVOKE_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a refresh, a sign-in, a sign-out or a delete waits for a
/// session's lease. Longer than a refresh can hold it (its request times out
/// after 30 s) and than a lease left by a crashed holder lives (30 s): one
/// not taken by then is held by something stuck, and a change of the key is
/// refused rather than made without it.
const EXCLUSIVE_WAIT: Duration = Duration::from_secs(45);

fn exclusive_wait() -> Duration {
    #[cfg(test)]
    if let Some(wait) = test_seams::exclusive_wait() {
        return wait;
    }
    EXCLUSIVE_WAIT
}

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

/// What the tests observe of this module from outside it.
#[cfg(test)]
pub(crate) mod test_seams {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;

    static REVOKES: AtomicUsize = AtomicUsize::new(0);
    static WAITS: Mutex<Option<mpsc::Sender<String>>> = Mutex::new(None);
    static EXCLUSIVE_WAIT: Mutex<Option<Duration>> = Mutex::new(None);

    /// Every wait for a session's key lock (`key <key>`) or refresh lease
    /// (`lease <slug>`) from now on, reported as it begins: a test waits for
    /// the contention it set up, not for time to pass.
    pub(crate) fn observe_waits() -> mpsc::Receiver<String> {
        let (sender, receiver) = mpsc::channel();
        *WAITS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sender);
        receiver
    }

    /// `future`, reported to [`observe_waits`] as `what` when a poll finds it
    /// waiting for the first time.
    pub(super) async fn reported<F: std::future::Future>(what: String, future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut what = Some(what);
        std::future::poll_fn(|context| {
            let poll = future.as_mut().poll(context);
            if poll.is_pending() {
                if let (Some(what), Some(waits)) = (
                    what.take(),
                    WAITS
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .as_ref(),
                ) {
                    let _ = waits.send(what);
                }
            }
            poll
        })
        .await
    }

    /// How long a change of a session waits for its key lock and its lease,
    /// in place of [`super::EXCLUSIVE_WAIT`]; `None` restores that.
    pub(crate) fn set_exclusive_wait(wait: Option<Duration>) {
        *EXCLUSIVE_WAIT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = wait;
    }

    pub(super) fn exclusive_wait() -> Option<Duration> {
        *EXCLUSIVE_WAIT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    type RefreshWriteHook = Box<dyn FnOnce(&str) + Send>;
    static BEFORE_REFRESH_WRITE: Mutex<Option<RefreshWriteHook>> = Mutex::new(None);

    /// Runs `hook` once, with the key, just before the next write of a
    /// refreshed pair: another process's change between the refresh's look
    /// and its write.
    pub(crate) fn before_next_refresh_write(hook: impl FnOnce(&str) + Send + 'static) {
        *BEFORE_REFRESH_WRITE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Box::new(hook));
    }

    pub(super) fn refresh_write(key: &str) {
        let hook = BEFORE_REFRESH_WRITE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(hook) = hook {
            hook(key);
        }
    }

    static MIRROR_REFUSED: AtomicBool = AtomicBool::new(false);

    /// The next save of a session's partition row is refused, as an account
    /// store that fails does, on any thread.
    pub(crate) fn refuse_next_mirror() {
        MIRROR_REFUSED.store(true, Ordering::SeqCst);
    }

    pub(super) fn mirror_refused() -> bool {
        MIRROR_REFUSED.swap(false, Ordering::SeqCst)
    }

    static NO_FILE_LOCKS: AtomicBool = AtomicBool::new(false);

    /// Plays a filesystem without file locks for every session key lock, on
    /// any thread, until called again with `false`.
    pub(crate) fn set_no_file_locks(none: bool) {
        NO_FILE_LOCKS.store(none, Ordering::SeqCst);
    }

    pub(super) fn no_file_locks() -> bool {
        NO_FILE_LOCKS.load(Ordering::SeqCst)
    }

    /// How many sign-outs on Proton this process has made.
    pub(crate) fn revokes() -> usize {
        REVOKES.load(Ordering::SeqCst)
    }

    static FAIL_REVOKES: AtomicBool = AtomicBool::new(false);
    static FAILED_REVOKES: AtomicUsize = AtomicUsize::new(0);

    /// Every sign-out on Proton from now on fails, as one sent offline does,
    /// until called again with `false`.
    pub(crate) fn fail_revokes(fail: bool) {
        FAIL_REVOKES.store(fail, Ordering::SeqCst);
    }

    /// How many sign-outs on Proton failed in this process.
    pub(crate) fn failed_revokes() -> usize {
        FAILED_REVOKES.load(Ordering::SeqCst)
    }

    /// Counts a sign-out on Proton in place of sending it, so a test never
    /// reaches Proton's host: done, or failed while [`fail_revokes`] is on.
    /// Always `Some`.
    pub(super) fn revoke_counted() -> Option<super::RevokeOutcome> {
        if FAIL_REVOKES.load(Ordering::SeqCst) {
            FAILED_REVOKES.fetch_add(1, Ordering::SeqCst);
            return Some(super::RevokeOutcome::Failed(
                "the sign-out on Proton failed".into(),
            ));
        }
        REVOKES.fetch_add(1, Ordering::SeqCst);
        Some(super::RevokeOutcome::Revoked)
    }
}

/// A non-expiring OS lock for a vault key, including a key not created yet.
/// Keep the file after unlocking: unlinking it could let a second inode be
/// locked while another process still holds the first one. A crash releases
/// the OS lock, so no stale timeout or lock-file stealing is needed.
struct KeyLease(std::fs::File);

impl Drop for KeyLease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

async fn lock_key_file(path: &std::path::Path, wait: Duration) -> Result<KeyLease, ProviderError> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options
        .open(path)
        .map_err(|error| ProviderError::Other(format!("Proton session lock: {error}")))?;
    let try_lock = || {
        #[cfg(test)]
        if test_seams::no_file_locks() {
            return Err(std::fs::TryLockError::Error(std::io::Error::from(
                std::io::ErrorKind::Unsupported,
            )));
        }
        file.try_lock()
    };
    let deadline = std::time::Instant::now() + wait;
    loop {
        match try_lock() {
            Ok(()) => return Ok(KeyLease(file)),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(ProviderError::Other(
                    "The Proton session is being changed elsewhere; try again in a moment".into(),
                ))
            }
            // Without file locks nothing keeps another process off the
            // session while it changes: refused, with the cause named.
            Err(std::fs::TryLockError::Error(error))
                if crate::user_partitions::file_locks_unavailable(&error) =>
            {
                return Err(ProviderError::Other(
                    crate::user_partitions::NO_FILE_LOCKS.into(),
                ))
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(ProviderError::Other(format!(
                    "Proton session lock: {error}"
                )))
            }
        }
    }
}

fn mutation_keys(
    owner: SessionOwner,
    profile_id: Option<&str>,
) -> Result<Vec<String>, ProviderError> {
    let mut keys = vec![owner.key(profile_id)];
    // Guard both names during migration. Resolve the actual source only
    // after locking, since a competing sign-in may migrate it while we wait.
    if profile_id.is_none()
        && owner.user_id.is_some()
        && crate::user_partitions::legacy_credential_owner_id().map_err(ProviderError::Other)?
            == owner.user_id
    {
        keys.push(UNBOUND_SESSION_KEY.into());
    }
    Ok(keys)
}

async fn lock_keys(mut keys: Vec<String>) -> Result<Vec<KeyLease>, ProviderError> {
    use sha2::{Digest, Sha256};
    let directory = crate::portable::aeroftp_data_root()
        .ok_or_else(|| ProviderError::Other("Could not find AeroFTP data root".into()))?
        .join("proton_session_locks");
    std::fs::create_dir_all(&directory)
        .map_err(|error| ProviderError::Other(format!("Proton session lock directory: {error}")))?;
    keys.sort_unstable();
    keys.dedup();
    let mut leases = Vec::with_capacity(keys.len());
    // All mutation paths take sorted key locks first, then the session UID
    // lease. Refreshes take only the UID lease, so neither order can invert.
    for key in keys {
        let path = directory.join(format!(
            "{}.lock",
            hex::encode(Sha256::digest(key.as_bytes()))
        ));
        let lease = lock_key_file(&path, exclusive_wait());
        #[cfg(test)]
        let lease = test_seams::reported(format!("key {key}"), lease);
        leases.push(lease.await?);
    }
    Ok(leases)
}

/// The vault key of the session saved for `profile_id`, or the unbound key.
pub fn session_key(profile_id: Option<&str>) -> String {
    match profile_id {
        Some(id) => format!("{SESSION_KEY_PREFIX}{id}"),
        None => UNBOUND_SESSION_KEY.to_string(),
    }
}

/// The account whose sessions live in its own partition alone: anyone but
/// the legacy owner, whose vault keys these names are. `None` for the legacy
/// owner and for the pre-partition vault, which keep the vault copy.
fn private_account(owner: SessionOwner) -> Result<Option<i64>, ProviderError> {
    match owner.user_id {
        None => Ok(None),
        Some(user) => crate::user_partitions::private_token_user(Some(user), PARTITION_TYPE)
            .map_err(ProviderError::Other),
    }
}

/// Whether a session is saved for `profile_id` of `owner`, seen by its key
/// and nothing of an account decrypted: the account's row, which a locked
/// account has too, or else the vault copy when it is one the account reads.
pub fn holds_saved_session(
    store: &CredentialStore,
    owner: SessionOwner,
    profile_id: &str,
) -> Result<bool, ProviderError> {
    let key = session_key(Some(profile_id));
    match owner.user_id {
        Some(user) => crate::user_partitions::holds_credential(store, user, &key)
            .map_err(ProviderError::Other),
        None => Ok(store.get(&key).is_ok_and(|value| !value.is_empty())),
    }
}

/// Removes `key` of `owner`: a private account's row, its only copy, or the
/// vault entry and the owner's mirror of it.
fn remove_saved(
    store: &CredentialStore,
    owner: SessionOwner,
    key: &str,
) -> Result<(), ProviderError> {
    if let Some(user) = private_account(owner)? {
        let conn = crate::user_partitions::open_or_init_cli().map_err(ProviderError::Other)?;
        return crate::user_partitions::delete_user_credential_for(&conn, user, key)
            .map_err(ProviderError::Other);
    }
    store
        .delete(key)
        .map_err(|error| ProviderError::Other(format!("the vault refused the delete: {error}")))?;
    if let Some(user) = owner.user_id {
        crate::user_partitions::unmirror_credential_for_user_best_effort_any(user, key);
    }
    Ok(())
}

/// Every session `owner` holds, by key: a private account's own rows, or the
/// vault's keys. The copies of one session a sign-out, a restore or a refresh
/// must see are among them, and never another account's.
fn saved_sessions(
    store: &CredentialStore,
    owner: SessionOwner,
) -> Result<Vec<(String, ProtonSession)>, ProviderError> {
    if let Some(user) = private_account(owner)? {
        let rows = crate::user_partitions::user_credentials_of_type(store, user, PARTITION_TYPE)
            .map_err(ProviderError::Other)?;
        return Ok(rows
            .into_iter()
            .filter_map(|(key, raw)| parse(&raw).ok().map(|session| (key, session)))
            .collect());
    }
    vault_sessions(|| store.list_accounts(), |key| store.get(key))
}

/// The sessions among the vault's keys. A list or a read the vault refuses is
/// an error, never a missing copy: a sign-out that missed a copy would revoke
/// the session it holds. A key gone since the list is no copy, and neither is
/// a value that is no session.
fn vault_sessions(
    list: impl FnOnce() -> Result<Vec<String>, CredentialError>,
    get: impl Fn(&str) -> Result<String, CredentialError>,
) -> Result<Vec<(String, ProtonSession)>, ProviderError> {
    let mut keys = list().map_err(|error| {
        ProviderError::Other(format!(
            "the vault could not list the saved sessions: {error}"
        ))
    })?;
    keys.sort_unstable();
    let mut sessions = Vec::new();
    for key in keys.into_iter().filter(|key| is_session_key(key)) {
        let raw = match get(&key) {
            Ok(raw) => raw,
            Err(CredentialError::NotFound(_)) => continue,
            Err(error) => {
                return Err(ProviderError::Other(format!(
                    "the vault could not read a saved session: {error}"
                )))
            }
        };
        if let Ok(session) = parse(&raw) {
            sessions.push((key, session));
        }
    }
    Ok(sessions)
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

fn read(
    store: &CredentialStore,
    owner: SessionOwner,
    key: &str,
) -> Result<Option<ProtonSession>, ProviderError> {
    let result = match owner.user_id {
        Some(user) => {
            crate::user_partitions::resolve_credential_for_user_owner_aware(store, user, key)
        }
        None => Ok(store.get(key).ok().map(Zeroizing::new)),
    };
    match result {
        Ok(Some(json)) => {
            let mut session = parse(&json)?;
            // A locked shared copy may not have accepted its mirror. The vault
            // is written first, so its newer pair for this SAME UID is
            // authoritative. The UID match confines this to the session the
            // caller's own partition already holds. A private account's
            // session is its row alone: the vault key is the legacy owner's.
            let shares_the_vault = private_account(owner)?.is_none();
            if let Some(raw) = shares_the_vault.then(|| store.get(key).ok()).flatten() {
                if let Ok(saved) = parse(&raw) {
                    if saved.tokens.uid == session.tokens.uid {
                        session.tokens = saved.tokens;
                    }
                }
            }
            Ok(Some(session))
        }
        Ok(None) => Ok(None),
        Err(error) => Err(ProviderError::Other(format!(
            "the saved Proton session could not be read: {error}"
        ))),
    }
}

/// The session saved for `profile_id` (the unbound one for `None`), `None`
/// when there is none.
pub fn load_session(
    owner: SessionOwner,
    profile_id: Option<&str>,
) -> Result<Option<ProtonSession>, ProviderError> {
    let store = open_store()?;
    read(&store, owner, &saved_key(&store, owner, profile_id)?)
}

fn saved_key(
    store: &CredentialStore,
    owner: SessionOwner,
    profile_id: Option<&str>,
) -> Result<String, ProviderError> {
    let own = owner.key(profile_id);
    if profile_id.is_some() || owner.user_id.is_none() || read(store, owner, &own)?.is_some() {
        return Ok(own);
    }
    // The old global unbound entry belongs only to the legacy account.
    if crate::user_partitions::legacy_credential_owner_id().map_err(ProviderError::Other)?
        == owner.user_id
    {
        return Ok(UNBOUND_SESSION_KEY.into());
    }
    Ok(own)
}

/// Saves `session` in the vault and the captured user's partition. Unbound
/// keys include the user id so concurrent sign-ins by different users stay
/// isolated. `None` ownership is the pre-partition legacy vault. Whole or
/// not at all: a row the account store refuses puts the vault key back as it
/// was ([`undo_vault_write`]), so a save that failed is never the session the
/// profile connects with.
pub fn store_session(
    owner: SessionOwner,
    profile_id: Option<&str>,
    session: &ProtonSession,
) -> Result<(), ProviderError> {
    let store = open_store()?;
    let key = owner.key(profile_id);
    let json = Zeroizing::new(
        serde_json::to_string(&StoredSession::from(session))
            .map_err(|error| ProviderError::Other(format!("session encoding: {error}")))?,
    );
    if let Some(user) = private_account(owner)? {
        crate::user_partitions::store_credential_for_user(
            &store,
            user,
            &key,
            PARTITION_TYPE,
            &json,
        )
        .map_err(ProviderError::Other)?;
        indexed(&store, user, &key);
        return Ok(());
    }
    let previous = store.get(&key).ok().map(Zeroizing::new);
    store
        .store(&key, &json)
        .map_err(|error| ProviderError::Other(format!("the vault refused the session: {error}")))?;
    if let Some(user) = owner.user_id {
        if let Err(error) = mirror_session(&store, user, &key, &json) {
            return Err(undo_vault_write(
                &store,
                &key,
                &json,
                previous.as_deref().map(String::as_str),
                &error,
            ));
        }
        indexed(&store, user, &key);
    }
    Ok(())
}

/// The legacy owner's row of the session `json` just written to the vault.
fn mirror_session(store: &CredentialStore, user: i64, key: &str, json: &str) -> Result<(), String> {
    #[cfg(test)]
    if test_seams::mirror_refused() {
        return Err("the account store refused the session".into());
    }
    crate::user_partitions::mirror_credential_for_user_typed(store, user, key, PARTITION_TYPE, json)
}

/// The vault key `key` put back as it was before `written` replaced it, once
/// the row failed with `error`: the previous session, or no key when there was
/// none. Every caller holds the key's lock and the sessions' leases, so no
/// other writer is undone; a value changed since is left as it is anyway. The
/// error says whether the vault was put back.
fn undo_vault_write(
    store: &CredentialStore,
    key: &str,
    written: &str,
    previous: Option<&str>,
    error: &str,
) -> ProviderError {
    let undone = match previous {
        Some(previous) => store
            .replace_secret_if_unchanged(key, written, previous)
            .map(|_| ()),
        None => store.delete(key),
    };
    match undone {
        Ok(()) => ProviderError::Other(format!(
            "the session was not saved, the account store refused it: {error}"
        )),
        Err(undo) => ProviderError::Other(format!(
            "the session reached the vault but not the account store, and the vault could not be put back ({undo}): {error}"
        )),
    }
}

/// Indexes the row `key` of `user` just written (see
/// [`crate::user_partitions::other_accounts_holding_session`]). A row left
/// out of the index keeps every other account's sign-out from revoking it,
/// until the account's next unlock indexes it: the save itself stands.
fn indexed(store: &CredentialStore, user: i64, key: &str) {
    if let Err(error) = crate::user_partitions::index_proton_session(store, user, key) {
        log::warn!("Proton: the saved session was not indexed: {error}");
    }
}

/// Removes the session saved for `profile_id` from the vault and the captured
/// user's partition. Nothing saved is not an error.
pub fn delete_session(owner: SessionOwner, profile_id: Option<&str>) -> Result<(), ProviderError> {
    let store = open_store()?;
    let key = saved_key(&store, owner, profile_id)?;
    remove_saved(&store, owner, &key)
}

/// [`remove_saved`] for a rollback or a Move: every half is attempted and
/// every failure returned, because the caller must name a session it could
/// not remove. A private account removes its own row only; the vault key of
/// the same name is the legacy owner's.
fn delete_discarded_session(owner: SessionOwner, profile_id: &str) -> Result<(), ProviderError> {
    let store = open_store()?;
    let key = saved_key(&store, owner, Some(profile_id))?;
    if let Some(user) = private_account(owner)? {
        return crate::user_partitions::delete_partition_credential_checked(user, &key)
            .map_err(ProviderError::Other);
    }
    let mut errors = Vec::new();
    if let Err(error) = store.delete(&key) {
        errors.push(format!("the vault refused the delete: {error}"));
    }
    if let Some(user) = owner.user_id {
        if let Err(error) = crate::user_partitions::delete_partition_credential_checked(user, &key)
        {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ProviderError::Other(errors.join("; ")))
    }
}

/// Remove a profile's saved session locally, without revoking it: a failed
/// local duplicate, whose source shares the remote session. The key lock and
/// the session's refresh lease are held, so a refresh in flight saves its pair
/// before the removal, never after.
pub fn discard_session_blocking(
    owner: SessionOwner,
    profile_id: &str,
) -> Result<(), ProviderError> {
    hold_session_blocking(owner, profile_id)?.discard()
}

/// The key lock of a profile's session and, when the key holds a session, its
/// refresh lease. A Move takes them before it looks whether the source holds
/// a session and keeps them until it has removed it: a sign-in, a restore or
/// a refresh of the profile in between waits for the Move, and a lease it
/// cannot take stops the Move before anything changed.
pub struct HeldSession {
    owner: SessionOwner,
    profile_id: String,
    _keys: Vec<KeyLease>,
    _lease: Option<RefreshLease>,
}

/// Takes the [`HeldSession`] of `profile_id`, on a thread and a runtime of its
/// own, so a synchronous caller can hold it across its own transaction. The
/// caller must not hold the account's session: a refresh in flight may need
/// it to save its pair while it holds the lease this waits for.
pub fn hold_session_blocking(
    owner: SessionOwner,
    profile_id: &str,
) -> Result<HeldSession, ProviderError> {
    let profile_id = profile_id.to_string();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| ProviderError::Other(format!("Proton session runtime: {error}")))?;
        runtime.block_on(async {
            let store = open_store()?;
            let keys = lock_keys(mutation_keys(owner, Some(&profile_id))?).await?;
            let key = saved_key(&store, owner, Some(&profile_id))?;
            let lease = lease_of_saved(owner, &key).await?;
            Ok(HeldSession {
                owner,
                profile_id,
                _keys: keys,
                _lease: lease,
            })
        })
    })
    .join()
    .map_err(|_| ProviderError::Other("Proton session hold stopped unexpectedly".into()))?
}

impl HeldSession {
    /// Removes the held session locally, without a sign-out on Proton (see
    /// [`delete_discarded_session`]).
    pub fn discard(&self) -> Result<(), ProviderError> {
        let _writes = session_writes();
        delete_discarded_session(self.owner, &self.profile_id)
    }
}

/// Whether a profile of `owner` other than the one saved under `key` holds the
/// session `uid`: the account's own copies only, since a session belongs to
/// one account and another account never shares it.
pub(crate) fn another_profile_holds(
    store: &CredentialStore,
    owner: SessionOwner,
    key: &str,
    uid: &str,
) -> Result<bool, ProviderError> {
    let others = saved_sessions(store, owner)?
        .into_iter()
        .filter(|(saved, _)| saved != key)
        .map(|(_, session)| session);
    Ok(session_shared(uid, others))
}

/// True when another key among `others` holds a session with `uid`: a
/// duplicated profile shares its original's session, and revoking it for
/// one would sign the other out.
fn session_shared(uid: &str, others: impl IntoIterator<Item = ProtonSession>) -> bool {
    others.into_iter().any(|other| other.tokens.uid == uid)
}

/// The lease of `slug`, waited for up to [`EXCLUSIVE_WAIT`] while another
/// holder keeps it: `Ok(None)` once the wait is over. A lease storage that
/// fails is no contention, and no wait cures it: its error comes back at
/// once, never read as a holder that did not let go.
async fn acquire_exclusive(slug: &str) -> Result<Option<RefreshLease>, ProviderError> {
    let started = std::time::Instant::now();
    loop {
        let lease = RefreshLease::acquire_checked(slug);
        #[cfg(test)]
        let lease = test_seams::reported(format!("lease {slug}"), lease);
        if let Some(lease) = lease.await? {
            return Ok(Some(lease));
        }
        if started.elapsed() >= exclusive_wait() {
            return Ok(None);
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
async fn lease_of_saved(
    owner: SessionOwner,
    key: &str,
) -> Result<Option<RefreshLease>, ProviderError> {
    let store = open_store()?;
    loop {
        let Some(saved) = read(&store, owner, key)? else {
            return exclusive(false, None);
        };
        let lease = exclusive(
            true,
            acquire_exclusive(&refresh_lease_slug(&saved.tokens.uid)).await?,
        )?;
        // A sign-in may have changed the key while we waited. Never mutate
        // its replacement while holding only the old session's lease.
        if read(&store, owner, key)?.is_some_and(|now| now.tokens.uid == saved.tokens.uid) {
            return Ok(lease);
        }
        drop(lease);
    }
}

/// Moves the unbound session to `profile_id`, the profile that just connected
/// with it.
pub async fn bind_unbound_session(
    owner: SessionOwner,
    profile_id: &str,
    session: &ProtonSession,
) -> Result<ProtonSession, ProviderError> {
    let store = open_store()?;
    let mut keys = mutation_keys(owner, None)?;
    keys.push(owner.key(Some(profile_id)));
    let _keys = lock_keys(keys).await?;
    refuse_moved_away(&store, owner, Some(profile_id))?;
    let source_key = saved_key(&store, owner, None)?;
    let _lease = lease_of_saved(owner, &source_key).await?;
    let _writes = session_writes();
    let current = takeover_session(read(&store, owner, &source_key)?, session)?;
    if read(&store, owner, &owner.key(Some(profile_id)))?.is_some() {
        return Err(takeover_changed());
    }
    store_session(owner, Some(profile_id), &current)?;
    clear_deferred_revoke(owner, Some(profile_id));
    remove_saved(&store, owner, &source_key)?;
    Ok(current)
}

fn takeover_changed() -> ProviderError {
    ProviderError::AuthenticationFailed(
        "The pending Proton session changed during connection; connect again".into(),
    )
}

/// Called under the source lease. Adopt refreshes of the expected session,
/// but never consume a newer sign-in or a session another connect took.
fn takeover_session(
    current: Option<ProtonSession>,
    expected: &ProtonSession,
) -> Result<ProtonSession, ProviderError> {
    current
        .filter(|now| now.tokens.uid == expected.tokens.uid)
        .ok_or_else(takeover_changed)
}

/// Saves a new sign-in for `profile_id`, after ending on Proton the session
/// it replaces (unless another saved profile still uses that one).
pub async fn replace_session(
    owner: SessionOwner,
    profile_id: Option<&str>,
    session: &ProtonSession,
) -> Result<(), ProviderError> {
    let store = open_store()?;
    let _keys = lock_keys(mutation_keys(owner, profile_id)?).await?;
    refuse_moved_away(&store, owner, profile_id)?;
    let previous_key = saved_key(&store, owner, profile_id)?;
    let _lease = lease_of_saved(owner, &previous_key).await?;
    // Best effort: the user asked for the new sign-in, which a failed
    // sign-out of the old session does not stop (the outcome is logged).
    revoke_saved_session(owner, profile_id).await;
    let _writes = session_writes();
    if previous_key != owner.key(profile_id) {
        remove_saved(&store, owner, &previous_key)?;
    }
    store_session(owner, profile_id, session)?;
    clear_deferred_revoke(owner, profile_id);
    Ok(())
}

/// Drops the mark a purge of `profile_id` left to its account's unlock
/// ([`crate::user_partitions::defer_proton_revoke`]), under the key lock of
/// the sign-in, import or duplicate that just saved a session there for the
/// profile, which is its own. Best effort: the mark names the purged
/// session, so one left behind ends nothing else.
fn clear_deferred_revoke(owner: SessionOwner, profile_id: Option<&str>) {
    let (Some(user), Some(profile_id)) = (owner.user_id, profile_id) else {
        return;
    };
    if let Err(error) = crate::user_partitions::clear_deferred_proton_revoke(user, profile_id) {
        log::warn!("Proton: the mark of a purged session was not cleared: {error}");
    }
}

/// Refuses a sign-in's save for a profile a Move took out of the account
/// while the sign-in was open: the Move held the profile's key until it had
/// marked the id, so a sign-in that ends after it sees the mark under the
/// same key lock.
fn refuse_moved_away(
    store: &CredentialStore,
    owner: SessionOwner,
    profile_id: Option<&str>,
) -> Result<(), ProviderError> {
    let (Some(user), Some(profile_id)) = (owner.user_id, profile_id) else {
        return Ok(());
    };
    if crate::user_partitions::profile_moved_away(store, user, profile_id)
        .map_err(ProviderError::Other)?
    {
        return Err(ProviderError::Other(
            "this profile was moved to another account, so its sign-in is not saved here; sign in from that account"
                .into(),
        ));
    }
    Ok(())
}

/// Import or duplicate a session without revoking it. This is a mutation of
/// the destination key too, including when no session was stored there yet.
/// Hold the imported UID's lease as well, so a duplicate cannot retain a
/// refresh-token pair another local copy has just rotated.
pub async fn restore_session(
    owner: SessionOwner,
    profile_id: &str,
    raw: &str,
) -> Result<(), ProviderError> {
    restore(owner, profile_id, raw, false).await.map(|_| ())
}

/// [`restore_session`]; for an `import`, only into the one account that may
/// hold the session. A copy another account holds, which a sign-out of this
/// one would revoke, or a saved session this process cannot read, which may
/// be that copy, keeps it out: `Ok(false)`, nothing written, and the profile
/// signs in again. A duplicate stays inside the account its source is in.
async fn restore(
    owner: SessionOwner,
    profile_id: &str,
    raw: &str,
    import: bool,
) -> Result<bool, ProviderError> {
    let mut session = parse(raw)?;
    let store = open_store()?;
    let key = owner.key(Some(profile_id));
    let _keys = lock_keys(vec![key.clone()]).await?;
    refuse_moved_away(&store, owner, Some(profile_id))?;
    let previous = read(&store, owner, &key)?;
    let mut uids = vec![session.tokens.uid.clone()];
    if let Some(previous) = &previous {
        uids.push(previous.tokens.uid.clone());
    }
    uids.sort_unstable();
    uids.dedup();
    let mut leases = Vec::with_capacity(uids.len());
    for uid in uids {
        if let Some(lease) = exclusive(true, acquire_exclusive(&refresh_lease_slug(&uid)).await?)? {
            leases.push(lease);
        }
    }
    let _writes = session_writes();
    let current = read(&store, owner, &key)?;
    if current.as_ref().map(|saved| &saved.tokens.uid)
        != previous.as_ref().map(|saved| &saved.tokens.uid)
    {
        return Err(takeover_changed());
    }
    if import && held_by_another_account(&store, owner, &session.tokens.uid)? {
        return Ok(false);
    }
    let same = match current.filter(|saved| saved.tokens.uid == session.tokens.uid) {
        Some(saved) => Some(saved),
        None => saved_sessions(&store, owner)?
            .into_iter()
            .map(|(_, saved)| saved)
            .find(|saved| saved.tokens.uid == session.tokens.uid),
    };
    if let Some(saved) = same {
        session.tokens = saved.tokens;
    }
    store_session(owner, Some(profile_id), &session)?;
    clear_deferred_revoke(owner, Some(profile_id));
    Ok(true)
}

/// Whether an account other than `owner` may hold the session `uid`, as far
/// as this process can tell: for an account other than the legacy owner, a
/// vault key (the legacy owner's) that holds it; for any account, a row of
/// another account that holds it, or one it cannot read.
fn held_by_another_account(
    store: &CredentialStore,
    owner: SessionOwner,
    uid: &str,
) -> Result<bool, ProviderError> {
    if private_account(owner)?.is_some()
        && vault_sessions(|| store.list_accounts(), |key| store.get(key))?
            .iter()
            .any(|(_, saved)| saved.tokens.uid == uid)
    {
        return Ok(true);
    }
    crate::user_partitions::another_account_may_hold_session(store, owner.user_id, uid)
        .map_err(ProviderError::Other)
}

/// Synchronous profile duplicate callers keep their existing atomic
/// user-selection scope while the async key and UID leases are acquired.
pub fn restore_session_blocking(
    owner: SessionOwner,
    profile_id: &str,
    raw: &str,
) -> Result<(), ProviderError> {
    restore_blocking(owner, profile_id, raw, false).map(|_| ())
}

/// Why an import left a profile without the session it carried: another
/// account on this device may hold it (see [`import_session_blocking`]).
pub const IMPORT_WITHHELD: &str =
    "another account on this device may hold its Proton session, so it signs in again";

/// The restore of a session an import carries, for a synchronous caller:
/// `Ok(false)` when another account may hold it, so the profile signs in
/// again (see [`restore`]).
pub fn import_session_blocking(
    owner: SessionOwner,
    profile_id: &str,
    raw: &str,
) -> Result<bool, ProviderError> {
    restore_blocking(owner, profile_id, raw, true)
}

fn restore_blocking(
    owner: SessionOwner,
    profile_id: &str,
    raw: &str,
    import: bool,
) -> Result<bool, ProviderError> {
    let profile_id = profile_id.to_string();
    let raw = Zeroizing::new(raw.to_string());
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| ProviderError::Other(format!("Proton session runtime: {error}")))?;
        runtime.block_on(restore(owner, &profile_id, &raw, import))
    })
    .join()
    .map_err(|_| ProviderError::Other("Proton session restore stopped unexpectedly".into()))?
}

/// Resolve the session a status or explicit sign-out sees. Profile deletion
/// keeps using end_session directly and never consumes an unbound session.
pub fn saved_session_target(
    owner: SessionOwner,
    profile_id: Option<&str>,
) -> Result<(Option<&str>, bool), ProviderError> {
    saved_session_target_in(&open_store()?, owner, profile_id)
}

/// Read-only status using the caller's vault handle and captured owner.
pub fn saved_session_target_in<'a>(
    store: &CredentialStore,
    owner: SessionOwner,
    profile_id: Option<&'a str>,
) -> Result<(Option<&'a str>, bool), ProviderError> {
    resolve_session_target(profile_id, |id| {
        read(store, owner, &saved_key(store, owner, id)?).map(|session| session.is_some())
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
pub async fn end_session(
    owner: SessionOwner,
    profile_id: Option<&str>,
) -> Result<(), ProviderError> {
    let store = open_store()?;
    let _keys = lock_keys(mutation_keys(owner, profile_id)?).await?;
    let _lease = lease_of_saved(owner, &saved_key(&store, owner, profile_id)?).await?;
    // Best effort, as a sign-out or a delete asked for: the local session
    // goes whatever the outcome, which is logged.
    revoke_saved_session(owner, profile_id).await;
    let _writes = session_writes();
    delete_session(owner, profile_id)
}

/// [`end_session`] of the session a purge left to its account's unlock, for
/// a synchronous caller: ended only when `left_by_the_purge`, given the UID
/// of the session saved now, read under the key lock and the session's
/// lease, says it is the session the purge left. A session saved there
/// since stays. Whether one was ended. A sign-out on Proton that failed is an
/// error, the row kept: a purged session is never dropped while it may still
/// be live, and the next unlock tries again.
pub fn end_purged_session_blocking(
    owner: SessionOwner,
    profile_id: &str,
    left_by_the_purge: impl FnOnce(&str) -> Result<bool, String> + Send + 'static,
) -> Result<bool, ProviderError> {
    let profile_id = profile_id.to_string();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| ProviderError::Other(format!("Proton session runtime: {error}")))?;
        runtime.block_on(async {
            let store = open_store()?;
            let _keys = lock_keys(mutation_keys(owner, Some(&profile_id))?).await?;
            let key = saved_key(&store, owner, Some(&profile_id))?;
            let _lease = lease_of_saved(owner, &key).await?;
            let Some(saved) = read(&store, owner, &key)? else {
                return Ok(false);
            };
            if !left_by_the_purge(&saved.tokens.uid).map_err(ProviderError::Other)? {
                return Ok(false);
            }
            if let RevokeOutcome::Failed(error) =
                revoke_saved_session(owner, Some(&profile_id)).await
            {
                return Err(ProviderError::Other(format!(
                    "the purged Proton session was not ended on Proton, so it stays for the next unlock: {error}"
                )));
            }
            let _writes = session_writes();
            delete_session(owner, Some(&profile_id))?;
            Ok(true)
        })
    })
    .join()
    .map_err(|_| ProviderError::Other("Proton session cleanup stopped unexpectedly".into()))?
}

/// [`end_session`] for a synchronous caller (the CLI profile delete): it
/// runs on a thread of its own with its own runtime, so it works whether or
/// not the caller is inside a Tokio runtime.
pub fn end_session_blocking(
    owner: SessionOwner,
    profile_id: Option<&str>,
) -> Result<(), ProviderError> {
    let profile_id = profile_id.map(str::to_string);
    let joined = std::thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(end_session(owner, profile_id.as_deref())),
            Err(error) => Err(ProviderError::Other(format!(
                "Proton session runtime: {error}"
            ))),
        }
    })
    .join();
    joined
        .map_err(|_| ProviderError::Other("Proton session cleanup stopped unexpectedly".into()))?
}

/// What [`revoke_saved_session`] did on Proton.
#[derive(Debug, PartialEq, Eq)]
pub enum RevokeOutcome {
    /// No session is saved there.
    NothingSaved,
    /// The session was ended on Proton.
    Revoked,
    /// Kept alive on Proton: another saved profile of the account uses it.
    KeptShared,
    /// Kept alive on Proton: another account on this device holds it or may
    /// hold it.
    KeptForAnotherAccount,
    /// Not ended, and it may still be live on Proton: the sign-out failed or
    /// timed out, or the session, its copies or the other accounts' sessions
    /// could not be read. The reason names no UID.
    Failed(String),
}

/// Revokes on Proton the session saved for `profile_id`, before a profile
/// delete (or a new sign-in) replaces the key. A session that another key on
/// this device also holds is left alive: another profile of the account, or
/// another account the session index says holds it or may hold it
/// ([`kept_for_another_account`]). The outcome is logged and returned: a
/// sign-out or a delete goes on whatever it is, a purge left to an unlock
/// keeps the session while it is [`RevokeOutcome::Failed`].
pub async fn revoke_saved_session(owner: SessionOwner, profile_id: Option<&str>) -> RevokeOutcome {
    let failed = |what: &str, error: &dyn std::fmt::Display| {
        log::warn!("Proton: saved session not revoked, {what}: {error}");
        RevokeOutcome::Failed(format!("{what}: {error}"))
    };
    let store = match open_store() {
        Ok(store) => store,
        Err(error) => return failed("the vault could not be opened", &error),
    };
    let key = match saved_key(&store, owner, profile_id) {
        Ok(key) => key,
        Err(error) => return failed("its owner could not be read", &error),
    };
    let session = match read(&store, owner, &key) {
        Ok(Some(session)) => session,
        Ok(None) => return RevokeOutcome::NothingSaved,
        Err(error) => return failed("it could not be read", &error),
    };
    let shared = match another_profile_holds(&store, owner, &key, &session.tokens.uid) {
        Ok(shared) => shared,
        Err(error) => return failed("its copies could not be read", &error),
    };
    if shared {
        log::info!("Proton: session kept, another saved profile uses it");
        return RevokeOutcome::KeptShared;
    }
    match kept_for_another_account(&store, owner, &session.tokens.uid) {
        Ok(None) => {}
        Ok(Some(reason)) => {
            log::info!("Proton: session dropped here and kept on Proton, {reason}");
            return RevokeOutcome::KeptForAnotherAccount;
        }
        Err(error) => {
            return failed("the other accounts' sessions could not be read", &error);
        }
    }
    #[cfg(test)]
    if let Some(outcome) = test_seams::revoke_counted() {
        return outcome;
    }
    let api = ProtonApi::new(super::app_version::app_version());
    api.set_tokens(Some(session.tokens)).await;
    match tokio::time::timeout(REVOKE_TIMEOUT, super::auth::sign_out(&api)).await {
        Ok(Ok(())) => {
            log::info!("Proton: session revoked with its profile");
            RevokeOutcome::Revoked
        }
        Ok(Err(error)) => failed("the sign-out on Proton failed", &error),
        Err(_) => failed("the sign-out on Proton", &"timed out"),
    }
}

/// Why a sign-out of `owner` leaves the session `uid` alive on Proton for
/// another account, `None` when no other account holds it: for an account
/// other than the legacy owner, the legacy owner's vault copy; for any
/// account, the session index, where another account's fingerprint of the
/// same UID, or a session of another account the index does not describe,
/// keeps it. The reason names no UID.
fn kept_for_another_account(
    store: &CredentialStore,
    owner: SessionOwner,
    uid: &str,
) -> Result<Option<&'static str>, ProviderError> {
    use crate::user_partitions::OtherAccounts;
    if private_account(owner)?.is_some()
        && vault_sessions(|| store.list_accounts(), |key| store.get(key))?
            .iter()
            .any(|(_, saved)| saved.tokens.uid == uid)
    {
        return Ok(Some("the legacy owner's vault holds it"));
    }
    let others = crate::user_partitions::other_accounts_holding_session(store, owner.user_id, uid)
        .map_err(ProviderError::Other)?;
    Ok(match others {
        OtherAccounts::HoldNone => None,
        OtherAccounts::HoldIt => Some("another account on this device holds it"),
        OtherAccounts::MayHoldIt => Some(
            "another account holds a session saved before the session index, which may be it, until that account is unlocked",
        ),
    })
}

/// The lease slug of a session's refresh: one per session UID, so the
/// profiles that share a session refresh it one at a time, across processes
/// too. Hashed, since it becomes a file name and a UID is not one.
pub(crate) fn refresh_lease_slug(uid: &str) -> String {
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

fn is_session_key(key: &str) -> bool {
    key.starts_with(SESSION_KEY_PREFIX)
        || key == UNBOUND_SESSION_KEY
        || key.starts_with(USER_UNBOUND_PREFIX)
}

struct VaultKeys {
    store: CredentialStore,
    owner: SessionOwner,
    own_key: String,
    /// [`private_account`] of `owner`: its copies are its rows alone.
    private: Option<i64>,
}

impl SessionKeys for VaultKeys {
    fn keys(&self) -> Vec<String> {
        // A private account's copies are its own rows, listed without
        // decrypting them: one it cannot decrypt is left out when it is read,
        // and the others still take the refreshed pair.
        if let Some(user) = self.private {
            return crate::user_partitions::open_or_init_cli()
                .and_then(|conn| {
                    crate::user_partitions::user_credential_ids_of_type(&conn, user, PARTITION_TYPE)
                })
                .unwrap_or_else(|error| {
                    log::warn!("Proton: the account's saved sessions could not be listed: {error}");
                    Vec::new()
                });
        }
        self.store
            .list_accounts()
            .unwrap_or_default()
            .into_iter()
            .filter(|account| is_session_key(account))
            .collect()
    }

    fn read(&self, key: &str) -> Option<ProtonSession> {
        if key == self.own_key || self.private.is_some() {
            read(&self.store, self.owner, key).ok().flatten()
        } else {
            self.store.get(key).ok().and_then(|raw| parse(&raw).ok())
        }
    }

    fn write(&self, key: &str, session: &ProtonSession) -> Result<(), ProviderError> {
        #[cfg(test)]
        test_seams::refresh_write(key);
        let raw = Zeroizing::new(
            serde_json::to_string(&StoredSession::from(session))
                .map_err(|error| ProviderError::Other(error.to_string()))?,
        );
        // Each copy is written only while it still holds the session, and a
        // copy that is gone is not created: a sign-in, a sign-out, a Move or
        // the alignment changed or removed it since the refresh looked, in
        // this process or another, with the session's lease or without it.
        let uid = &session.tokens.uid;
        let same_session =
            |current: &str| parse(current).is_ok_and(|saved| saved.tokens.uid == *uid);
        let refreshed = |user: i64| -> Result<(), ProviderError> {
            let written = crate::user_partitions::refresh_user_credential(
                &self.store,
                user,
                key,
                PARTITION_TYPE,
                &same_session,
                &raw,
            )
            .map_err(ProviderError::Other)?;
            if written {
                indexed(&self.store, user, key);
            }
            Ok(())
        };
        if let Some(user) = self.private {
            return refreshed(user);
        }
        // The vault key is the legacy owner's, and so is the one row this
        // write carries the pair to. Another account's row of the same name
        // and session is that account's own copy: a matching session does not
        // make it the legacy owner's.
        replace_vault_session(&self.store, key, &same_session, &raw)?;
        let legacy = self
            .owner
            .user_id
            .map_or_else(crate::user_partitions::legacy_credential_owner_id, |user| {
                Ok(Some(user))
            })
            .map_err(ProviderError::Other)?;
        legacy.map_or(Ok(()), refreshed)
    }
}

/// The vault key `key` replaced by `raw` only while it holds a value
/// `still_holds` accepts, and never created: the read, the comparison and the
/// write share the vault's write lock. A key rewritten in the same session at
/// every attempt is left as it is.
fn replace_vault_session(
    store: &CredentialStore,
    key: &str,
    still_holds: &dyn Fn(&str) -> bool,
    raw: &str,
) -> Result<bool, ProviderError> {
    for _ in 0..3 {
        let current = match store.get(key) {
            Ok(current) => Zeroizing::new(current),
            Err(CredentialError::NotFound(_)) => return Ok(false),
            Err(error) => return Err(ProviderError::Other(error.to_string())),
        };
        if !still_holds(&current) {
            return Ok(false);
        }
        if store
            .replace_secret_if_unchanged(key, &current, raw)
            .map_err(|error| ProviderError::Other(error.to_string()))?
        {
            return Ok(true);
        }
    }
    Ok(false)
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
/// [`SESSION_WRITES`] keeps the look again and the write together, and the
/// write itself is a compare-and-write on the session, so a change made
/// without the lease (the alignment, another process) is not undone either.
/// `between` runs after the first look, for a test to change a key there.
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
    owner: SessionOwner,
    profile_id: Option<String>,
    uid: String,
    key_password: secrecy::SecretString,
}

impl VaultSession {
    pub fn new(owner: SessionOwner, profile_id: Option<&str>, session: &ProtonSession) -> Self {
        Self {
            owner,
            profile_id: profile_id.map(str::to_string),
            uid: session.tokens.uid.clone(),
            key_password: session.key_password.clone(),
        }
    }
}

#[async_trait::async_trait]
impl SessionStore for VaultSession {
    async fn lock(&self) -> Option<Box<dyn Send>> {
        match acquire_exclusive(&refresh_lease_slug(&self.uid)).await {
            Ok(lease) => lease.map(|lease| Box::new(lease) as Box<dyn Send>),
            Err(error) => {
                log::warn!("Proton: the refresh lease could not be taken: {error}");
                None
            }
        }
    }

    fn load(&self) -> Option<SessionTokens> {
        match load_session(self.owner, self.profile_id.as_deref()) {
            Ok(session) => session.map(|session| session.tokens),
            Err(error) => {
                log::warn!("Proton: saved session not readable: {error}");
                None
            }
        }
    }

    fn save(&self, tokens: &SessionTokens) {
        // Off the async worker: another process may be writing the vault.
        crate::credential_store::off_async_worker(|| {
            match (open_store(), private_account(self.owner)) {
                (Ok(store), Ok(private)) => save_refreshed(
                    &VaultKeys {
                        store,
                        owner: self.owner,
                        own_key: self.owner.key(self.profile_id.as_deref()),
                        private,
                    },
                    &self.owner.key(self.profile_id.as_deref()),
                    &self.uid,
                    &self.key_password,
                    tokens,
                ),
                (Err(error), _) | (_, Err(error)) => {
                    log::warn!("Proton: refreshed session not saved: {error}")
                }
            }
        })
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
    fn key_guards_cover_absent_sessions_across_processes() {
        const CHILD: &str = "AEROFTP_PROTON_KEY_GUARD_TEST";
        const PATH: &str = "AEROFTP_PROTON_KEY_GUARD_PATH";
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        if let Some(mode) = std::env::var_os(CHILD) {
            let path = std::path::PathBuf::from(std::env::var_os(PATH).unwrap());
            let result = runtime.block_on(lock_key_file(&path, Duration::ZERO));
            assert_eq!(result.is_ok(), mode == "free");
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("absent-session.lock");
        let held = runtime
            .block_on(lock_key_file(&path, Duration::ZERO))
            .unwrap();
        let probe = |mode| {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "providers::proton_native::vault::tests::key_guards_cover_absent_sessions_across_processes", "--test-threads=1"])
                .env(CHILD, mode).env(PATH, &path).output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        probe("held");
        drop(held);
        assert!(
            path.is_file(),
            "a persistent lock inode must not be unlinked"
        );
        probe("free");
    }

    #[test]
    fn takeover_requires_the_expected_uid_and_adopts_its_rotated_pair() {
        let expected = session("first");
        assert!(takeover_session(None, &expected).is_err());
        assert!(takeover_session(Some(session("replacement")), &expected).is_err());
        let mut refreshed = session("first");
        refreshed.tokens.access_token = SecretString::from("refreshed".to_string());
        assert_eq!(
            takeover_session(Some(refreshed), &expected)
                .unwrap()
                .tokens
                .access_token
                .expose_secret(),
            "refreshed"
        );
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

    /// A copy the vault cannot list or read is not a missing copy: the sign-out
    /// that asks whether another profile holds the session gets an error and
    /// keeps the session, instead of revoking one that profile may hold. A key
    /// gone since the list is no copy.
    #[test]
    fn a_copy_the_vault_cannot_read_stops_the_revoke() {
        let raw = serde_json::to_string(&StoredSession::from(&session("uid-1"))).unwrap();
        let (held, gone) = (session_key(Some("held")), session_key(Some("gone")));
        let refused = || CredentialError::Encryption("synthetic refusal".into());
        assert!(vault_sessions(|| Err(refused()), |_| Ok(raw.clone())).is_err());
        assert!(vault_sessions(|| Ok(vec![held.clone()]), |_| Err(refused())).is_err());
        let found = vault_sessions(
            || Ok(vec![gone.clone(), held.clone()]),
            |key| {
                if key == held {
                    Ok(raw.clone())
                } else {
                    Err(CredentialError::NotFound(key.into()))
                }
            },
        )
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, held);
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
