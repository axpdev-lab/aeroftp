// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! What one bridge scan (`import_bridge_config`) wrote before its profiles
//! were saved, so nothing it wrote outlives a scan the window never
//! confirmed.
//!
//! A scan stores each profile's secrets (passwords, OAuth and Jottacloud
//! tokens, native Proton sessions, crypt and Filen keys) and the OAuth app a
//! provider has none of when it reads the file; the profiles are saved
//! later, by the confirm. Every write is
//! recorded first, in a ledger kept where every process reads it, by key and
//! by a keyed MAC of what was written (never the secret): a header row that
//! names the account, and one row per write, so a record costs one row
//! whatever the scan wrote before it. Settling the scan (the window confirmed
//! it, closed it or scanned again) removes what it wrote for every profile
//! the account does not list, and only while it is still what the scan
//! wrote: a value saved since, or a profile the confirm saved, stays. A
//! ledger no live scan holds (the window went with a crash or a quit, and
//! its process with it) is settled at the next scan or the next opening of
//! the bridge panel. A live scan holds a file lock of its own, so a scan of
//! another process is never taken for one whose window is gone; where the
//! filesystem has no file locks, the lock file names the process instead,
//! and only a process proven gone gives its scan up.

use crate::credential_store::{CredentialError, CredentialStore};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::Mutex;
use zeroize::Zeroizing;

/// Prefix of a scan's ledger header in `global_state`.
const LEDGER_PREFIX: &str = "bridge_import_scan_";

/// Prefix of a scan's ledger entries in `global_state`: not a prefix of
/// [`LEDGER_PREFIX`], so a listing of the headers never meets an entry.
const ENTRY_PREFIX: &str = "bridge_import_entry_";

/// The prefix of the entries of the scan `id`.
fn entry_prefix(id: &str) -> String {
    format!("{ENTRY_PREFIX}{id}_")
}

/// The scans of this process no window has settled yet, each with the lock
/// file it holds while it lives (`None` on a filesystem without file locks).
static LIVE: Mutex<Vec<(String, Option<std::fs::File>)>> = Mutex::new(Vec::new());

/// The lock file a live scan `id` holds, beside the account database.
pub(crate) fn lock_path(id: &str) -> Result<std::path::PathBuf, String> {
    let db = crate::user_partitions::cli_db_path()?;
    let dir = db
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("bridge-import-scans");
    Ok(dir.join(format!("{id}.lock")))
}

/// Plays a filesystem without file locks (test only). Process wide, not per
/// thread: a settle runs on a blocking thread of its own.
#[cfg(test)]
pub(crate) static LOCKS_UNAVAILABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn try_lock(file: &std::fs::File) -> Result<(), std::fs::TryLockError> {
    #[cfg(test)]
    if LOCKS_UNAVAILABLE.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(std::fs::TryLockError::Error(std::io::Error::from(
            std::io::ErrorKind::Unsupported,
        )));
    }
    file.try_lock()
}

/// A process as a lock file without a lock names it: the boot and pid
/// namespace its pid belongs to, the pid, and when it started, so a pid
/// reused by another process is not taken for it.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
pub(crate) struct Holder {
    space: Option<String>,
    pid: u32,
    start: Option<u64>,
}

impl Holder {
    fn this_process() -> Self {
        let pid = std::process::id();
        Self {
            space: crate::user_partitions::pid_space(),
            pid,
            start: crate::user_partitions::process_start(pid).ok().flatten(),
        }
    }

    /// Whether the process named is proven gone: this process, which holds
    /// no such live scan (its `LIVE` was read first), or a process of this
    /// boot and pid namespace whose pid is free, a zombie, or another
    /// process's. Anything that cannot be read is not proof.
    fn gone(&self) -> bool {
        let this = Self::this_process();
        let (Some(space), Some(start)) = (&self.space, self.start) else {
            return false;
        };
        if this.space.as_ref() != Some(space) {
            return false;
        }
        if *self == this {
            return true;
        }
        match crate::user_partitions::process_start(self.pid) {
            Ok(None) => true,
            Ok(Some(now)) => now != start,
            Err(()) => false,
        }
    }
}

/// Takes the lock of the new scan `id`: `None` where the filesystem has no
/// file locks, and the lock file then names this process.
fn hold(id: &str) -> Result<Option<std::fs::File>, String> {
    let path = lock_path(id)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Create the import lock folder: {e}"))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("Open the import lock: {e}"))?;
    match try_lock(&file) {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::Error(e))
            if crate::user_partitions::file_locks_unavailable(&e) =>
        {
            use std::io::Write;
            let holder = serde_json::to_vec(&Holder::this_process())
                .map_err(|e| format!("Name the import lock's process: {e}"))?;
            file.write_all(&holder)
                .and_then(|()| file.sync_all())
                .map_err(|e| format!("Name the import lock's process: {e}"))?;
            Ok(None)
        }
        Err(std::fs::TryLockError::Error(e)) => Err(format!("Take the import lock: {e}")),
        Err(std::fs::TryLockError::WouldBlock) => Err("Take the import lock: it is held".into()),
    }
}

/// Whether a live scan holds the ledger `id`: one of this process, the lock
/// of one of another, or, without file locks, the lock file of a process not
/// proven gone. A lock that cannot be read counts as held, so a scan is
/// never settled under a window that may still confirm it.
fn held(id: &str) -> bool {
    if LIVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .any(|(live, _)| live == id)
    {
        return true;
    }
    let Ok(path) = lock_path(id) else {
        return true;
    };
    let file = match std::fs::OpenOptions::new().write(true).open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    match try_lock(&file) {
        Ok(()) => false,
        Err(std::fs::TryLockError::Error(e))
            if crate::user_partitions::file_locks_unavailable(&e) =>
        {
            !std::fs::read(&path)
                .ok()
                .and_then(|holder| serde_json::from_slice::<Holder>(&holder).ok())
                .is_some_and(|holder| holder.gone())
        }
        Err(std::fs::TryLockError::Error(_)) | Err(std::fs::TryLockError::WouldBlock) => true,
    }
}

/// Ends the life of the scan `id` in this process: its lock goes, and the file.
fn release(id: &str) {
    LIVE.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|(live, _)| live != id);
    if let Ok(path) = lock_path(id) {
        let _ = std::fs::remove_file(path);
    }
}

/// What the refusal of a scan says when the active account changed while
/// the file was read.
pub(crate) const SCAN_ACCOUNT_CHANGED: &str = "The active account changed while the file was read, so nothing was imported. Make the account you want to import into the active one again and import the same file again.";

/// The header of a scan's ledger: the account its secrets went to.
#[derive(Serialize, Deserialize, Default)]
struct Header {
    user: Option<i64>,
}

/// One write of a scan: the profile it is for, its key, and the MAC of the
/// secret (of the session UID for a Proton session). An OAuth app's slot is
/// shared by the provider's profiles: `holders` names every profile of the
/// scan that uses it.
#[derive(Serialize, Deserialize, Clone)]
struct Written {
    profile: String,
    key: String,
    kind: Kind,
    mac: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    holders: Vec<String>,
}

impl Written {
    /// Whether a profile the account lists, among `listed`, keeps this write.
    fn kept_by(&self, listed: &[String]) -> bool {
        listed.contains(&self.profile) || self.holders.iter().any(|id| listed.contains(id))
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    /// A secret of the dual writers: the vault key and the account's row.
    Secret,
    /// A native Proton Drive session saved for the profile.
    ProtonSession,
    /// A slot of a provider's OAuth app, in the vault alone.
    OauthApp,
}

/// A scan in progress: the account its secrets go to and the profiles it read.
pub(crate) struct Scan {
    id: String,
    profiles: Vec<String>,
    mac_key: Zeroizing<[u8; 32]>,
    /// The connection its entries are written on, and how many it wrote.
    ledger: Mutex<(Connection, u64)>,
}

/// The key the ledger's MACs are made with: derived from this vault's
/// partition root key, so a ledger names no secret anyone could test.
fn mac_key(store: &CredentialStore) -> Zeroizing<[u8; 32]> {
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let mut key = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<Sha256>::new(None, root_key.as_ref())
        .expand(b"aeroftp-bridge-import-scan-v1", key.as_mut())
        .expect("HKDF expand");
    key
}

fn mac(key: &[u8; 32], value: &str) -> String {
    use hmac::Mac;
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(value.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Runs once in the next [`Scan::begin`], right after its header is written
/// (test only): an abandoned-scan cleanup running meanwhile.
#[cfg(test)]
#[allow(clippy::type_complexity)]
pub(crate) static AFTER_SCAN_HEADER: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

/// Refuses every record of a scan (test only), as a database that fails.
#[cfg(test)]
pub(crate) static RECORD_REFUSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

impl Scan {
    /// Begins a scan whose secrets go to `user`, for the profiles `profiles`
    /// of the file, its header written before any secret is.
    pub(crate) fn begin(
        store: &CredentialStore,
        user: Option<i64>,
        profiles: Vec<String>,
    ) -> Result<Self, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let lock = hold(&id)?;
        // Live before its header is: a cleanup that reads the header finds
        // the scan held here, where no file lock can say so.
        LIVE.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((id.clone(), lock));
        let begun = crate::user_partitions::open_or_init_cli().and_then(|conn| {
            let header = serde_json::to_string(&Header { user })
                .map_err(|e| format!("Encode the import record: {e}"))?;
            conn.execute(
                "INSERT INTO global_state(key, value, updated_at) VALUES (?1, ?2, ?3)",
                params![
                    format!("{LEDGER_PREFIX}{id}"),
                    header,
                    chrono::Utc::now().timestamp_millis()
                ],
            )
            .map_err(|e| format!("Write the import record: {e}"))?;
            #[cfg(test)]
            if let Some(hook) = AFTER_SCAN_HEADER
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                hook();
            }
            Ok(conn)
        });
        let conn = match begun {
            Ok(conn) => conn,
            Err(error) => {
                release(&id);
                return Err(error);
            }
        };
        Ok(Self {
            id,
            profiles,
            mac_key: mac_key(store),
            ledger: Mutex::new((conn, 0)),
        })
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// The profile of the file that `key` belongs to, by the keys each
    /// profile's readers compose ([`crate::per_profile_vault_keys_for_target_reset`]:
    /// an OAuth token's key lowercases the id, so `oauth_dropbox_team` is
    /// `Team`'s), the id the key ends with as it is (after an underscore)
    /// first, then the longest. A key no profile composes is the longest id
    /// it ends with.
    fn profile_of(&self, key: &str) -> String {
        let ends_with = |id: &str| key.strip_suffix(id).is_some_and(|head| head.ends_with('_'));
        self.profiles
            .iter()
            .filter(|id| !id.is_empty())
            .filter(|id| {
                ends_with(id)
                    || crate::per_profile_vault_keys_for_target_reset(id, &[])
                        .iter()
                        .any(|composed| composed == key)
            })
            .max_by_key(|id| (ends_with(id), id.len()))
            .cloned()
            .unwrap_or_default()
    }

    /// Records, before the write, that the scan writes `value` (a secret, or
    /// a Proton session's UID) under `key`. A record that cannot be kept
    /// refuses the write, so nothing is written the scan could not remove.
    pub(crate) fn record(&self, key: &str, kind: Kind, value: &str) -> Result<(), String> {
        self.record_for(&self.profile_of(key), key, kind, value)
    }

    /// [`Scan::record`] for a write whose profile is known: `profile`. One
    /// row is written, the entry's own.
    pub(crate) fn record_for(
        &self,
        profile: &str,
        key: &str,
        kind: Kind,
        value: &str,
    ) -> Result<(), String> {
        self.record_entry(Written {
            profile: profile.to_string(),
            key: key.to_string(),
            kind,
            mac: mac(&self.mac_key, value),
            holders: Vec::new(),
        })
    }

    /// Records, before the write, that the scan fills the slot `key` of an
    /// OAuth app with `value`, for the profiles `holders` of the file that
    /// use the app: it is taken back only when the account lists none of
    /// them.
    pub(crate) fn record_app(
        &self,
        holders: &[String],
        key: &str,
        value: &str,
    ) -> Result<(), String> {
        self.record_entry(Written {
            profile: holders.first().cloned().unwrap_or_default(),
            key: key.to_string(),
            kind: Kind::OauthApp,
            mac: mac(&self.mac_key, value),
            holders: holders.to_vec(),
        })
    }

    /// Records that the scan relies on the slot `key` of an OAuth app, which
    /// already holds the `value` its config carries, when another scan filled
    /// it and has not settled: that scan's settle then leaves the slot to
    /// this one ([`claimed_by_another_scan`]), which takes it back only when
    /// none of `holders` is saved. A slot no scan records (the user's own
    /// app) is no scan's to take back, and nothing is recorded. The caller
    /// holds the list lock, as a settle does, so the scan that filled it
    /// cannot settle between the look and the record.
    pub(crate) fn share_app(
        &self,
        holders: &[String],
        key: &str,
        value: &str,
    ) -> Result<(), String> {
        let written = Written {
            profile: holders.first().cloned().unwrap_or_default(),
            key: key.to_string(),
            kind: Kind::OauthApp,
            mac: mac(&self.mac_key, value),
            holders: holders.to_vec(),
        };
        let claimed = {
            let ledger = self
                .ledger
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            claimed_by_another_scan(&ledger.0, &self.id, None, &written)?
        };
        if claimed {
            self.record_entry(written)?;
        }
        Ok(())
    }

    /// Writes the ledger row of `written`.
    fn record_entry(&self, written: Written) -> Result<(), String> {
        let entry = serde_json::to_string(&written)
            .map_err(|e| format!("Encode the import record: {e}"))?;
        let mut ledger = self
            .ledger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        #[cfg(test)]
        if RECORD_REFUSED.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("Write the import record: refused (test)".to_string());
        }
        let (conn, count) = &mut *ledger;
        conn.execute(
            "INSERT INTO global_state(key, value, updated_at) VALUES (?1, ?2, ?3)",
            params![
                format!("{}{:010}", entry_prefix(&self.id), *count),
                entry,
                chrono::Utc::now().timestamp_millis()
            ],
        )
        .map_err(|e| format!("Write the import record: {e}"))?;
        *count += 1;
        Ok(())
    }
}

/// The OAuth app slots the profiles `profiles` use: the provider's app is
/// read by every profile of it on the machine
/// ([`crate::bridge_commands::oauth_client_cred_key`]).
fn oauth_app_slots(profiles: &[serde_json::Value]) -> Vec<String> {
    let mut slots: Vec<String> = profiles
        .iter()
        .filter_map(|profile| profile.get("protocol").and_then(|p| p.as_str()))
        .filter_map(crate::bridge_commands::oauth_client_cred_key)
        .flat_map(|app| {
            ["client_id", "client_secret", "region"].map(|slot| format!("oauth_{app}_{slot}"))
        })
        .collect();
    slots.sort();
    slots.dedup();
    slots
}

/// A saved list of profiles adopts the OAuth apps its profiles use: the slots
/// any scan filled, which a settle would take back when the scan's own
/// profiles are not saved, are no longer the scan's to remove once a profile
/// of another scan's confirm, or of any save, uses the app. Called by every
/// writer of a profile list ([`crate::user_partitions`]), the one-time move
/// of a pre-partition install's list included
/// ([`crate::user_partitions::migrate_legacy_payloads`]: a scan with no
/// account yet keeps its ledger in the database before that move), inside
/// its transaction. Every other list writer holds the list lock a settle
/// holds too ([`settle_ledger`]), and a settle waits for the move, so a list
/// saved and a settle cannot interleave.
pub(crate) fn adopt_oauth_apps(
    tx: &Connection,
    profiles: &[serde_json::Value],
) -> Result<(), String> {
    let slots = oauth_app_slots(profiles);
    if slots.is_empty() {
        return Ok(());
    }
    let entries: Vec<(String, String)> = tx
        .prepare("SELECT key, value FROM global_state WHERE substr(key, 1, length(?1)) = ?1")
        .and_then(|mut statement| {
            statement
                .query_map(params![ENTRY_PREFIX], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|e| format!("Read the import records: {e}"))?;
    for (entry, value) in entries {
        let adopted = serde_json::from_str::<Written>(&value)
            .is_ok_and(|written| written.kind == Kind::OauthApp && slots.contains(&written.key));
        if adopted {
            tx.execute("DELETE FROM global_state WHERE key = ?1", params![entry])
                .map_err(|e| format!("Adopt an imported OAuth app: {e}"))?;
        }
    }
    Ok(())
}

/// The ids of `profiles`.
fn ids(profiles: &[serde_json::Value]) -> impl Iterator<Item = String> + '_ {
    profiles
        .iter()
        .filter_map(|profile| profile.get("id").and_then(|id| id.as_str()))
        .map(str::to_string)
}

/// The profile ids that may hold what a scan with no account wrote, to the
/// vault alone: the legacy list while no account exists; once accounts
/// exist, the legacy owner's list (the account the legacy vault went to)
/// and the legacy list too, which keeps more and never removes a profile
/// either of them lists. A legacy owner that cannot be read refuses, so the
/// settle is retried.
fn legacy_listed(store: &CredentialStore, conn: &Connection) -> Result<Vec<String>, String> {
    let mut listed: Vec<String> = match store.get("config_server_profiles") {
        Ok(raw) => ids(&serde_json::from_str::<Vec<serde_json::Value>>(&raw)
            .map_err(|e| format!("Read the saved profiles: {e}"))?)
        .collect(),
        Err(CredentialError::NotFound(_)) => Vec::new(),
        Err(e) => return Err(format!("Read the saved profiles: {e}")),
    };
    if let Some(owner) = crate::user_partitions::nonprofile_secret_owner_unpersisted(conn)? {
        let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
        let profiles = crate::user_partitions::list_server_profiles_for(conn, &root_key, owner)?;
        listed.extend(ids(&profiles));
    }
    Ok(listed)
}

/// Whether account `user` still exists. Account ids are never reused, so one
/// that does not names an account that was deleted.
fn exists(conn: &Connection, user: i64) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id = ?1)",
        params![user],
        |row| row.get(0),
    )
    .map_err(|e| format!("Read the import's account: {e}"))
}

/// Whose stores a settle reads the writes of a scan in.
#[derive(Clone, Copy)]
enum Owner {
    /// The account the scan wrote to: its row, and the vault key.
    Account(i64),
    /// No account (the scan began with none), or one deleted since, whose
    /// rows went with it: the vault key alone.
    VaultOnly,
}

/// What became of one write of a scan a settle looked at.
#[derive(Debug, PartialEq, Eq)]
enum Discarded {
    /// Removed, or no longer the scan's own to remove.
    Done,
    /// Left for a build that can remove it (a Proton session in a build
    /// without native Proton), its entry kept for that build's settle.
    #[cfg_attr(
        all(feature = "proton-native", not(test)),
        expect(
            dead_code,
            reason = "made only by a build without native Proton, and by its test stand-in"
        )
    )]
    Deferred,
}

/// Plays a build without native Proton (test only).
#[cfg(all(test, feature = "proton-native"))]
pub(crate) static PROTON_UNAVAILABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// A writer of an account's row: the store, the account and the key.
#[cfg(test)]
pub(crate) type RowWriter = fn(&CredentialStore, i64, &str);

/// Runs between the read of an account's row and its removal (test only):
/// a writer that saves the same key in between.
#[cfg(test)]
pub(crate) static BETWEEN_READ_AND_REMOVAL: Mutex<Option<RowWriter>> = Mutex::new(None);

/// Removes the vault key `key` while it still holds a value `ours` accepts.
fn discard_vault_key(
    store: &CredentialStore,
    key: &str,
    ours: impl Fn(&str) -> bool,
) -> Result<(), String> {
    match store.get(key) {
        Ok(value) if ours(&value) => {
            store
                .delete_secret_if_unchanged(key, &value)
                .map_err(|e| format!("Remove an imported secret: {e}"))?;
            Ok(())
        }
        Ok(_) | Err(CredentialError::NotFound(_)) => Ok(()),
        Err(e) => Err(format!("Read an imported secret: {e}")),
    }
}

/// Removes one write of a scan, where it still holds what the scan wrote.
fn discard(
    store: &CredentialStore,
    conn: &Connection,
    key: &[u8; 32],
    owner: Owner,
    written: &Written,
) -> Result<Discarded, String> {
    #[cfg(test)]
    if DISCARD_REFUSED.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("the removal was refused (test)".to_string());
    }
    match written.kind {
        Kind::Secret => {
            if let Owner::Account(user) = owner {
                let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
                let row = crate::user_partitions::get_user_credential_for(
                    conn,
                    &root_key,
                    user,
                    &written.key,
                )?;
                #[cfg(test)]
                if let Some(between) = *BETWEEN_READ_AND_REMOVAL.lock().unwrap() {
                    between(store, user, &written.key);
                }
                if let Some(row) = row.filter(|row| mac(key, row) == written.mac) {
                    // Compared again in the transaction of the removal: a
                    // value saved since the read stays.
                    crate::user_partitions::swap_user_credential_if(
                        conn,
                        &root_key,
                        user,
                        &written.key,
                        "",
                        Some(&row),
                        None,
                    )?;
                }
            }
            discard_vault_key(store, &written.key, |value| mac(key, value) == written.mac)?;
            Ok(Discarded::Done)
        }
        Kind::ProtonSession => discard_proton_session(store, key, owner, written),
        Kind::OauthApp => {
            discard_vault_key(store, &written.key, |value| mac(key, value) == written.mac)?;
            Ok(Discarded::Done)
        }
    }
}

/// [`discard`] of a Proton session: removed through the session's own locks
/// for an account; for no account, or one deleted, the vault key alone (the
/// session's row, if it had one, went with the account).
#[cfg(feature = "proton-native")]
fn discard_proton_session(
    store: &CredentialStore,
    key: &[u8; 32],
    owner: Owner,
    written: &Written,
) -> Result<Discarded, String> {
    use crate::providers::proton_native::vault;
    #[cfg(test)]
    if PROTON_UNAVAILABLE.load(std::sync::atomic::Ordering::SeqCst) {
        return Ok(Discarded::Deferred);
    }
    let ours = |uid: &str| mac(key, uid) == written.mac;
    match owner {
        Owner::Account(user) => vault::discard_imported_session_blocking(
            vault::SessionOwner::for_user(Some(user)),
            &written.profile,
            ours,
        )
        .map(|_| Discarded::Done)
        .map_err(|e| e.to_string()),
        Owner::VaultOnly => {
            discard_vault_key(store, &vault::session_key(Some(&written.profile)), |raw| {
                vault::session_uid(raw).is_ok_and(|uid| ours(&uid))
            })?;
            Ok(Discarded::Done)
        }
    }
}

/// [`discard`] of a Proton session in a build without native Proton: it
/// cannot be read here, so its entry waits for a build that can.
#[cfg(not(feature = "proton-native"))]
fn discard_proton_session(
    _store: &CredentialStore,
    _key: &[u8; 32],
    _owner: Owner,
    _written: &Written,
) -> Result<Discarded, String> {
    Ok(Discarded::Deferred)
}

/// Whether a scan other than `id` still records `written`, the same value
/// under the same key, for the same owner: an OAuth app's slot is the
/// machine's, whoever the scan stored for; any other write is the account's,
/// `user` (the header of `id`). That scan has not settled, so it may still be
/// confirmed and needs the value; its own settle takes it back when its
/// profiles are not saved. Read under the list lock ([`settle_ledger`]).
fn claimed_by_another_scan(
    conn: &Connection,
    id: &str,
    user: Option<i64>,
    written: &Written,
) -> Result<bool, String> {
    let own = entry_prefix(id);
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT key, value FROM global_state WHERE substr(key, 1, length(?1)) = ?1")
        .and_then(|mut statement| {
            statement
                .query_map(params![ENTRY_PREFIX], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|e| format!("Read the import records: {e}"))?;
    for (entry, value) in rows {
        if entry.starts_with(&own) {
            continue;
        }
        let Ok(other) = serde_json::from_str::<Written>(&value) else {
            continue;
        };
        if other.kind != written.kind || other.key != written.key || other.mac != written.mac {
            continue;
        }
        if written.kind == Kind::OauthApp {
            return Ok(true);
        }
        // The scan of this entry, by its header: its id is what follows the
        // prefix, up to the entry's number.
        let Some(scan) = entry
            .strip_prefix(ENTRY_PREFIX)
            .and_then(|rest| rest.rsplit_once('_'))
            .map(|(scan, _)| scan)
        else {
            continue;
        };
        let header: Option<String> = conn
            .query_row(
                "SELECT value FROM global_state WHERE key = ?1",
                params![format!("{LEDGER_PREFIX}{scan}")],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Read the import record: {e}"))?;
        let same_owner = header
            .and_then(|header| serde_json::from_str::<Header>(&header).ok())
            .is_some_and(|header| header.user == user);
        if same_owner {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Settles the scan `id`: every write for a profile its account does not
/// list goes, and the ledger with it. A write that could not be removed
/// stays in the ledger for the next settle, and the error says so; one only
/// a build with native Proton can remove stays too, without an error.
/// Whatever the outcome, this process no longer holds the scan: every
/// caller is done with it, so a ledger left behind is settled again as an
/// abandoned one.
pub(crate) fn settle(store: &CredentialStore, id: &str) -> Result<(), String> {
    let settled = settle_ledger(store, id);
    release(id);
    settled
}

/// Lets go of the scan `id` a window is done with when it cannot be settled
/// now (the vault is locked): its ledger stays, and the next scan or opening
/// settles it as an abandoned one.
pub(crate) fn give_up(id: &str) {
    release(id);
}

/// Refuses every removal of a settle (test only), as a store that fails.
#[cfg(test)]
pub(crate) static DISCARD_REFUSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Runs once in the next [`settle_ledger`], right after it took the list
/// lock and before its first read (test only).
#[cfg(test)]
#[allow(clippy::type_complexity)]
pub(crate) static WHILE_SETTLING: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

/// Runs once in the next [`settle_ledger`], after its removals and right
/// before the transaction that drops its records (test only): the other end
/// of the span the list lock covers.
#[cfg(test)]
#[allow(clippy::type_complexity)]
pub(crate) static BEFORE_SETTLE_COMMIT: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

/// The body of [`settle`].
fn settle_ledger(store: &CredentialStore, id: &str) -> Result<(), String> {
    let conn = crate::user_partitions::open_or_init_cli()?;
    // From the first read to the last write: a list saved meanwhile would
    // adopt what this settle takes back ([`adopt_oauth_apps`]), or list a
    // profile whose write it removes, and another scan's fill of an app
    // records its share of it ([`Scan::share_app`]); both hold this lock. On a
    // filesystem without file locks it excludes nobody, and the settle goes
    // on as an ordinary save does.
    let _lock = crate::user_partitions::ProfileListLock::acquire(&conn)?;
    #[cfg(test)]
    if let Some(hook) = WHILE_SETTLING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        hook();
    }
    // A database the legacy move has not run on yet (a scan with no account
    // keeps its ledger there): the move inserts a list without the list lock,
    // so this settle could not tell a profile it moves meanwhile. Nothing is
    // settled until it ran, and the ledger waits for the next settle.
    if !crate::user_partitions::legacy_payloads_moved(&conn)? {
        return Ok(());
    }
    let header_key = format!("{LEDGER_PREFIX}{id}");
    let header: Option<String> = conn
        .query_row(
            "SELECT value FROM global_state WHERE key = ?1",
            params![header_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Read the import record: {e}"))?;
    let Some(header) = header else {
        return Ok(());
    };
    let header: Header =
        serde_json::from_str(&header).map_err(|e| format!("Read the import record: {e}"))?;
    let prefix = entry_prefix(id);
    let entries: Vec<(String, Written)> = conn
        .prepare(
            "SELECT key, value FROM global_state WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key",
        )
        .and_then(|mut statement| {
            statement
                .query_map(params![prefix], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|e| format!("Read the import record: {e}"))?
        .into_iter()
        .map(|(key, value)| {
            serde_json::from_str(&value)
                .map(|written| (key, written))
                .map_err(|e| format!("Read the import record: {e}"))
        })
        .collect::<Result<_, _>>()?;
    let (owner, listed) = match header.user {
        Some(user) if exists(&conn, user)? => {
            let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
            let profiles =
                crate::user_partitions::list_server_profiles_for(&conn, &root_key, user)?;
            (Owner::Account(user), ids(&profiles).collect())
        }
        // The account was deleted: its rows went with it, and what stays is
        // in the vault, which the legacy owner reads.
        Some(_) | None => (Owner::VaultOnly, legacy_listed(store, &conn)?),
    };
    let key = mac_key(store);
    let mut settled = Vec::new();
    let mut kept = false;
    let mut errors = Vec::new();
    for (entry, written) in entries {
        // Another scan that has not settled needs the same value: its settle
        // decides, and this record goes.
        if written.kept_by(&listed) || claimed_by_another_scan(&conn, id, header.user, &written)? {
            settled.push(entry);
            continue;
        }
        match discard(store, &conn, &key, owner, &written) {
            Ok(Discarded::Done) => settled.push(entry),
            Ok(Discarded::Deferred) => kept = true,
            Err(error) => {
                errors.push(format!("{}: {error}", written.key));
                kept = true;
            }
        }
    }
    #[cfg(test)]
    if let Some(hook) = BEFORE_SETTLE_COMMIT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        hook();
    }
    let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Remove the import record: {e}"))?;
    for entry in &settled {
        tx.execute("DELETE FROM global_state WHERE key = ?1", params![entry])
            .map_err(|e| format!("Remove the import record: {e}"))?;
    }
    if !kept {
        tx.execute(
            "DELETE FROM global_state WHERE key = ?1",
            params![header_key],
        )
        .map_err(|e| format!("Remove the import record: {e}"))?;
    }
    tx.commit()
        .map_err(|e| format!("Remove the import record: {e}"))?;
    if errors.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Some secrets of an import that was not saved could not be removed yet: {}",
        errors.join("; ")
    ))
}

/// Settles every scan no live window holds: one whose window went with a
/// crash or a quit. Best effort, logged.
pub(crate) fn settle_abandoned(store: &CredentialStore) {
    let ids: Vec<String> = match crate::user_partitions::open_or_init_cli().and_then(|conn| {
        conn.prepare(
            "SELECT key FROM global_state WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key",
        )
        .and_then(|mut statement| {
            statement
                .query_map(params![LEDGER_PREFIX], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|e| format!("List the import records: {e}"))
    }) {
        Ok(keys) => keys
            .iter()
            .filter_map(|key| key.strip_prefix(LEDGER_PREFIX))
            .map(str::to_string)
            .collect(),
        Err(error) => {
            log::warn!("Bridge import: {error}");
            return;
        }
    };
    for id in ids.iter().filter(|id| !held(id)) {
        if let Err(error) = settle(store, id) {
            log::warn!("Bridge import: {error}");
        }
    }
}

/// Lets go of the scans this process holds, as a process that ended does
/// (test only).
#[cfg(all(test, feature = "proton-native"))]
pub(crate) fn forget_live_scans() {
    LIVE.lock().unwrap().clear();
}
