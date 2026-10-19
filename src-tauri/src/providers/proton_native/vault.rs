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
use crate::credential_store::CredentialStore;
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

/// A non-expiring OS lock for a vault key, including a key not created yet.
/// Keep the file after unlocking: unlinking it could let a second inode be
/// locked while another process still holds the first one. A crash releases
/// the OS lock, so no stale timeout or lock-file stealing is needed.
struct KeyLease(std::fs::File);

#[cfg(test)]
type KeyLockTestHook = Option<Box<dyn FnMut(&std::path::Path, &str)>>;

#[cfg(test)]
thread_local! {
    static KEY_LOCK_TEST_HOOK: std::cell::RefCell<KeyLockTestHook> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn observe_key_locks_for_test(callback: impl FnMut(&std::path::Path, &str) + 'static) {
    KEY_LOCK_TEST_HOOK.set(Some(Box::new(callback)));
}

#[cfg(test)]
fn key_lock_test_hook(path: &std::path::Path, stage: &str) {
    KEY_LOCK_TEST_HOOK.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook(path, stage);
        }
    });
}

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
    let deadline = std::time::Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => {
                #[cfg(test)]
                key_lock_test_hook(path, "acquired");
                return Ok(KeyLease(file));
            }
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                #[cfg(test)]
                key_lock_test_hook(path, "contended");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(ProviderError::Other(
                    "The Proton session is being changed elsewhere; try again in a moment".into(),
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
        leases.push(lock_key_file(&path, EXCLUSIVE_WAIT).await?);
    }
    Ok(leases)
}

/// Cross-user relocation resolves partition DEKs on its calling thread, then
/// takes these guards without retaining USER_SESSION or a SQLite transaction.
/// Keep the key guards while discovering both UIDs: even an absent destination
/// must be protected from a concurrent sign-in.
pub(crate) struct RelocationKeys {
    keys: Vec<KeyLease>,
    source_key: String,
    target_key: String,
}

pub(crate) struct RelocationGuards {
    _keys: Vec<KeyLease>,
    _sessions: Vec<RefreshLease>,
    source_key: String,
    target_key: String,
    source_uid: Option<String>,
    target_uid: Option<String>,
    source_vault_uid: Option<String>,
    target_vault_uid: Option<String>,
}

fn vault_session(
    store: &CredentialStore,
    key: &str,
) -> Result<Option<ProtonSession>, ProviderError> {
    match store.get(key) {
        Ok(raw) => parse(&raw).map(Some),
        Err(crate::credential_store::CredentialError::NotFound(_)) => Ok(None),
        Err(error) => Err(ProviderError::Other(format!(
            "Read Proton relocation session: {error}"
        ))),
    }
}

fn session_uid(raw: Option<&str>) -> Result<Option<String>, ProviderError> {
    raw.map(parse)
        .transpose()
        .map(|session| session.map(|session| session.tokens.uid))
}

fn relocation_runtime() -> Result<tokio::runtime::Runtime, ProviderError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| ProviderError::Other(format!("Proton session runtime: {error}")))
}

pub(crate) fn lock_relocation_keys_blocking(
    source_profile: &str,
    target_profile: &str,
) -> Result<RelocationKeys, ProviderError> {
    let source_key = session_key(Some(source_profile));
    let target_key = session_key(Some(target_profile));
    std::thread::spawn(move || {
        let keys = relocation_runtime()?
            .block_on(lock_keys(vec![source_key.clone(), target_key.clone()]))?;
        Ok(RelocationKeys {
            keys,
            source_key,
            target_key,
        })
    })
    .join()
    .map_err(|_| {
        ProviderError::Other("Proton relocation key locking stopped unexpectedly".into())
    })?
}

impl RelocationKeys {
    /// The caller reads both explicit partitions after acquiring the key locks.
    /// Refreshes can rotate the captured pair while we wait, but cannot change
    /// its UID. Reread the pair with `with_current_session` after this returns.
    pub(crate) fn lock_sessions_blocking(
        self,
        store: &CredentialStore,
        source_raw: Option<&str>,
        target_raw: Option<&str>,
    ) -> Result<RelocationGuards, ProviderError> {
        let source_uid = session_uid(source_raw)?;
        let target_uid = session_uid(target_raw)?;
        let source_vault_uid =
            vault_session(store, &self.source_key)?.map(|session| session.tokens.uid);
        let target_vault_uid =
            vault_session(store, &self.target_key)?.map(|session| session.tokens.uid);
        std::thread::spawn(move || {
            let mut uids: Vec<String> = source_uid
                .iter()
                .chain(target_uid.iter())
                .chain(source_vault_uid.iter())
                .chain(target_vault_uid.iter())
                .cloned()
                .collect();
            uids.sort_unstable();
            uids.dedup();
            let sessions = relocation_runtime()?.block_on(async {
                let mut sessions = Vec::with_capacity(uids.len());
                for uid in uids {
                    if let Some(lease) =
                        exclusive(true, acquire_exclusive(&refresh_lease_slug(&uid)).await)?
                    {
                        sessions.push(lease);
                    }
                }
                Ok::<_, ProviderError>(sessions)
            })?;
            Ok(RelocationGuards {
                _keys: self.keys,
                _sessions: sessions,
                source_key: self.source_key,
                target_key: self.target_key,
                source_uid,
                target_uid,
                source_vault_uid,
                target_vault_uid,
            })
        })
        .join()
        .map_err(|_| {
            ProviderError::Other("Proton relocation session locking stopped unexpectedly".into())
        })?
    }
}

impl RelocationGuards {
    /// Identical profile IDs can exist in different partitions. Never remove
    /// another owner's global fallback when only the scoped source held ours.
    pub(crate) fn source_vault_matches(&self) -> bool {
        self.source_uid.is_some() && self.source_uid == self.source_vault_uid
    }

    /// Run profile/partition commit and subsequent vault bookkeeping under the
    /// same guards. The caller supplies session-free reads of the explicit
    /// source and target partitions, never an active-user lookup.
    pub(crate) fn with_current_session<R>(
        &self,
        store: &CredentialStore,
        source_raw: Option<&str>,
        target_raw: Option<&str>,
        apply: impl FnOnce(Option<&str>) -> Result<R, String>,
    ) -> Result<R, String> {
        let _writes = session_writes();
        let source_vault =
            vault_session(store, &self.source_key).map_err(|error| error.to_string())?;
        let target_vault =
            vault_session(store, &self.target_key).map_err(|error| error.to_string())?;
        if session_uid(source_raw).map_err(|error| error.to_string())? != self.source_uid
            || session_uid(target_raw).map_err(|error| error.to_string())? != self.target_uid
            || source_vault.as_ref().map(|session| &session.tokens.uid)
                != self.source_vault_uid.as_ref()
            || target_vault.as_ref().map(|session| &session.tokens.uid)
                != self.target_vault_uid.as_ref()
        {
            return Err("The Proton session changed during profile relocation; try again".into());
        }
        let mut source = source_raw
            .map(parse)
            .transpose()
            .map_err(|error| error.to_string())?;
        if let Some(session) = &mut source {
            // A refresh writes the vault before its best-effort partition
            // mirror. Adopt that exact newer pair only for the same UID.
            let same = source_vault.filter(|saved| saved.tokens.uid == session.tokens.uid);
            let same = match same {
                Some(saved) => Some(saved),
                None => {
                    // A prior committed vault-write failure can leave only
                    // the scoped partition copy. A different local profile
                    // may since have rotated this UID; it is protected by
                    // the same held lease, just as in guarded import/restore.
                    let mut keys = store.list_accounts().map_err(|error| error.to_string())?;
                    keys.sort_unstable();
                    keys.into_iter()
                        .filter(|key| is_session_key(key))
                        .filter_map(|key| vault_session(store, &key).ok().flatten())
                        .find(|saved| saved.tokens.uid == session.tokens.uid)
                }
            };
            if let Some(saved) = same {
                session.tokens = saved.tokens;
            }
        }
        let raw = source
            .as_ref()
            .map(|session| {
                serde_json::to_string(&StoredSession::from(session))
                    .map(Zeroizing::new)
                    .map_err(|error| format!("Encode Proton relocation session: {error}"))
            })
            .transpose()?;
        apply(raw.as_deref().map(String::as_str))
    }
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

fn read(
    store: &CredentialStore,
    owner: SessionOwner,
    key: &str,
) -> Result<Option<ProtonSession>, ProviderError> {
    let result = match owner.user_id {
        Some(user) => crate::user_partitions::resolve_credential_for_user(store, user, key),
        None => Ok(store.get(key).ok().map(Zeroizing::new)),
    };
    match result {
        Ok(Some(json)) => {
            let mut session = parse(&json)?;
            // A locked shared copy may not have accepted its mirror. The
            // vault is written first, so its newer pair for this SAME UID is
            // authoritative. A different user's session is never adopted.
            if let Ok(raw) = store.get(key) {
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
/// isolated. `None` ownership is the pre-partition legacy vault.
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
    store
        .store(&key, &json)
        .map_err(|error| ProviderError::Other(format!("the vault refused the session: {error}")))?;
    if let Some(user) = owner.user_id {
        crate::user_partitions::mirror_credential_for_user_typed(
            &store,
            user,
            &key,
            PARTITION_TYPE,
            &json,
        );
    }
    Ok(())
}

/// Removes the session saved for `profile_id` from the vault and the captured
/// user's partition. Nothing saved is not an error.
pub fn delete_session(owner: SessionOwner, profile_id: Option<&str>) -> Result<(), ProviderError> {
    let store = open_store()?;
    let key = saved_key(&store, owner, profile_id)?;
    store
        .delete(&key)
        .map_err(|error| ProviderError::Other(format!("the vault refused the delete: {error}")))?;
    if let Some(user) = owner.user_id {
        crate::user_partitions::unmirror_credential_for_user_best_effort_any(user, &key);
    }
    Ok(())
}

/// Remove a failed local duplicate without revoking its source's shared
/// remote session. The fresh destination still uses normal mutation guards.
pub fn discard_session_blocking(
    owner: SessionOwner,
    profile_id: &str,
) -> Result<(), ProviderError> {
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
            let _writes = session_writes();
            delete_session(owner, Some(&profile_id))
        })
    })
    .join()
    .map_err(|_| ProviderError::Other("Proton duplicate cleanup stopped unexpectedly".into()))?
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
            acquire_exclusive(&refresh_lease_slug(&saved.tokens.uid)).await,
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
    let source_key = saved_key(&store, owner, None)?;
    let _lease = lease_of_saved(owner, &source_key).await?;
    let _writes = session_writes();
    let current = takeover_session(read(&store, owner, &source_key)?, session)?;
    if read(&store, owner, &owner.key(Some(profile_id)))?.is_some() {
        return Err(takeover_changed());
    }
    store_session(owner, Some(profile_id), &current)?;
    store
        .delete(&source_key)
        .map_err(|error| ProviderError::Other(error.to_string()))?;
    if let Some(user) = owner.user_id {
        crate::user_partitions::unmirror_credential_for_user_best_effort_any(user, &source_key);
    }
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
    let previous_key = saved_key(&store, owner, profile_id)?;
    let _lease = lease_of_saved(owner, &previous_key).await?;
    revoke_saved_session(owner, profile_id).await;
    let _writes = session_writes();
    if previous_key != owner.key(profile_id) {
        store
            .delete(&previous_key)
            .map_err(|error| ProviderError::Other(error.to_string()))?;
        if let Some(user) = owner.user_id {
            crate::user_partitions::unmirror_credential_for_user_best_effort_any(
                user,
                &previous_key,
            );
        }
    }
    store_session(owner, profile_id, session)
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
    let mut session = parse(raw)?;
    let store = open_store()?;
    let key = owner.key(Some(profile_id));
    let _keys = lock_keys(vec![key.clone()]).await?;
    let previous = read(&store, owner, &key)?;
    let mut uids = vec![session.tokens.uid.clone()];
    if let Some(previous) = &previous {
        uids.push(previous.tokens.uid.clone());
    }
    uids.sort_unstable();
    uids.dedup();
    let mut leases = Vec::with_capacity(uids.len());
    for uid in uids {
        if let Some(lease) = exclusive(true, acquire_exclusive(&refresh_lease_slug(&uid)).await)? {
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
    let same = current
        .filter(|saved| saved.tokens.uid == session.tokens.uid)
        .or_else(|| {
            store
                .list_accounts()
                .unwrap_or_default()
                .into_iter()
                .filter(|key| is_session_key(key))
                .filter_map(|key| store.get(&key).ok().and_then(|raw| parse(&raw).ok()))
                .find(|saved| saved.tokens.uid == session.tokens.uid)
        });
    if let Some(saved) = same {
        session.tokens = saved.tokens;
    }
    store_session(owner, Some(profile_id), &session)
}

/// Synchronous profile import/duplicate callers keep their existing atomic
/// user-selection scope while the async key and UID leases are acquired.
pub fn restore_session_blocking(
    owner: SessionOwner,
    profile_id: &str,
    raw: &str,
) -> Result<(), ProviderError> {
    let profile_id = profile_id.to_string();
    let raw = Zeroizing::new(raw.to_string());
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| ProviderError::Other(format!("Proton session runtime: {error}")))?;
        runtime.block_on(restore_session(owner, &profile_id, &raw))
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
    revoke_saved_session(owner, profile_id).await;
    let _writes = session_writes();
    delete_session(owner, profile_id)
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

/// Revokes on Proton the session saved for `profile_id`, best effort, before
/// a profile delete (or a new sign-in) replaces the key. A session that
/// another key on this device also holds is left alive. Never fails the
/// caller: errors are logged.
pub async fn revoke_saved_session(owner: SessionOwner, profile_id: Option<&str>) {
    let store = match open_store() {
        Ok(store) => store,
        Err(_) => return,
    };
    let key = match saved_key(&store, owner, profile_id) {
        Ok(key) => key,
        Err(error) => {
            log::warn!("Proton: saved session owner could not be read: {error}");
            return;
        }
    };
    let session = match read(&store, owner, &key) {
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
        .filter(|account| is_session_key(account) && *account != key)
        .filter_map(|account| store.get(&account).ok().and_then(|raw| parse(&raw).ok()));
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

fn is_session_key(key: &str) -> bool {
    key.starts_with(SESSION_KEY_PREFIX)
        || key == UNBOUND_SESSION_KEY
        || key.starts_with(USER_UNBOUND_PREFIX)
}

struct VaultKeys {
    store: CredentialStore,
    owner: SessionOwner,
    own_key: String,
}

impl SessionKeys for VaultKeys {
    fn keys(&self) -> Vec<String> {
        self.store
            .list_accounts()
            .unwrap_or_default()
            .into_iter()
            .filter(|account| is_session_key(account))
            .collect()
    }

    fn read(&self, key: &str) -> Option<ProtonSession> {
        if key == self.own_key {
            read(&self.store, self.owner, key).ok().flatten()
        } else {
            self.store.get(key).ok().and_then(|raw| parse(&raw).ok())
        }
    }

    fn write(&self, key: &str, session: &ProtonSession) -> Result<(), ProviderError> {
        let raw = Zeroizing::new(
            serde_json::to_string(&StoredSession::from(session))
                .map_err(|error| ProviderError::Other(error.to_string()))?,
        );
        // Determine owners before writing the global fallback. An identically
        // named key in another partition can hold a different session.
        let mut owners = if let Some(profile) = key.strip_prefix(SESSION_KEY_PREFIX) {
            crate::user_partitions::profile_credential_owner_ids(&self.store, profile, key)
                .unwrap_or_else(|error| {
                    log::warn!("Proton: shared session owners could not be read: {error}");
                    Vec::new()
                })
        } else if let Some(user) = key
            .strip_prefix(USER_UNBOUND_PREFIX)
            .and_then(|user| user.parse::<i64>().ok())
        {
            vec![user]
        } else {
            Vec::new()
        };
        if key == self.own_key {
            owners.extend(self.owner.user_id);
        }
        owners.sort_unstable();
        owners.dedup();
        let owners: Vec<i64> = owners
            .into_iter()
            .filter(|user| {
                read(&self.store, SessionOwner::for_user(Some(*user)), key)
                    .ok()
                    .flatten()
                    .is_some_and(|saved| saved.tokens.uid == session.tokens.uid)
            })
            .collect();
        self.store
            .store(key, &raw)
            .map_err(|error| ProviderError::Other(error.to_string()))?;
        for user in owners {
            crate::user_partitions::mirror_credential_for_user_typed(
                &self.store,
                user,
                key,
                PARTITION_TYPE,
                &raw,
            );
        }
        Ok(())
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
        acquire_exclusive(&refresh_lease_slug(&self.uid))
            .await
            .map(|lease| Box::new(lease) as Box<dyn Send>)
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
        match open_store() {
            Ok(store) => save_refreshed(
                &VaultKeys {
                    store,
                    owner: self.owner,
                    own_key: self.owner.key(self.profile_id.as_deref()),
                },
                &self.owner.key(self.profile_id.as_deref()),
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
