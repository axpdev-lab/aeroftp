// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! What one bridge scan (`import_bridge_config`) wrote before its profiles
//! were saved, so nothing it wrote outlives a scan the window never
//! confirmed.
//!
//! A scan stores each profile's secrets (passwords, OAuth and Jottacloud
//! tokens, native Proton sessions, crypt and Filen keys) when it reads the
//! file; the profiles are saved later, by the confirm. Every write is
//! recorded first, in a ledger kept where every process reads it, by key and
//! by a keyed MAC of what was written (never the secret). Settling the scan
//! (the window confirmed it, closed it or scanned again) removes what it
//! wrote for every profile the account does not list, and only while it is
//! still what the scan wrote: a value saved since, or a profile the confirm
//! saved, stays. A ledger no live scan holds (the window went with a crash or
//! a quit, and its process with it) is settled at the next scan or the next
//! opening of the bridge panel. A live scan holds a file lock of its own, so a
//! scan of another process is never taken for one whose window is gone.

use crate::credential_store::{CredentialError, CredentialStore};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::Mutex;
use zeroize::Zeroizing;

/// Prefix of a scan's ledger in `global_state`.
const LEDGER_PREFIX: &str = "bridge_import_scan_";

/// The scans of this process no window has settled yet, each with the lock
/// file it holds while it lives (`None` on a filesystem without file locks).
static LIVE: Mutex<Vec<(String, Option<std::fs::File>)>> = Mutex::new(Vec::new());

/// The lock file a live scan `id` holds, beside the account database.
fn lock_path(id: &str) -> Result<std::path::PathBuf, String> {
    let db = crate::user_partitions::cli_db_path()?;
    let dir = db
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("bridge-import-scans");
    Ok(dir.join(format!("{id}.lock")))
}

/// Takes the lock of the new scan `id`: `None` where the filesystem has no
/// file locks, and the scan is known live to this process only.
fn hold(id: &str) -> Result<Option<std::fs::File>, String> {
    let path = lock_path(id)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Create the import lock folder: {e}"))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("Open the import lock: {e}"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::Error(e))
            if crate::user_partitions::file_locks_unavailable(&e) =>
        {
            Ok(None)
        }
        Err(std::fs::TryLockError::Error(e)) => Err(format!("Take the import lock: {e}")),
        Err(std::fs::TryLockError::WouldBlock) => Err("Take the import lock: it is held".into()),
    }
}

/// Whether a live scan holds the ledger `id`: one of this process, or the lock
/// of one of another. A lock that cannot be read counts as held, so a scan is
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
    match file.try_lock() {
        Ok(()) => false,
        Err(std::fs::TryLockError::Error(e)) => !crate::user_partitions::file_locks_unavailable(&e),
        Err(std::fs::TryLockError::WouldBlock) => true,
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

#[derive(Serialize, Deserialize, Default)]
struct Ledger {
    user: Option<i64>,
    written: Vec<Written>,
}

/// One write of a scan: the profile it is for, its key, and the MAC of the
/// secret (of the session UID for a Proton session).
#[derive(Serialize, Deserialize, Clone)]
struct Written {
    profile: String,
    key: String,
    kind: Kind,
    mac: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    /// A secret of the dual writers: the vault key and the account's row.
    Secret,
    /// A native Proton Drive session saved for the profile.
    ProtonSession,
}

/// A scan in progress: the account its secrets go to and the profiles it read.
pub(crate) struct Scan {
    id: String,
    user: Option<i64>,
    profiles: Vec<String>,
    mac_key: Zeroizing<[u8; 32]>,
    written: Mutex<Vec<Written>>,
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

fn persist(id: &str, ledger: &Ledger) -> Result<(), String> {
    let value =
        serde_json::to_string(ledger).map_err(|e| format!("Encode the import record: {e}"))?;
    crate::user_partitions::open_or_init_cli()?
        .execute(
            "INSERT INTO global_state(key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![
                format!("{LEDGER_PREFIX}{id}"),
                value,
                chrono::Utc::now().timestamp_millis()
            ],
        )
        .map_err(|e| format!("Write the import record: {e}"))?;
    Ok(())
}

impl Scan {
    /// Begins a scan whose secrets go to `user`, for the profiles `profiles`
    /// of the file, its empty ledger written before any secret is.
    pub(crate) fn begin(
        store: &CredentialStore,
        user: Option<i64>,
        profiles: Vec<String>,
    ) -> Result<Self, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let lock = hold(&id)?;
        if let Err(error) = persist(
            &id,
            &Ledger {
                user,
                written: Vec::new(),
            },
        ) {
            drop(lock);
            release(&id);
            return Err(error);
        }
        LIVE.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((id.clone(), lock));
        Ok(Self {
            id,
            user,
            profiles,
            mac_key: mac_key(store),
            written: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// The profile of the file that `key` belongs to: the longest id the key
    /// ends with, after an underscore.
    fn profile_of(&self, key: &str) -> String {
        self.profiles
            .iter()
            .filter(|id| {
                !id.is_empty()
                    && key
                        .strip_suffix(id.as_str())
                        .is_some_and(|head| head.ends_with('_'))
            })
            .max_by_key(|id| id.len())
            .cloned()
            .unwrap_or_default()
    }

    /// Records, before the write, that the scan writes `value` (a secret, or
    /// a Proton session's UID) under `key`. A record that cannot be kept
    /// refuses the write, so nothing is written the scan could not remove.
    pub(crate) fn record(&self, key: &str, kind: Kind, value: &str) -> Result<(), String> {
        self.record_for(&self.profile_of(key), key, kind, value)
    }

    /// [`Scan::record`] for a write whose profile is known: `profile`.
    pub(crate) fn record_for(
        &self,
        profile: &str,
        key: &str,
        kind: Kind,
        value: &str,
    ) -> Result<(), String> {
        let mut written = self
            .written
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        written.push(Written {
            profile: profile.to_string(),
            key: key.to_string(),
            kind,
            mac: mac(&self.mac_key, value),
        });
        let ledger = Ledger {
            user: self.user,
            written: written.clone(),
        };
        persist(&self.id, &ledger).inspect_err(|_| {
            written.pop();
        })
    }
}

/// The profile ids `user` lists now. `None`, an install with no account,
/// reads the list every reader of it reads.
fn listed(store: &CredentialStore, user: Option<i64>) -> Result<Vec<String>, String> {
    let profiles = match user {
        Some(user) => {
            let conn = crate::user_partitions::open_or_init_cli()?;
            let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
            crate::user_partitions::list_server_profiles_for(&conn, &root_key, user)?
        }
        None => crate::user_partitions::mcp_list_active_server_profiles(store)?,
    };
    Ok(profiles
        .iter()
        .filter_map(|profile| profile.get("id").and_then(|id| id.as_str()))
        .map(str::to_string)
        .collect())
}

/// Removes one write of a scan, where it still holds what the scan wrote.
fn discard(
    store: &CredentialStore,
    key: &[u8; 32],
    user: Option<i64>,
    written: &Written,
) -> Result<(), String> {
    #[cfg(test)]
    if DISCARD_REFUSED.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("the removal was refused (test)".to_string());
    }
    match written.kind {
        Kind::Secret => {
            if let Some(user) = user {
                let conn = crate::user_partitions::open_or_init_cli()?;
                let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
                let row = crate::user_partitions::get_user_credential_for(
                    &conn,
                    &root_key,
                    user,
                    &written.key,
                )?;
                if row.is_some_and(|row| mac(key, &row) == written.mac) {
                    crate::user_partitions::delete_user_credential_for(&conn, user, &written.key)?;
                }
            }
            match store.get(&written.key) {
                Ok(value) if mac(key, &value) == written.mac => {
                    store
                        .delete_secret_if_unchanged(&written.key, &value)
                        .map_err(|e| format!("Remove an imported secret: {e}"))?;
                }
                Ok(_) | Err(CredentialError::NotFound(_)) => {}
                Err(e) => return Err(format!("Read an imported secret: {e}")),
            }
            Ok(())
        }
        Kind::ProtonSession => {
            #[cfg(feature = "proton-native")]
            {
                use crate::providers::proton_native::vault;
                vault::discard_imported_session_blocking(
                    vault::SessionOwner::for_user(user),
                    &written.profile,
                    |uid| mac(key, uid) == written.mac,
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
            }
            #[cfg(not(feature = "proton-native"))]
            Ok(())
        }
    }
}

/// Settles the scan `id`: every write for a profile its account does not
/// list goes, and the ledger with it. A write that could not be removed
/// stays in the ledger for the next settle, and the error says so. Whatever
/// the outcome, this process no longer holds the scan: every caller is done
/// with it, so a ledger left behind is settled again as an abandoned one.
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

/// The body of [`settle`].
fn settle_ledger(store: &CredentialStore, id: &str) -> Result<(), String> {
    let conn = crate::user_partitions::open_or_init_cli()?;
    let ledger_key = format!("{LEDGER_PREFIX}{id}");
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM global_state WHERE key = ?1",
            params![ledger_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Read the import record: {e}"))?;
    let Some(value) = value else {
        return Ok(());
    };
    let ledger: Ledger =
        serde_json::from_str(&value).map_err(|e| format!("Read the import record: {e}"))?;
    let listed = listed(store, ledger.user)?;
    let key = mac_key(store);
    let mut kept = Vec::new();
    let mut errors = Vec::new();
    for written in ledger.written {
        if listed.contains(&written.profile) {
            continue;
        }
        if let Err(error) = discard(store, &key, ledger.user, &written) {
            errors.push(format!("{}: {error}", written.key));
            kept.push(written);
        }
    }
    if errors.is_empty() {
        conn.execute(
            "DELETE FROM global_state WHERE key = ?1",
            params![ledger_key],
        )
        .map_err(|e| format!("Remove the import record: {e}"))?;
        return Ok(());
    }
    persist(
        id,
        &Ledger {
            user: ledger.user,
            written: kept,
        },
    )?;
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
