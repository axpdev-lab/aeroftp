//! Multi-user partition metadata store.
//!
//! `vault.db` remains the existing encrypted JSON credential vault. This
//! module owns the additive SQLite database used to partition profiles and
//! per-user settings without changing the legacy vault format in-place.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::credential_store::{CredentialError, CredentialStore};
use crate::profile_auth_state::oauth_vault_key_for_protocol;
use crate::storage_dedup::{dedup_key, normalize_host, normalize_user, ProfileView};
use crate::user_crypto::{self, SecretKey};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use secrecy::zeroize::{Zeroize, Zeroizing};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
static SESSION_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
use tauri::AppHandle;

#[cfg(all(test, feature = "proton-native"))]
#[path = "native_relocation_tests.rs"]
mod native_relocation_tests;

pub(crate) const DB_FILENAME: &str = "user_partitions.db";
const SCHEMA_VERSION: &str = "5";
const LEGACY_PROFILES_KEY: &str = "__legacy_server_profiles";
/// Vault account of the single-user profile list that predates partitions.
/// Still read by several subsystems, so it mirrors the active user.
const LEGACY_PROFILES_BLOB: &str = "config_server_profiles";
const LEGACY_SETTINGS_KEY: &str = "__legacy_settings";
const ACTIVE_USER_KEY: &str = "active_user_id";
const LEGACY_OWNER_KEY: &str = "legacy_credential_owner_id";
const SCHEMA_VERSION_KEY: &str = "schema_version";
const DEFAULT_USER_NAME: &str = "default";
const DEFAULT_USER_AVATAR: &str = "D";
const DEFAULT_USER_COLOR: &str = "#3b82f6";
const LEGACY_SETTINGS_SCOPE: &str = "legacy_app_settings";
const LOCKOUT_THRESHOLD: u32 = 5;
const LOCKOUT_BACKOFF_MS: [i64; 5] = [30_000, 60_000, 300_000, 900_000, 3_600_000];

static USER_SESSION: Mutex<Option<UserSession>> = Mutex::new(None);

struct UserSession {
    user_id: i64,
    dek: SecretKey,
}

thread_local! {
    /// The session as this thread found it when it took [`USER_SESSION`], for
    /// as long as it holds it ([`SessionHold`]); `Some(None)` is held with no
    /// account unlocked. The lock is not reentrant: a partition call made under
    /// it (an MCP OAuth freshness check inside a token write) reads the session
    /// from here, where taking the lock again would wait on its own thread.
    static HELD_SESSION: RefCell<Option<Option<(i64, SecretKey)>>> = const { RefCell::new(None) };
}

/// [`USER_SESSION`], held by this thread until dropped. A writer takes it
/// before SQLite's write lock, the order [`with_user_dek`] callers take them in:
/// a writer that held the database's reservation and then waited on the
/// session would leave a thread holding the session and waiting on the
/// database to fail its write when the busy timeout runs out. A hold taken
/// while this thread already holds the session is empty.
pub(crate) struct SessionHold(Option<MutexGuard<'static, Option<UserSession>>>);

impl Drop for SessionHold {
    fn drop(&mut self) {
        if self.0.is_some() {
            HELD_SESSION.set(None);
        }
    }
}

fn hold_session() -> Result<SessionHold, String> {
    if HELD_SESSION.with_borrow(Option::is_some) {
        return Ok(SessionHold(None));
    }
    let guard = USER_SESSION
        .lock()
        .map_err(|_| "USER_SESSION_LOCK_POISONED".to_string())?;
    let session = guard.as_ref().map(|session| {
        let dek = user_crypto::secret_key_from_bytes(session.dek.expose_secret());
        (session.user_id, dek)
    });
    HELD_SESSION.set(Some(session));
    Ok(SessionHold(Some(guard)))
}

/// Hold the session before opening a transaction that goes through `user_id`'s
/// partition. Taken for an account with a passphrase, the kind whose partition
/// calls take the session. When `user_id` cannot be read, or the lock is
/// poisoned, the hold is empty and those calls answer as they would without it.
pub(crate) fn hold_user_session(conn: &Connection, user_id: i64) -> SessionHold {
    match read_user_key_row(conn, user_id) {
        Ok(row) if row.has_passphrase => hold_session().unwrap_or(SessionHold(None)),
        _ => SessionHold(None),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMetadata {
    pub id: i64,
    pub name: String,
    pub avatar_emoji: Option<String>,
    pub avatar_color: Option<String>,
    pub has_passphrase: bool,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_unlocked_at: Option<i64>,
    pub is_active: bool,
    /// Admin role: can edit/rename/reset-passphrase/delete OTHER users
    /// from inside Manage Users. The first user created by the legacy
    /// migration (lowest id) is seeded as admin so an existing single
    /// install upgraded to MU keeps full control over the new account
    /// surface. There must always be at least one admin in the table.
    pub is_admin: bool,
    /// Default / favourite user: the account auto-unlocked on launch (a
    /// single-winner flag, so at most one user is default at a time). Set from
    /// the CLI `users -i` Fav verb or the GUI Manage Users star, and honoured by
    /// the boot account selection (`decideBootAccountAction`). Only password-free
    /// accounts can be default, since a protected account always shows its
    /// prompt rather than auto-unlocking. Per Ehud #311 (D1).
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationReport {
    pub schema_version: String,
    pub created_default_user: bool,
    pub migrated_profiles: usize,
    pub migrated_settings_scopes: usize,
    pub already_migrated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserStorageStats {
    pub user_id: i64,
    pub profile_count: i64,
    pub settings_count: i64,
    pub encrypted_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserUnlockStatus {
    pub active_user_id: Option<i64>,
    pub unlocked_user_id: Option<i64>,
    pub is_unlocked: bool,
    #[serde(default)]
    pub security_reset: Vec<crate::profile_security::ProfileReset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct UserLockoutState {
    fail_count: u32,
    unlock_at_epoch_ms: Option<i64>,
}

/// MU-7 cross-user dedup match. Only metadata of the OTHER user is returned:
/// the encrypted profile blob and the dedup_key itself remain in the database
/// and are never echoed to the caller. Frontend uses this to surface a
/// "this server is already saved in account X" warning without ever needing
/// to decrypt the other partition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossUserDedupMatch {
    pub user_id: i64,
    pub user_name: String,
    pub user_avatar_emoji: Option<String>,
    pub user_avatar_color: Option<String>,
}

pub fn db_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(crate::portable::app_config_dir(app)?.join(DB_FILENAME))
}

fn open_or_init_path(path: PathBuf) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create user partitions dir: {e}"))?;
    }
    let conn = Connection::open(&path).map_err(|e| format!("Open user partitions DB: {e}"))?;
    init_db_schema(&conn)?;
    Ok(conn)
}

/// Opens a partition database a keystore import writes, for a writer: the
/// copy a restore wrote whole (its staged copy, or the preview's copy of the
/// backup's), or the live one when the backup publishes none, which an
/// import may be the first open of since an upgrade. Its schema is first
/// brought to this build's ([`init_db_schema`]) as [`open_or_init_path`]
/// brings the live database's. A backup made by an
/// older build lacks what this build's writers name (the
/// `server_profiles.token_uid` column, the sign-in stamp table), and the
/// first write of a profile list would fail on it. The schema step adds
/// columns, tables, indexes and triggers only, and seeds the first admin
/// where it adds that column, as the live database's first open would; the
/// columns stay in the file, so every later open of it finds them.
pub(crate) fn open_restored_db(db_path: &std::path::Path) -> Result<Connection, String> {
    let conn = Connection::open(db_path).map_err(|e| format!("Open user partitions DB: {e}"))?;
    init_db_schema(&conn)?;
    Ok(conn)
}

pub fn open_or_init(app: &AppHandle) -> Result<Connection, String> {
    open_or_init_path(db_path(app)?)
}

pub fn cli_db_path() -> Result<PathBuf, String> {
    let base = crate::portable::cli_app_config_dir()
        .ok_or_else(|| "Cannot resolve AeroFTP CLI config directory".to_string())?;
    Ok(base.join(DB_FILENAME))
}

pub fn open_or_init_cli() -> Result<Connection, String> {
    open_or_init_path(cli_db_path()?)
}

pub fn init_empty_db(app: &AppHandle) -> Result<(), String> {
    let _conn = open_or_init(app)?;
    Ok(())
}

/// Whether `error` is SQLite refusing to add a column that is already there:
/// another connection (the GUI and the CLI opening the database at once)
/// added it between this one's check and its `ALTER TABLE`.
fn is_duplicate_column(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(_, Some(message))
            if message.starts_with("duplicate column name")
    )
}

#[cfg(test)]
thread_local! {
    /// Runs once on this thread in the next column migration, between its
    /// check and its `ALTER TABLE`: another connection adding the column.
    static BEFORE_COLUMN_ADD: DualWriteHook = const { RefCell::new(None) };
}

fn ensure_users_is_admin_column(conn: &Connection) -> Result<(), String> {
    let mut stmt = match conn.prepare("PRAGMA table_info(users)") {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let mut table_exists = false;
    let mut has_is_admin = false;
    while let Some(row) = rows
        .next()
        .map_err(|e| format!("ensure_users_is_admin_column row: {e}"))?
    {
        table_exists = true;
        let name: String = row.get(1).map_err(|e| format!("read column name: {e}"))?;
        if name == "is_admin" {
            has_is_admin = true;
            break;
        }
    }
    if table_exists && !has_is_admin {
        #[cfg(test)]
        run_dual_write_hook(&BEFORE_COLUMN_ADD);
        match conn.execute(
            "ALTER TABLE users ADD COLUMN is_admin INTEGER NOT NULL DEFAULT 0",
            [],
        ) {
            Ok(_) => {}
            // Added since the check by the connection that won, which seeds
            // the first admin.
            Err(e) if is_duplicate_column(&e) => return Ok(()),
            Err(e) => return Err(format!("ALTER TABLE users ADD is_admin: {e}")),
        }
        conn.execute(
            "UPDATE users SET is_admin = 1
             WHERE id = (SELECT id FROM users ORDER BY id ASC LIMIT 1)",
            [],
        )
        .map_err(|e| format!("Seed first user as admin: {e}"))?;
    }
    Ok(())
}

/// Idempotent migration adding the `is_default` column to an existing `users`
/// table. The default/favourite user is the account auto-unlocked on launch
/// (CLI `users -i` Fav, GUI Manage Users star); it is a single-winner flag, so
/// no user starts as default after the migration (the GUI/CLI set it on
/// demand). Mirrors [`ensure_users_is_admin_column`]: a no-op on fresh installs
/// (the CREATE TABLE below already carries the column) and on already-migrated
/// databases. Per Ehud #311 (D1).
fn ensure_users_is_default_column(conn: &Connection) -> Result<(), String> {
    let mut stmt = match conn.prepare("PRAGMA table_info(users)") {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let mut table_exists = false;
    let mut has_is_default = false;
    while let Some(row) = rows
        .next()
        .map_err(|e| format!("ensure_users_is_default_column row: {e}"))?
    {
        table_exists = true;
        let name: String = row.get(1).map_err(|e| format!("read column name: {e}"))?;
        if name == "is_default" {
            has_is_default = true;
            break;
        }
    }
    if table_exists && !has_is_default {
        #[cfg(test)]
        run_dual_write_hook(&BEFORE_COLUMN_ADD);
        match conn.execute(
            "ALTER TABLE users ADD COLUMN is_default INTEGER NOT NULL DEFAULT 0",
            [],
        ) {
            // Or added by another connection since the check above.
            Ok(_) => {}
            Err(e) if is_duplicate_column(&e) => {}
            Err(e) => return Err(format!("ALTER TABLE users ADD is_default: {e}")),
        }
    }
    Ok(())
}

/// Idempotent migration adding the `token_uid` column to an existing
/// `server_profiles` table ([`profile_token_tag`]). Rows written before it
/// hold none until [`fill_profile_token_tags`] gives them theirs, once their
/// account's DEK is reachable, or their list is written again; a no-op on
/// fresh installs, whose CREATE TABLE below carries the column.
fn ensure_server_profiles_token_uid_column(conn: &Connection) -> Result<(), String> {
    let mut stmt = match conn.prepare("PRAGMA table_info(server_profiles)") {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| format!("ensure_server_profiles_token_uid_column: {e}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("read column name: {e}"))?;
    if !columns.is_empty() && !columns.iter().any(|name| name == "token_uid") {
        #[cfg(test)]
        run_dual_write_hook(&BEFORE_COLUMN_ADD);
        match conn.execute("ALTER TABLE server_profiles ADD COLUMN token_uid TEXT", []) {
            // Or added by another connection since the check above.
            Ok(_) => {}
            Err(e) if is_duplicate_column(&e) => {}
            Err(e) => return Err(format!("ALTER TABLE server_profiles ADD token_uid: {e}")),
        }
    }
    Ok(())
}

pub fn init_db_schema(conn: &Connection) -> Result<(), String> {
    // Idempotent schema migration: if a pre-v3 `users` table already exists
    // without the is_admin column, add it before CREATE TABLE IF NOT EXISTS
    // becomes a no-op. Safe to run on fresh installs (PRAGMA returns 0 rows
    // until CREATE TABLE runs, so the check just skips).
    ensure_users_is_admin_column(conn)?;
    ensure_users_is_default_column(conn)?;
    ensure_server_profiles_token_uid_column(conn)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;

         CREATE TABLE IF NOT EXISTS users (
             id                INTEGER PRIMARY KEY AUTOINCREMENT,
             name              TEXT NOT NULL,
             name_canonical    TEXT NOT NULL UNIQUE,
             avatar_emoji      TEXT,
             avatar_color      TEXT,
             has_passphrase    INTEGER NOT NULL DEFAULT 0 CHECK (has_passphrase IN (0, 1)),
             kdf_salt          BLOB,
             kdf_params        TEXT,
             wrapped_dek       BLOB NOT NULL,
             dek_verifier      BLOB NOT NULL,
             sort_order        INTEGER NOT NULL DEFAULT 0,
             created_at        INTEGER NOT NULL,
             updated_at        INTEGER NOT NULL,
             last_unlocked_at  INTEGER,
             is_admin          INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0, 1)),
             is_default        INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
             CHECK (
                 (has_passphrase = 0 AND kdf_salt IS NULL AND kdf_params IS NULL)
                 OR
                 (has_passphrase = 1 AND kdf_salt IS NOT NULL AND kdf_params IS NOT NULL)
             )
         );

         CREATE INDEX IF NOT EXISTS idx_users_sort
             ON users(sort_order, name_canonical);

         CREATE TABLE IF NOT EXISTS server_profiles (
             id              INTEGER PRIMARY KEY AUTOINCREMENT,
             user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
             profile_uid     TEXT NOT NULL,
             dedup_key       TEXT NOT NULL,
             name            TEXT NOT NULL,
             encrypted_blob  BLOB NOT NULL,
             nonce           BLOB NOT NULL,
             aead_alg        TEXT NOT NULL DEFAULT 'aes-256-gcm',
             created_at      INTEGER NOT NULL,
             updated_at      INTEGER NOT NULL,
             token_uid       TEXT,
             UNIQUE(user_id, profile_uid)
         );

         CREATE INDEX IF NOT EXISTS idx_profiles_user
             ON server_profiles(user_id);

         CREATE INDEX IF NOT EXISTS idx_profiles_token_uid
             ON server_profiles(token_uid);

         CREATE INDEX IF NOT EXISTS idx_profiles_user_name
             ON server_profiles(user_id, name COLLATE NOCASE);

         CREATE INDEX IF NOT EXISTS idx_profiles_dedup
             ON server_profiles(dedup_key);

         CREATE TABLE IF NOT EXISTS user_settings (
             user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
             scope           TEXT NOT NULL,
             encrypted_blob  BLOB NOT NULL,
             nonce           BLOB NOT NULL,
             aead_alg        TEXT NOT NULL DEFAULT 'aes-256-gcm',
             updated_at      INTEGER NOT NULL,
             PRIMARY KEY(user_id, scope)
         );

         CREATE TABLE IF NOT EXISTS user_credentials (
             user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
             credential_id   TEXT NOT NULL,
             credential_type TEXT NOT NULL,
             encrypted_blob  BLOB NOT NULL,
             nonce           BLOB NOT NULL,
             aead_alg        TEXT NOT NULL DEFAULT 'aes-256-gcm',
             updated_at      INTEGER NOT NULL,
             PRIMARY KEY(user_id, credential_id)
         );

         CREATE INDEX IF NOT EXISTS idx_user_credentials_type
             ON user_credentials(user_id, credential_type);

         CREATE TABLE IF NOT EXISTS global_state (
             key             TEXT PRIMARY KEY,
             value           TEXT NOT NULL,
             updated_at      INTEGER NOT NULL
         );

         CREATE TABLE IF NOT EXISTS peer_identity (
             user_id        INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
             encrypted_blob BLOB NOT NULL,
             nonce          BLOB NOT NULL,
             aead_alg       TEXT NOT NULL DEFAULT 'aes-256-gcm',
             public_id      TEXT NOT NULL,
             created_at     INTEGER NOT NULL,
             updated_at     INTEGER NOT NULL
         );

         CREATE TABLE IF NOT EXISTS peer_contacts (
             user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
             contact_id     TEXT NOT NULL,
             alias          TEXT NOT NULL,
             added_at       INTEGER NOT NULL,
             PRIMARY KEY(user_id, contact_id)
         );

         CREATE TABLE IF NOT EXISTS peer_drives (
             user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
             namespace_id   TEXT NOT NULL,
             role           TEXT NOT NULL,
             encrypted_blob BLOB NOT NULL,
             nonce          BLOB NOT NULL,
             aead_alg       TEXT NOT NULL DEFAULT 'aes-256-gcm',
             created_at     INTEGER NOT NULL,
             updated_at     INTEGER NOT NULL,
             PRIMARY KEY(user_id, namespace_id)
         );

         CREATE TABLE IF NOT EXISTS peer_mutes (
             user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
             contact_id     TEXT NOT NULL,
             muted_at       INTEGER NOT NULL,
             PRIMARY KEY(user_id, contact_id)
         );

         CREATE TABLE IF NOT EXISTS peer_settings (
             user_id            INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
             friends_only       INTEGER NOT NULL DEFAULT 0,
             discovery_mode     TEXT NOT NULL DEFAULT 'both',
             rate_limit_per_min INTEGER NOT NULL DEFAULT 20,
             updated_at         INTEGER NOT NULL
         );

         CREATE TABLE IF NOT EXISTS proton_session_index (
             user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
             credential_id  TEXT NOT NULL,
             fingerprint    TEXT NOT NULL,
             blob_digest    TEXT NOT NULL,
             key_tag        TEXT NOT NULL,
             updated_at     INTEGER NOT NULL,
             PRIMARY KEY(user_id, credential_id)
         );

         CREATE TRIGGER IF NOT EXISTS proton_session_index_leaves_with_its_row
             AFTER DELETE ON user_credentials
         BEGIN
             DELETE FROM proton_session_index
             WHERE user_id = OLD.user_id AND credential_id = OLD.credential_id;
         END;

         CREATE TABLE IF NOT EXISTS private_token_sign_ins (
             user_id        INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
             credential_id  TEXT NOT NULL,
             blob_digest    TEXT NOT NULL,
             PRIMARY KEY(user_id, credential_id)
         );

         CREATE TRIGGER IF NOT EXISTS private_token_sign_in_leaves_with_its_row
             AFTER DELETE ON user_credentials
         BEGIN
             DELETE FROM private_token_sign_ins
             WHERE user_id = OLD.user_id AND credential_id = OLD.credential_id;
         END;",
    )
    .map_err(|e| format!("User partitions schema init: {e}"))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn default_kdf_params() -> user_crypto::Argon2Params {
    #[cfg(test)]
    {
        user_crypto::Argon2Params::fast_for_tests()
    }
    #[cfg(not(test))]
    {
        user_crypto::Argon2Params::default()
    }
}

pub(crate) fn clear_user_session() {
    #[cfg(test)]
    assert_test_lock_held();
    if let Ok(mut session) = USER_SESSION.lock() {
        *session = None;
        SESSION_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    invalidate_session_consumers();
}

/// Partition lock and user switch keep the vault cache, so MCP OAuth freshness
/// checks alone would not see them: cancel every pending and in-flight attempt.
fn invalidate_session_consumers() {
    crate::mcp_client_oauth::lifecycle::shared().invalidate_all();
    crate::mcp_client_install::invalidate_all();
}

impl SessionHold {
    /// [`clear_user_session`] through this hold, for a caller that took the
    /// session before SQLite's write lock and must drop it while it still
    /// holds both. The consumers are told once the hold is let go
    /// ([`invalidate_session_consumers`]). An empty hold (a poisoned lock)
    /// has nothing it can clear.
    fn clear(&mut self) {
        #[cfg(test)]
        assert_test_lock_held();
        if let Some(session) = self.0.as_mut() {
            **session = None;
            SESSION_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            HELD_SESSION.set(Some(None));
        }
    }
}

fn set_user_session(user_id: i64, dek: SecretKey) -> Result<(), String> {
    #[cfg(test)]
    assert_test_lock_held();
    let mut session = USER_SESSION
        .lock()
        .map_err(|_| "USER_SESSION_LOCK_POISONED".to_string())?;
    *session = Some(UserSession { user_id, dek });
    SESSION_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

pub(crate) fn session_generation() -> u64 {
    SESSION_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

/// Keep account selection and session unlock stable through a GUI config commit.
/// SQLite serializes account switches across processes; the session mutex serializes lock/unlock.
pub(crate) fn with_gui_account<R>(
    conn: &mut Connection,
    expected_user: Option<i64>,
    generation: u64,
    run: impl FnOnce() -> Result<R, String>,
) -> Result<R, String> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "gui_scope_unavailable")?;
    // Existing partition writers may hold the session while taking SQLite locks.
    // Never wait for that session while our IMMEDIATE transaction owns SQLite.
    let session = USER_SESSION.try_lock().map_err(|_| "busy")?;
    let user = active_user_id(&tx)?;
    if user.is_none() || user != expected_user || session_generation() != generation {
        return Err("gui_scope_changed".into());
    }
    let requires_password = read_user_key_row(&tx, user.unwrap())?.has_passphrase;
    if requires_password && session.as_ref().map(|s| s.user_id) != user {
        return Err("locked".into());
    }
    let result = run()?;
    // A read-only transaction holds the account lock; no partition state is changed.
    drop(tx);
    Ok(result)
}

pub fn user_unlock_status(conn: &Connection) -> Result<UserUnlockStatus, String> {
    let active_user_id = active_user_id(conn)?;
    let session_user_id = USER_SESSION
        .lock()
        .map_err(|_| "USER_SESSION_LOCK_POISONED".to_string())?
        .as_ref()
        .map(|session| session.user_id);
    let active_requires_passphrase = active_user_id
        .map(|user_id| read_user_key_row(conn, user_id).map(|row| row.has_passphrase))
        .transpose()?
        .unwrap_or(false);
    let unlocked_user_id = if active_user_id.is_some() && !active_requires_passphrase {
        active_user_id
    } else {
        session_user_id
    };
    Ok(UserUnlockStatus {
        active_user_id,
        unlocked_user_id,
        is_unlocked: active_user_id.is_some() && active_user_id == unlocked_user_id,
        security_reset: Vec::new(),
    })
}

fn normalize_name(name: &str) -> Result<(String, String), String> {
    let display = name.trim();
    if display.is_empty() {
        return Err("USER_NAME_REQUIRED".to_string());
    }
    if display.chars().count() > 80 {
        return Err("USER_NAME_TOO_LONG".to_string());
    }
    Ok((display.to_string(), display.to_lowercase()))
}

fn validate_avatar_fields(
    avatar_emoji: Option<&str>,
    avatar_color: Option<&str>,
) -> Result<(), String> {
    if let Some(avatar) = avatar_emoji {
        if avatar.len() > 128 * 1024 {
            return Err("AVATAR_TOO_LARGE".to_string());
        }
    }
    if let Some(color) = avatar_color {
        let valid = color.len() == 7
            && color.starts_with('#')
            && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit);
        if !valid {
            return Err("INVALID_AVATAR_COLOR".to_string());
        }
    }
    Ok(())
}

fn current_schema_version(conn: &Connection) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT value FROM global_state WHERE key = ?1",
        params![SCHEMA_VERSION_KEY],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| format!("Read schema version: {e}"))
}

/// The first schema version the move of the legacy payloads set: a database
/// at `"1"` or with no version predates the user partitions
/// ([`apply_pending_upgrades`]).
const LEGACY_MOVE_SCHEMA_VERSION: u32 = 2;

/// Whether the database `conn` has been through the move of the legacy
/// payloads ([`migrate_legacy_payloads`]), which sets the schema version in
/// the transaction that inserts the list and never runs again after. Any
/// version from the one that introduced the move says so, not only this
/// build's: a database the upgrades have not brought to this build yet was
/// moved, and so was one a newer build stamped before a downgrade. No
/// version, an older one, or one that is not a number has not been moved.
pub(crate) fn legacy_payloads_moved(conn: &Connection) -> Result<bool, String> {
    Ok(current_schema_version(conn)?
        .and_then(|version| version.trim().parse::<u32>().ok())
        .is_some_and(|version| version >= LEGACY_MOVE_SCHEMA_VERSION))
}

pub(crate) fn active_user_id(conn: &Connection) -> Result<Option<i64>, String> {
    let value = conn
        .query_row(
            "SELECT value FROM global_state WHERE key = ?1",
            params![ACTIVE_USER_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| format!("Read active user: {e}"))?;
    match value.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => value
            .parse::<i64>()
            .map(Some)
            .map_err(|e| format!("Invalid active user id: {e}")),
    }
}

/// One statement observes account selection and its key revision together.
/// This is metadata only: no vault read, DEK unwrap or KDF per GUI command.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct GuiAccountSnapshot {
    pub(crate) user: Option<i64>,
    activation_revision: i64,
    dek_verifier: Vec<u8>,
    has_passphrase: bool,
    session_generation: u64,
}

/// A snapshot for a test that plays account changes without a database.
#[cfg(test)]
pub(crate) fn gui_account_snapshot_for_test(
    user: Option<i64>,
    activation_revision: i64,
) -> GuiAccountSnapshot {
    GuiAccountSnapshot {
        user,
        activation_revision,
        dek_verifier: Vec::new(),
        has_passphrase: false,
        session_generation: 0,
    }
}

pub(crate) fn gui_account_snapshot() -> Result<GuiAccountSnapshot, String> {
    gui_account_snapshot_from(&open_or_init_cli()?)
}

pub(crate) fn with_gui_account_snapshot<T>(
    expected: &GuiAccountSnapshot,
    publish: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let mut conn = open_or_init_cli()?;
    let _lock = ProfileListLock::acquire(&conn)?;
    let _session = hold_session()?;
    with_gui_account_snapshot_in(&mut conn, expected, publish)
}

/// [`with_gui_account_snapshot`] for a caller that already holds the
/// [`ProfileListLock`] of this database (the lock is not re-entrant): the
/// AeroCloud placement, which resolves the profile names under it and then
/// writes. The session and the active-account row are held as there, so no
/// switch, lock or unlock lands between the check and the write.
pub(crate) fn with_gui_account_snapshot_holding_the_list_lock<T>(
    expected: &GuiAccountSnapshot,
    publish: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let mut conn = open_or_init_cli()?;
    let _session = hold_session()?;
    with_gui_account_snapshot_in(&mut conn, expected, publish)
}

fn with_gui_account_snapshot_in<T>(
    conn: &mut Connection,
    expected: &GuiAccountSnapshot,
    publish: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    if &gui_account_snapshot_from(&tx)? != expected {
        return Err(ACCOUNT_CHANGED.into());
    }
    // No await inside publish: session, active-account row and publication
    // are one critical section, including other desktop/CLI processes.
    publish()
}

fn gui_account_snapshot_from(conn: &Connection) -> Result<GuiAccountSnapshot, String> {
    let row = conn.query_row(
        "SELECT s.value, s.updated_at, COALESCE(u.dek_verifier, X''), COALESCE(u.has_passphrase, 0)
         FROM global_state s LEFT JOIN users u ON u.id = CAST(s.value AS INTEGER) WHERE s.key = ?1",
        params![ACTIVE_USER_KEY],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, Vec<u8>>(2)?, row.get::<_, bool>(3)?)),
    ).optional().map_err(|e| format!("Read GUI account revision: {e}"))?;
    let (user, activation_revision, dek_verifier, has_passphrase) = match row {
        Some((value, rev, verifier, protected)) => (
            if value.trim().is_empty() {
                None
            } else {
                Some(
                    value
                        .trim()
                        .parse::<i64>()
                        .map_err(|_| ACCOUNT_CHANGED.to_string())?,
                )
            },
            rev,
            verifier,
            protected,
        ),
        None => (None, 0, Vec::new(), false),
    };
    Ok(GuiAccountSnapshot {
        user,
        activation_revision,
        dek_verifier,
        has_passphrase,
        session_generation: session_generation(),
    })
}

fn upsert_global_state(
    tx: &Transaction<'_>,
    key: &str,
    value: &str,
    updated_at: i64,
) -> Result<(), String> {
    tx.execute(
        "INSERT INTO global_state(key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = CASE WHEN excluded.key = 'active_user_id' THEN MAX(global_state.updated_at + 1, excluded.updated_at) ELSE excluded.updated_at END",
        params![key, value, updated_at],
    )
    .map_err(|e| format!("Write global_state[{key}]: {e}"))?;
    Ok(())
}

fn lockout_key(user_id: i64) -> String {
    format!("user_lockout_{user_id}")
}

fn read_lockout_state(conn: &Connection, user_id: i64) -> Result<UserLockoutState, String> {
    let Some(value) = conn
        .query_row(
            "SELECT value FROM global_state WHERE key = ?1",
            params![lockout_key(user_id)],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| format!("Read user lockout: {e}"))?
    else {
        return Ok(UserLockoutState::default());
    };
    serde_json::from_str(&value).map_err(|e| format!("Parse user lockout: {e}"))
}

fn write_lockout_state(
    conn: &Connection,
    user_id: i64,
    state: &UserLockoutState,
) -> Result<(), String> {
    let value = serde_json::to_string(state).map_err(|e| format!("Serialize lockout: {e}"))?;
    conn.execute(
        "INSERT INTO global_state(key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![lockout_key(user_id), value, now_ms()],
    )
    .map_err(|e| format!("Write user lockout: {e}"))?;
    Ok(())
}

fn check_lockout(conn: &Connection, user_id: i64) -> Result<(), String> {
    let state = read_lockout_state(conn, user_id)?;
    if let Some(unlock_at) = state.unlock_at_epoch_ms {
        if unlock_at > now_ms() {
            return Err(format!("LOCKED_OUT:{unlock_at}"));
        }
    }
    Ok(())
}

fn reset_lockout(conn: &Connection, user_id: i64) -> Result<(), String> {
    conn.execute(
        "DELETE FROM global_state WHERE key = ?1",
        params![lockout_key(user_id)],
    )
    .map_err(|e| format!("Reset user lockout: {e}"))?;
    Ok(())
}

fn record_unlock_failure(conn: &Connection, user_id: i64) -> Result<(), String> {
    let mut state = read_lockout_state(conn, user_id)?;
    state.fail_count = state.fail_count.saturating_add(1);
    if state.fail_count >= LOCKOUT_THRESHOLD {
        let idx = (state.fail_count - LOCKOUT_THRESHOLD) as usize;
        let backoff = LOCKOUT_BACKOFF_MS
            .get(idx)
            .copied()
            .unwrap_or(*LOCKOUT_BACKOFF_MS.last().expect("backoff table"));
        state.unlock_at_epoch_ms = Some(now_ms() + backoff);
    }
    write_lockout_state(conn, user_id, &state)
}

fn encrypt_global_state_value(root_key: &SecretKey, value: &str) -> Result<String, String> {
    let (encrypted, nonce) = user_crypto::encrypt_blob(root_key, value.as_bytes())?;
    let payload = serde_json::json!({
        "enc": "aes-256-gcm",
        "nonce": BASE64.encode(nonce),
        "data": BASE64.encode(encrypted),
    })
    .to_string();
    Ok(payload)
}

fn encrypt_value(dek: &SecretKey, value: &Value) -> Result<(Vec<u8>, Vec<u8>), String> {
    let plaintext =
        serde_json::to_vec(value).map_err(|e| format!("Serialize JSON payload: {e}"))?;
    let (encrypted, nonce) = user_crypto::encrypt_blob(dek, &plaintext)?;
    Ok((encrypted, nonce.to_vec()))
}

fn decrypt_value(dek: &SecretKey, nonce: &[u8], encrypted: &[u8]) -> Result<Value, String> {
    let plaintext = user_crypto::decrypt_blob(dek, nonce, encrypted)?;
    let value = serde_json::from_slice::<Value>(&plaintext)
        .map_err(|e| format!("Parse encrypted JSON payload: {e}"));
    value
}

fn parse_legacy_profiles(input: Option<&str>) -> Result<Vec<Value>, String> {
    let Some(input) = input else {
        return Ok(Vec::new());
    };
    if input.trim().is_empty() {
        return Ok(Vec::new());
    }
    match serde_json::from_str::<Value>(input).map_err(|e| format!("Parse legacy profiles: {e}"))? {
        Value::Array(profiles) => Ok(profiles),
        Value::Null => Ok(Vec::new()),
        _ => Err("Legacy server_profiles payload must be a JSON array".to_string()),
    }
}

fn parse_legacy_settings(input: Option<&str>) -> Result<Option<Value>, String> {
    let Some(input) = input else {
        return Ok(None);
    };
    if input.trim().is_empty() {
        return Ok(None);
    }
    let value =
        serde_json::from_str::<Value>(input).map_err(|e| format!("Parse legacy settings: {e}"))?;
    Ok(Some(value))
}

fn value_str<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn value_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|v| match v {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => s.parse::<u64>().ok(),
            _ => None,
        })
    })
}

fn quota_u64(value: &Value, key: &str) -> Option<u64> {
    value
        .get("lastQuota")
        .and_then(|quota| value_u64(quota, &[key]))
        .or_else(|| value_u64(value, &[key]))
}

fn profile_uid_seed(profile: &Value, index: usize, seen: &mut HashSet<String>) -> String {
    let base = value_str(profile, &["id", "uid", "profileUid"])
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("legacy_profile_{}", index + 1));
    if seen.insert(base.clone()) {
        return base;
    }
    for suffix in 2usize.. {
        let candidate = format!("{base}_{suffix}");
        if seen.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("bounded by memory")
}

/// The tag of the OAuth token keys a profile id names: the id lowercased, as
/// [`crate::providers::oauth2::oauth_token_key`] lowercases it, so two
/// profile ids that differ only in case (`Team`, `team`) have one. The
/// alignment reads from it which accounts' profiles name a token key
/// ([`align_private_tokens_of`]), where the `profile_uid` tag tells only
/// which hold the id as it is.
fn profile_token_tag(root_key: &SecretKey, uid_seed: &str) -> Result<String, String> {
    user_crypto::metadata_tag(root_key, b"profile-token-uid", &uid_seed.to_lowercase())
}

fn profile_dedup_key(
    root_key: &SecretKey,
    profile: &Value,
    uid_seed: &str,
) -> Result<String, String> {
    let protocol = value_str(profile, &["protocol"]).unwrap_or("ftp");
    let provider_id = value_str(profile, &["providerId", "provider_id"]);
    let host = value_str(profile, &["host", "hostname", "endpoint"]).unwrap_or("");
    let username = value_str(profile, &["username", "user", "email", "account"]).unwrap_or("");
    let port = value_u64(profile, &["port"]).unwrap_or(0);
    let view = ProfileView {
        id: uid_seed,
        protocol,
        provider_id,
        host,
        port,
        username,
        used: quota_u64(profile, "used"),
        total: quota_u64(profile, "total"),
    };
    user_crypto::metadata_tag(root_key, b"dedup-key", &dedup_key(&view))
}

// --- EF-19 relocate identity probe (Option B) --------------------------------
//
// Storage `dedup_key` deliberately strips profile id and all secrets so the My
// Servers footer can collapse multi-protocol surfaces of the *same drive*. The
// cross-user Copy/Move probe must identify an *account*, not a drive: two
// distinct S3 / preset-OAuth / preset-WebDAV accounts that share an empty or
// placeholder blob username must NOT collide. This relocate-only key therefore
// pairs a stable account surface with a fingerprint of the resolved credential
// secret. `dedup_key` / `profile_dedup_key` stay untouched for storage.

/// Account surface for relocate identity: protocol + provider + host + port +
/// normalized username/account. Secrets are deliberately excluded here; they
/// land in the credential fingerprint instead.
fn relocate_identity_surface(profile: &Value) -> String {
    let protocol = value_str(profile, &["protocol"])
        .unwrap_or("ftp")
        .to_ascii_lowercase();
    let provider_id = value_str(profile, &["providerId", "provider_id"])
        .unwrap_or("")
        .to_ascii_lowercase();
    let host = normalize_host(value_str(profile, &["host", "hostname", "endpoint"]).unwrap_or(""));
    let port = value_u64(profile, &["port"]).unwrap_or(0);
    let username_raw = value_str(profile, &["username", "user", "email", "account"]).unwrap_or("");
    let user = normalize_user(username_raw).unwrap_or_default();
    format!("{protocol}\0{provider_id}\0{host}\0{port}\0{user}")
}

/// True when the blob carries a usable account identifier (non-empty, not an
/// opaque token). Weak surfaces (empty S3 access key, empty OAuth username,
/// shared WebDAV placeholder) require a matching credential fingerprint before
/// a Copy may be skipped.
fn relocate_surface_has_account_id(profile: &Value) -> bool {
    let username_raw = value_str(profile, &["username", "user", "email", "account"]).unwrap_or("");
    normalize_user(username_raw).is_some()
}

/// Vault / partition key candidates that hold the identifying secret for a
/// profile. Prefer per-profile keys so distinct accounts never share a
/// machine-level singleton fingerprint.
///
/// This is the *identity* subset only (`server_*` + OAuth/Jottacloud token).
/// Cross-user secret dual-write uses [`relocate_all_secret_key_candidates`] so
/// Crypt overlay / Filen / OneDrive hints travel with the profile (#366).
fn relocate_credential_key_candidates(protocol: &str, profile_id: &str) -> Vec<String> {
    let mut keys = vec![format!("server_{profile_id}")];
    if let Some(oauth_base) = oauth_vault_key_for_protocol(protocol) {
        // oauth_vault_key_for_protocol returns e.g. "oauth_pcloud" or
        // "jottacloud_refresh"; append the profile id for the per-profile key,
        // as the OAuth runtime composes it for an OAuth token.
        keys.push(match oauth_base.strip_prefix("oauth_") {
            Some(slug) => crate::providers::oauth2::oauth_token_key(slug, profile_id),
            None => format!("{oauth_base}_{profile_id}"),
        });
    }
    keys
}

/// Every per-profile vault key that must follow a profile across users on
/// Copy/Move. Delegates to [`crate::per_profile_vault_keys`] (same source of
/// truth as same-user duplicate / purge / export) so Crypt overlay passwords,
/// salts, keyfile paths, keystore blobs, Filen keys and OneDrive drive hints
/// are not left behind when the profile JSON badges still say Crypt is ON.
fn relocate_all_secret_key_candidates(protocol: &str, profile_id: &str) -> Vec<String> {
    crate::per_profile_vault_keys(profile_id, Some(protocol))
}

/// Decrypt one credential row with an already-resolved DEK (session-free).
fn read_credential_with_dek(
    conn: &Connection,
    user_id: i64,
    dek: &SecretKey,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    let row: Option<(Vec<u8>, Vec<u8>)> = conn
        .query_row(
            "SELECT encrypted_blob, nonce FROM user_credentials
             WHERE user_id = ?1 AND credential_id = ?2",
            params![user_id, credential_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("Read user credential: {e}"))?;
    match row {
        None => Ok(None),
        Some((blob, nonce)) => {
            let plaintext = user_crypto::decrypt_blob(dek, &nonce, &blob)?;
            let secret = String::from_utf8(plaintext.to_vec())
                .map_err(|_| "CREDENTIAL_NOT_UTF8".to_string())?;
            Ok(Some(Zeroizing::new(secret)))
        }
    }
}

/// One credential row decrypted with an already-resolved DEK, with the digest
/// of the very ciphertext decrypted ([`credential_blob_digest`]), both from
/// one SELECT: what [`checked_row`] compares the sign-in stamp against, so
/// the stamp is checked on the bytes the reader returns.
fn read_credential_and_digest_with_dek(
    conn: &Connection,
    user_id: i64,
    dek: &SecretKey,
    credential_id: &str,
) -> Result<Option<RowRead>, String> {
    let row: Option<(Vec<u8>, Vec<u8>)> = conn
        .query_row(
            "SELECT encrypted_blob, nonce FROM user_credentials
             WHERE user_id = ?1 AND credential_id = ?2",
            params![user_id, credential_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("Read user credential: {e}"))?;
    match row {
        None => Ok(None),
        Some((blob, nonce)) => {
            let plaintext = user_crypto::decrypt_blob(dek, &nonce, &blob)?;
            let secret = String::from_utf8(plaintext.to_vec())
                .map_err(|_| "CREDENTIAL_NOT_UTF8".to_string())?;
            Ok(Some((
                Zeroizing::new(secret),
                credential_blob_digest(&blob, &nonce),
            )))
        }
    }
}

/// A decrypted row and the digest of its ciphertext
/// ([`read_credential_and_digest_with_dek`]).
type RowRead = (Zeroizing<String>, String);

/// Resolve the first available identifying secret for `profile_id` from the
/// user's partition (and, when present, the legacy vault as fallback).
fn resolve_relocate_secret(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    user_dek: Option<&SecretKey>,
    protocol: &str,
    profile_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    let read_vault =
        |key: &str| Ok(CredentialStore::from_cache().and_then(|store| store.get_secret(key).ok()));
    resolve_relocate_secret_with(
        conn,
        root_key,
        user_id,
        user_dek,
        protocol,
        profile_id,
        &read_vault,
    )
}

/// [`resolve_relocate_secret`] with the vault read given.
fn resolve_relocate_secret_with(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    user_dek: Option<&SecretKey>,
    protocol: &str,
    profile_id: &str,
    read_vault: &SecretReader<'_>,
) -> Result<Option<Zeroizing<String>>, String> {
    let candidates = relocate_credential_key_candidates(protocol, profile_id);
    for key in &candidates {
        let from_partition = if let Some(dek) = user_dek {
            read_credential_and_digest_with_dek(conn, user_id, dek, key)?
        } else {
            // Session / device-wrapped path for the source (active) user.
            match get_user_credential_and_digest_for(conn, root_key, user_id, key) {
                Ok(v) => v,
                // A locked passphrase target without an explicit DEK is not a
                // fatal relocate error here: fall through to the vault.
                Err(e) if e == "USER_LOCKED" => None,
                Err(e) => return Err(e),
            }
        };
        // A row that may be another account's copy never moves with it.
        let from_partition = checked_row(conn, user_id, key, from_partition, read_vault)?;
        if let Some(secret) = from_partition {
            if !secret.is_empty() {
                return Ok(Some(secret));
            }
        }
        // A private token of another account than the legacy owner is its
        // partition row alone: the vault key of that name is the owner's.
        if vault_copy_is_another_accounts(conn, user_id, key)? {
            continue;
        }
        // Dual-write fallback: vault still holds secrets not yet mirrored.
        if let Ok(Some(secret)) = read_vault(key) {
            if !secret.is_empty() {
                return Ok(Some(secret));
            }
        }
    }
    Ok(None)
}

/// HMAC fingerprint of the identifying credential (empty when unresolved).
fn relocate_credential_fingerprint(
    conn: &Connection,
    root_key: &[u8; 32],
    root_secret: &SecretKey,
    user_id: i64,
    user_dek: Option<&SecretKey>,
    profile: &Value,
    profile_id: &str,
) -> Result<String, String> {
    let protocol = value_str(profile, &["protocol"]).unwrap_or("ftp");
    let secret = resolve_relocate_secret(conn, root_key, user_id, user_dek, protocol, profile_id)?;
    match secret {
        Some(s) if !s.is_empty() => {
            user_crypto::metadata_tag(root_secret, b"relocate-cred-fp", s.as_str())
        }
        _ => Ok(String::new()),
    }
}

/// True when `existing` is the same *account* as `source` for relocate purposes.
/// Requires matching account surface; credential fingerprints must also match
/// when either side has one. Weak surfaces (no usable username) without a
/// matching non-empty fingerprint never skip a Copy — that was the EF-19 false
/// "already saved" regression under storage `dedup_key`.
fn relocate_accounts_match(
    source: &Value,
    source_fp: &str,
    existing: &Value,
    existing_fp: &str,
) -> bool {
    if relocate_identity_surface(source) != relocate_identity_surface(existing) {
        return false;
    }
    if !source_fp.is_empty() && source_fp == existing_fp {
        return true;
    }
    // Strong surface (host+user etc.) may match without secrets (e.g. SFTP
    // key-only / no password stored). Weak surfaces must not.
    source_fp.is_empty() && existing_fp.is_empty() && relocate_surface_has_account_id(source)
}

fn insert_default_user(
    tx: &Transaction<'_>,
    root_key: &SecretKey,
    now: i64,
) -> Result<(i64, SecretKey, bool), String> {
    let (name, canonical) = normalize_name(DEFAULT_USER_NAME)?;
    if let Some((id, has_passphrase, wrapped_dek, dek_verifier)) = tx
        .query_row(
            "SELECT id, has_passphrase, wrapped_dek, dek_verifier
             FROM users WHERE name_canonical = ?1",
            params![&canonical],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|e| format!("Read existing default user: {e}"))?
    {
        if has_passphrase {
            return Err(
                "Default user already exists but is passphrase-protected; cannot resume legacy migration without unlocking it"
                    .to_string(),
            );
        }
        let dek = user_crypto::unwrap_dek(root_key, &wrapped_dek)?;
        if !user_crypto::verify_dek(&dek, &dek_verifier)? {
            return Err("DEK_VERIFIER_MISMATCH".to_string());
        }
        return Ok((id, dek, false));
    }

    let dek = user_crypto::generate_dek();
    let wrapped_dek = user_crypto::wrap_dek(root_key, &dek)?;
    let verifier = user_crypto::compute_dek_verifier(&dek)?.to_vec();

    tx.execute(
        "INSERT INTO users(
             name, name_canonical, avatar_emoji, avatar_color, has_passphrase,
             kdf_salt, kdf_params, wrapped_dek, dek_verifier, sort_order,
             created_at, updated_at, last_unlocked_at, is_admin
         )
         VALUES (?1, ?2, ?3, ?4, 0, NULL, NULL, ?5, ?6, 0, ?7, ?7, ?7, 1)",
        params![
            name,
            canonical,
            DEFAULT_USER_AVATAR,
            DEFAULT_USER_COLOR,
            wrapped_dek,
            verifier,
            now
        ],
    )
    .map_err(|e| format!("Create default user: {e}"))?;

    Ok((tx.last_insert_rowid(), dek, true))
}

pub fn migrate_legacy_payloads(
    conn: &mut Connection,
    legacy_profiles_json: Option<&str>,
    legacy_settings_json: Option<&str>,
    root_key: &[u8; 32],
) -> Result<MigrationReport, String> {
    init_db_schema(conn)?;
    if matches!(
        current_schema_version(conn)?.as_deref(),
        Some(SCHEMA_VERSION)
    ) {
        return Ok(MigrationReport {
            schema_version: SCHEMA_VERSION.to_string(),
            created_default_user: false,
            migrated_profiles: 0,
            migrated_settings_scopes: 0,
            already_migrated: true,
        });
    }

    let legacy_profiles = parse_legacy_profiles(legacy_profiles_json)?;
    // The legacy store let two profiles share a name: the first keeps it and
    // the others get " (1)", " (2)", ..., as every write of a list does.
    let (legacy_profiles, _) =
        crate::util::profile_name::unique_profile_names(&legacy_profiles, &legacy_profiles);
    let legacy_settings = parse_legacy_settings(legacy_settings_json)?;
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let legacy_profiles_backup =
        encrypt_global_state_value(&root_secret, legacy_profiles_json.unwrap_or("[]"))?;
    let legacy_settings_backup =
        encrypt_global_state_value(&root_secret, legacy_settings_json.unwrap_or("{}"))?;

    // Acquire the write lock up-front (IMMEDIATE) and wait, rather than erroring,
    // if another process is mid-migration, so concurrent GUI + CLI first-runs on
    // the same vault serialize instead of racing on the default-user INSERT.
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("Set busy timeout: {e}"))?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Start user partitions migration: {e}"))?;
    // Re-check the schema version inside the transaction: a concurrent process may
    // have completed the migration between our pre-transaction check above and our
    // acquiring the write lock here. Without this re-check the loser of the race
    // hit `UNIQUE constraint failed: users.name_canonical` on the default user.
    if matches!(
        current_schema_version(&tx)?.as_deref(),
        Some(SCHEMA_VERSION)
    ) {
        return Ok(MigrationReport {
            schema_version: SCHEMA_VERSION.to_string(),
            created_default_user: false,
            migrated_profiles: 0,
            migrated_settings_scopes: 0,
            already_migrated: true,
        });
    }
    let now = now_ms();
    let (default_user_id, default_dek, created_default_user) =
        insert_default_user(&tx, &root_secret, now)?;
    let mut seen_uids = HashSet::new();
    let mut migrated_profiles = 0usize;

    for (index, profile) in legacy_profiles.iter().enumerate() {
        let uid_seed = profile_uid_seed(profile, index, &mut seen_uids);
        let uid = user_crypto::metadata_tag(&root_secret, b"profile-uid", &uid_seed)?;
        let token_uid = profile_token_tag(&root_secret, &uid_seed)?;
        let key = profile_dedup_key(&root_secret, profile, &uid_seed)?;
        let (encrypted_blob, nonce) = encrypt_value(&default_dek, profile)?;
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO server_profiles(
                 user_id, profile_uid, dedup_key, name, encrypted_blob, nonce,
                 aead_alg, created_at, updated_at, token_uid
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'aes-256-gcm', ?7, ?7, ?8)",
                params![
                    default_user_id,
                    uid,
                    key,
                    uid,
                    encrypted_blob,
                    nonce,
                    now,
                    token_uid
                ],
            )
            .map_err(|e| format!("Migrate legacy profile: {e}"))?;
        migrated_profiles += inserted;
    }
    // A bridge scan with no account yet keeps its ledger in this database
    // before the move (it opens it through `open_or_init_cli`, schema but no
    // version): the list moved adopts the OAuth apps its profiles use, as
    // every other list writer's does.
    crate::bridge_import_scan::adopt_oauth_apps(&tx, &legacy_profiles)?;

    let mut migrated_settings_scopes = 0usize;
    if let Some(settings) = legacy_settings.as_ref() {
        let (encrypted_blob, nonce) = encrypt_value(&default_dek, settings)?;
        migrated_settings_scopes = tx
            .execute(
                "INSERT OR IGNORE INTO user_settings(
                 user_id, scope, encrypted_blob, nonce, aead_alg, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, 'aes-256-gcm', ?5)",
                params![
                    default_user_id,
                    LEGACY_SETTINGS_SCOPE,
                    encrypted_blob,
                    nonce,
                    now
                ],
            )
            .map_err(|e| format!("Migrate legacy settings: {e}"))?;
    }

    upsert_global_state(&tx, LEGACY_PROFILES_KEY, &legacy_profiles_backup, now)?;
    upsert_global_state(&tx, LEGACY_SETTINGS_KEY, &legacy_settings_backup, now)?;
    upsert_global_state(&tx, ACTIVE_USER_KEY, &default_user_id.to_string(), now)?;
    upsert_global_state(&tx, SCHEMA_VERSION_KEY, SCHEMA_VERSION, now)?;
    tx.commit()
        .map_err(|e| format!("Commit user partitions migration: {e}"))?;

    Ok(MigrationReport {
        schema_version: SCHEMA_VERSION.to_string(),
        created_default_user,
        migrated_profiles,
        migrated_settings_scopes,
        already_migrated: false,
    })
}

fn get_optional_store_entry(
    store: &CredentialStore,
    account: &str,
) -> Result<Option<String>, String> {
    match store.get(account) {
        Ok(value) => Ok(Some(value)),
        Err(CredentialError::NotFound(_)) => Ok(None),
        Err(e) => Err(format!("Read legacy credential {account}: {e}")),
    }
}

/// MUV-2: best-effort eager credential migration on the GUI side. Pulls the
/// cached store; if the vault is locked behind a Master Password the store is
/// unavailable and the migration simply runs on a later boot. Never fails the
/// caller, and skips entirely when nothing is eager-pending so it adds no cost
/// to the per-command boot path once the bulk copy is done.
fn run_eager_credential_migration_gui(conn: &Connection) {
    if !has_eager_pending_users(conn).unwrap_or(false) {
        return;
    }
    if let Some(store) = CredentialStore::from_cache() {
        let mut root_key = store.derive_user_partition_wrapping_key();
        let _ = migrate_credentials_eager_all(conn, &store, &root_key);
        root_key.zeroize();
    }
}

pub fn init_or_migrate(app: &AppHandle) -> Result<MigrationReport, String> {
    let mut conn = open_or_init(app)?;
    // Apply any pending in-place schema upgrades in cascade:
    //   v2 -> v3 (is_admin column + admin seed),
    //   v3 -> v4 (user_credentials table).
    // A v2 database visits both steps in one startup. `Ok(true)` means the
    // schema is current and there is nothing legacy to migrate.
    if apply_pending_upgrades(&mut conn)? {
        run_eager_credential_migration_gui(&conn);
        return Ok(already_migrated_report());
    }

    // Multi-User is a first-class surface and does not require Master Password.
    // If the credential store is not cached yet (the GUI usually primes it via
    // `init_credential_store`, but the auto-keyring path may not have fired
    // before the user dropdown asks for users), try a best-effort bootstrap
    // here. A failure is non-fatal: STORE_NOT_READY is the legitimate response
    // when the vault is locked behind a Master Password that the user has not
    // unlocked yet.
    let store = match CredentialStore::from_cache() {
        Some(store) => store,
        None => {
            let _ = CredentialStore::init();
            CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?
        }
    };
    let mut root_key = store.derive_user_partition_wrapping_key();
    let profiles_json = get_optional_store_entry(&store, "config_server_profiles")?;
    let settings_json = match get_optional_store_entry(&store, "config_app_settings")? {
        Some(value) => Some(value),
        None => get_optional_store_entry(&store, "aeroftp_settings")?,
    };
    let result = migrate_legacy_payloads(
        &mut conn,
        profiles_json.as_deref(),
        settings_json.as_deref(),
        &root_key,
    );
    root_key.zeroize();
    let report = result?;
    // MUV-2: the legacy migration just created the `default` user; copy its
    // raw secrets out of the vault into `user_credentials` in the same boot.
    run_eager_credential_migration_gui(&conn);
    Ok(report)
}

fn upgrade_v2_to_v3(conn: &mut Connection) -> Result<(), String> {
    // SQLite does not support `IF NOT EXISTS` on ADD COLUMN. Detect the
    // column via pragma so the migration is idempotent across restarts
    // and across the test suite running on already-v3 in-memory dbs.
    let has_is_admin = {
        let mut stmt = conn
            .prepare("PRAGMA table_info(users)")
            .map_err(|e| format!("Read users table info: {e}"))?;
        let mut found = false;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("Iterate users table info: {e}"))?;
        while let Some(row) = rows
            .next()
            .map_err(|e| format!("Read users table info row: {e}"))?
        {
            let name: String = row.get(1).map_err(|e| format!("Read column name: {e}"))?;
            if name == "is_admin" {
                found = true;
                break;
            }
        }
        found
    };

    let tx = conn
        .transaction()
        .map_err(|e| format!("Start v2->v3 upgrade: {e}"))?;
    if !has_is_admin {
        #[cfg(test)]
        run_dual_write_hook(&BEFORE_COLUMN_ADD);
        match tx.execute(
            "ALTER TABLE users ADD COLUMN is_admin INTEGER NOT NULL DEFAULT 0",
            [],
        ) {
            // Or added by another connection since the check above; the seed
            // below runs either way.
            Ok(_) => {}
            Err(e) if is_duplicate_column(&e) => {}
            Err(e) => return Err(format!("ALTER TABLE users ADD is_admin: {e}")),
        }
    }
    tx.execute(
        "UPDATE users SET is_admin = 1
         WHERE id = (SELECT id FROM users ORDER BY id ASC LIMIT 1)",
        [],
    )
    .map_err(|e| format!("Seed first user as admin: {e}"))?;
    // Land exactly on "3"; the cascade in `apply_pending_upgrades` then runs
    // v3 -> v4. Using the literal (not SCHEMA_VERSION) keeps the step bounded
    // so chaining from an older database visits every intermediate version.
    upsert_global_state(&tx, SCHEMA_VERSION_KEY, "3", now_ms())?;
    tx.commit()
        .map_err(|e| format!("Commit v2->v3 upgrade: {e}"))?;
    Ok(())
}

/// v3 -> v4: add the `user_credentials` table (per-user encrypted secrets under
/// the user DEK). The table create is idempotent (`IF NOT EXISTS`) and a fresh
/// install already gets it from [`init_db_schema`]; this upgrade exists so an
/// existing v3 database gains the table and lands on schema "4". No data is
/// moved here: migrating the legacy `vault.db` secrets is MUV-2.
fn upgrade_v3_to_v4(conn: &mut Connection) -> Result<(), String> {
    let tx = conn
        .transaction()
        .map_err(|e| format!("Start v3->v4 upgrade: {e}"))?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS user_credentials (
             user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
             credential_id   TEXT NOT NULL,
             credential_type TEXT NOT NULL,
             encrypted_blob  BLOB NOT NULL,
             nonce           BLOB NOT NULL,
             aead_alg        TEXT NOT NULL DEFAULT 'aes-256-gcm',
             updated_at      INTEGER NOT NULL,
             PRIMARY KEY(user_id, credential_id)
         );
         CREATE INDEX IF NOT EXISTS idx_user_credentials_type
             ON user_credentials(user_id, credential_type);",
    )
    .map_err(|e| format!("Create user_credentials table: {e}"))?;
    upsert_global_state(&tx, SCHEMA_VERSION_KEY, "4", now_ms())?;
    tx.commit()
        .map_err(|e| format!("Commit v3->v4 upgrade: {e}"))?;
    Ok(())
}

/// v4 -> v5 upgrade. The P2P secret-store tables (peer_identity / peer_contacts /
/// peer_drives) used by AeroShare are additive and already created by
/// `init_db_schema`'s `CREATE TABLE IF NOT EXISTS` on every open, so this only
/// stamps the new schema version inside a transaction, mirroring
/// [`upgrade_v2_to_v3`]. This branch is REQUIRED: without it a v4 database would
/// keep reporting v4 and the cascade would never settle on `SCHEMA_VERSION`.
fn upgrade_v4_to_v5(conn: &mut Connection) -> Result<(), String> {
    let tx = conn
        .transaction()
        .map_err(|e| format!("Start v4->v5 upgrade: {e}"))?;
    upsert_global_state(&tx, SCHEMA_VERSION_KEY, SCHEMA_VERSION, now_ms())?;
    tx.commit()
        .map_err(|e| format!("Commit v4->v5 upgrade: {e}"))?;
    Ok(())
}

/// Run `f` with the unlocked DEK of `user_id`, scoped to that partition without
/// switching the active session. Thin wrapper over [`with_user_dek`] so the
/// sibling P2P secret-store facade tests ([`crate::peer_identity`]) can thread a
/// partition's real DEK. Test-only for now: shipping callers thread the DEK from
/// the active session exactly like the profile commands do.
#[cfg(test)]
pub(crate) fn with_partition_dek<R>(
    conn: &Connection,
    root_key: &SecretKey,
    user_id: i64,
    f: impl FnOnce(&SecretKey) -> Result<R, String>,
) -> Result<R, String> {
    with_user_dek(conn, root_key, user_id, |_, dek| f(dek))
}

fn already_migrated_report() -> MigrationReport {
    MigrationReport {
        schema_version: SCHEMA_VERSION.to_string(),
        created_default_user: false,
        migrated_profiles: 0,
        migrated_settings_scopes: 0,
        already_migrated: true,
    }
}

/// Apply pending schema upgrades in cascade on an already-open connection.
///
/// Returns `Ok(true)` when the schema is now current (`SCHEMA_VERSION`) and no
/// legacy-payload migration is required; `Ok(false)` when the database predates
/// the user-partitions schema (version `None`/`"1"`) and the caller must run
/// [`migrate_legacy_payloads`]. The cascade is written sequentially (not
/// mutually exclusive) so a v2 database visits v3, v4, then v5 in a single startup.
fn apply_pending_upgrades(conn: &mut Connection) -> Result<bool, String> {
    if matches!(
        current_schema_version(conn)?.as_deref(),
        Some(SCHEMA_VERSION)
    ) {
        return Ok(true);
    }
    if current_schema_version(conn)?.as_deref() == Some("2") {
        upgrade_v2_to_v3(conn)?;
    }
    if current_schema_version(conn)?.as_deref() == Some("3") {
        upgrade_v3_to_v4(conn)?;
    }
    if current_schema_version(conn)?.as_deref() == Some("4") {
        upgrade_v4_to_v5(conn)?;
        return Ok(true);
    }
    Ok(false)
}

pub fn list_users(conn: &Connection) -> Result<Vec<UserMetadata>, String> {
    let active = active_user_id(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, name, avatar_emoji, avatar_color, has_passphrase, sort_order,
                    created_at, updated_at, last_unlocked_at, is_admin, is_default
             FROM users
             ORDER BY sort_order ASC, name_canonical ASC",
        )
        .map_err(|e| format!("Prepare list users: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let has_passphrase: i64 = row.get(4)?;
            let is_admin: i64 = row.get(9)?;
            let is_default: i64 = row.get(10)?;
            Ok(UserMetadata {
                id,
                name: row.get(1)?,
                avatar_emoji: row.get(2)?,
                avatar_color: row.get(3)?,
                has_passphrase: has_passphrase != 0,
                sort_order: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
                last_unlocked_at: row.get(8)?,
                is_active: active == Some(id),
                is_admin: is_admin != 0,
                is_default: is_default != 0,
            })
        })
        .map_err(|e| format!("Query list users: {e}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Read list users: {e}"))
}

pub fn get_active_user(conn: &Connection) -> Result<Option<UserMetadata>, String> {
    let Some(active_id) = active_user_id(conn)? else {
        return Ok(None);
    };
    Ok(list_users(conn)?
        .into_iter()
        .find(|user| user.id == active_id))
}

struct UserKeyRow {
    has_passphrase: bool,
    kdf_salt: Option<Vec<u8>>,
    kdf_params: Option<String>,
    wrapped_dek: Vec<u8>,
    dek_verifier: Vec<u8>,
}

fn read_user_key_row(conn: &Connection, user_id: i64) -> Result<UserKeyRow, String> {
    conn.query_row(
        "SELECT has_passphrase, kdf_salt, kdf_params, wrapped_dek, dek_verifier
         FROM users WHERE id = ?1",
        params![user_id],
        |row| {
            let has_passphrase: i64 = row.get(0)?;
            Ok(UserKeyRow {
                has_passphrase: has_passphrase != 0,
                kdf_salt: row.get(1)?,
                kdf_params: row.get(2)?,
                wrapped_dek: row.get(3)?,
                dek_verifier: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(|e| format!("Read user key metadata: {e}"))?
    .ok_or_else(|| "USER_NOT_FOUND".to_string())
}

fn salt_from_row(row: &UserKeyRow) -> Result<[u8; 16], String> {
    let salt = row
        .kdf_salt
        .as_ref()
        .ok_or_else(|| "KDF_SALT_MISSING".to_string())?;
    salt.as_slice()
        .try_into()
        .map_err(|_| "INVALID_KDF_SALT_SIZE".to_string())
}

fn unwrap_user_dek_with_key(
    row: &UserKeyRow,
    wrapping_key: &SecretKey,
) -> Result<SecretKey, String> {
    let dek = user_crypto::unwrap_dek(wrapping_key, &row.wrapped_dek)?;
    if !user_crypto::verify_dek(&dek, &row.dek_verifier)? {
        return Err("DEK_VERIFIER_MISMATCH".to_string());
    }
    Ok(dek)
}

fn unwrap_user_dek_with_passphrase(
    row: &UserKeyRow,
    passphrase: &str,
) -> Result<SecretKey, String> {
    let params = user_crypto::params_from_json(
        row.kdf_params
            .as_deref()
            .ok_or_else(|| "KDF_PARAMS_MISSING".to_string())?,
    )?;
    let salt = salt_from_row(row)?;
    let wrapping_key = user_crypto::derive_wrapping_key(passphrase, &salt, &params)?;
    unwrap_user_dek_with_key(row, &wrapping_key)
}

fn set_active_user_row(conn: &Connection, user_id: i64) -> Result<(), String> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id = ?1)",
            params![user_id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|e| format!("Check user exists: {e}"))?;
    if !exists {
        return Err("USER_NOT_FOUND".to_string());
    }
    let now = now_ms();
    conn.execute(
        "INSERT INTO global_state(key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = MAX(global_state.updated_at + 1, excluded.updated_at)",
        params![ACTIVE_USER_KEY, user_id.to_string(), now],
    )
    .map_err(|e| format!("Set active user: {e}"))?;
    conn.execute(
        "UPDATE users SET last_unlocked_at = ?1, updated_at = ?1 WHERE id = ?2",
        params![now, user_id],
    )
    .map_err(|e| format!("Touch active user: {e}"))?;
    Ok(())
}

/// Unlock the DEK for a specific user id without touching active_user_id.
/// Used to scope an operation to a specific partition (e.g. CLI `--user`
/// flag) without persisting the switch.
fn with_user_dek<R>(
    conn: &Connection,
    root_key: &SecretKey,
    user_id: i64,
    f: impl FnOnce(i64, &SecretKey) -> Result<R, String>,
) -> Result<R, String> {
    if security_reset_blocked(conn, user_id) {
        return Err("SECURITY_RESET_PENDING".to_string());
    }
    let row = read_user_key_row(conn, user_id)?;
    if row.has_passphrase {
        let _session = hold_session()?;
        return HELD_SESSION.with_borrow(|held| {
            let Some((_, dek)) = held
                .as_ref()
                .and_then(Option::as_ref)
                .filter(|(session_user, _)| *session_user == user_id)
            else {
                return Err("USER_LOCKED".to_string());
            };
            if !user_crypto::verify_dek(dek, &row.dek_verifier)? {
                return Err("DEK_VERIFIER_MISMATCH".to_string());
            }
            if security_reset_pending(conn, user_id)? {
                return Err("SECURITY_RESET_PENDING".to_string());
            }
            f(user_id, dek)
        });
    }

    let dek = unwrap_user_dek_with_key(&row, root_key)?;
    if security_reset_pending(conn, user_id)? {
        return Err("SECURITY_RESET_PENDING".to_string());
    }
    f(user_id, &dek)
}

/// Resolve a user's DEK with an explicit (optional) passphrase, WITHOUT
/// touching `active_user_id` or the global [`USER_SESSION`]. This is the
/// session-free counterpart of [`unlock_user_transient`]: it is used to write
/// into ANOTHER user's partition (cross-user profile copy/move, N4) while the
/// source user's session stays primed.
///
/// For a passphrase-protected target the passphrase is REQUIRED
/// (`TARGET_PASSPHRASE_REQUIRED` otherwise) and lockout accounting is enforced
/// exactly like an interactive unlock, so this path cannot be used to brute
/// force a partition. For a passphrase-free target the local `root_key`
/// unwraps the DEK and a stray passphrase is rejected.
fn resolve_user_dek_scoped(
    conn: &Connection,
    root_secret: &SecretKey,
    user_id: i64,
    passphrase: Option<&str>,
) -> Result<SecretKey, String> {
    if security_reset_blocked(conn, user_id) {
        return Err("SECURITY_RESET_PENDING".to_string());
    }
    let row = read_user_key_row(conn, user_id)?;
    if security_reset_pending(conn, user_id)? {
        return Err("SECURITY_RESET_PENDING".to_string());
    }
    if row.has_passphrase {
        check_lockout(conn, user_id)?;
        let passphrase = passphrase.ok_or_else(|| "TARGET_PASSPHRASE_REQUIRED".to_string())?;
        match unwrap_user_dek_with_passphrase(&row, passphrase) {
            Ok(dek) => {
                reset_lockout(conn, user_id)?;
                Ok(dek)
            }
            Err(_) => {
                record_unlock_failure(conn, user_id)?;
                Err("WRONG_PASSPHRASE".to_string())
            }
        }
    } else {
        if passphrase.is_some() {
            return Err("PASSPHRASE_NOT_NEEDED".to_string());
        }
        unwrap_user_dek_with_key(&row, root_secret)
    }
}

pub fn list_active_server_profiles(
    conn: &Connection,
    root_key: &[u8; 32],
) -> Result<Vec<Value>, String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    list_server_profiles_for(conn, root_key, user_id)
}

/// Returns the list stored, whose names [`insert_profiles_with_dek`] made
/// unique.
pub fn replace_active_server_profiles(
    conn: &mut Connection,
    root_key: &[u8; 32],
    profiles: &[Value],
) -> Result<Vec<Value>, String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    replace_server_profiles_for(conn, root_key, user_id, profiles)
}

/// Refusal of a profile write whose account is no longer the active one.
pub const ACCOUNT_CHANGED: &str =
    "ACCOUNT_CHANGED: the active account changed since these profiles were read; nothing was saved";

/// Refusal of a write whose list was read before another writer saved. The
/// caller reads the list again and redoes its change.
pub const PROFILES_CHANGED: &str =
    "PROFILES_CHANGED: the saved profiles changed since they were read; nothing was saved";

/// How long a writer waits for [`ProfileListLock`] before giving up.
const PROFILE_LIST_LOCK_WAIT: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_millis(300)
} else {
    std::time::Duration::from_secs(60)
};

/// An exclusive operating-system lock on the profile list of one database, which
/// every writer of the list takes, but for the migration on opening
/// ([`init_or_migrate_cli`]): an import for its secrets and its list together, any
/// other for its write. The GUI and the CLI share it across
/// processes, and the system drops it when the holder exits, however it exits, so
/// a crash leaves nothing to clean up. It sits next to the database it guards:
/// another config directory (a portable copy, a test) has its own. Taken before
/// the session and the transaction. Not re-entrant, even in one thread: a holder
/// must not call a writer that takes it.
pub struct ProfileListLock {
    _file: std::fs::File,
    exclusive: bool,
}

impl ProfileListLock {
    /// Whether this holder really excludes the others: false on a filesystem
    /// without file locks. An ordinary save goes on, the revision refusing a stale
    /// read. An import must not: another import's rollback could put back a secret
    /// under the list this one stored.
    pub fn is_exclusive(&self) -> bool {
        self.exclusive
    }

    /// Wait for the lock of the database `conn` has open, up to one minute. An
    /// in-memory database has no other writer and no lock.
    pub fn acquire(conn: &Connection) -> Result<Option<Self>, String> {
        match conn.path() {
            Some(path) if !path.is_empty() => {
                Self::acquire_within(std::path::Path::new(path), PROFILE_LIST_LOCK_WAIT).map(Some)
            }
            _ => Ok(None),
        }
    }

    /// Wait for the lock of the database at `db_path`, up to one minute, for a
    /// writer that replaces the file itself and has no connection to it.
    pub fn acquire_for_path(db_path: &std::path::Path) -> Result<Self, String> {
        Self::acquire_within(db_path, PROFILE_LIST_LOCK_WAIT)
    }

    /// Wait for the lock of the database at `db_path`, up to `wait`. The path is
    /// resolved first, as SQLite resolves the one a connection reports, so a data
    /// folder reached through a symbolic link still has one lock. A wait on a
    /// runtime worker hands its other tasks over first, and stays on this thread,
    /// which holds the session.
    pub fn acquire_within(
        db_path: &std::path::Path,
        wait: std::time::Duration,
    ) -> Result<Self, String> {
        let parent = db_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create user partitions dir: {e}"))?;
        let resolved = std::fs::canonicalize(db_path).or_else(|_| {
            std::fs::canonicalize(parent)
                .map(|dir| dir.join(db_path.file_name().unwrap_or_default()))
        });
        let resolved = resolved.map_err(|e| format!("Resolve user partitions dir: {e}"))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(resolved.with_extension("write-lock"))
            .map_err(|e| format!("Open profile list lock: {e}"))?;
        let exclusive = std::cell::Cell::new(true);
        let try_lock = || {
            #[cfg(test)]
            if NO_FILE_LOCKS_IN_TEST.get() {
                return Err(std::fs::TryLockError::Error(std::io::Error::from(
                    std::io::ErrorKind::Unsupported,
                )));
            }
            file.try_lock()
        };
        let take = || match try_lock() {
            Ok(()) => Ok(true),
            Err(std::fs::TryLockError::WouldBlock) => Ok(false),
            // A filesystem without file locks (some network mounts) must not end
            // every save: the revision still refuses a list written from a stale
            // read. An import refuses by itself ([`Self::is_exclusive`]).
            Err(std::fs::TryLockError::Error(e)) if file_locks_unavailable(&e) => {
                tracing::warn!("Profile list lock not available on this filesystem: {e}");
                exclusive.set(false);
                Ok(true)
            }
            Err(std::fs::TryLockError::Error(e)) => Err(format!("Take profile list lock: {e}")),
        };
        if take()? {
            return Ok(Self {
                exclusive: exclusive.get(),
                _file: file,
            });
        }
        let wait_for_it = || {
            let deadline = std::time::Instant::now() + wait;
            loop {
                if std::time::Instant::now() >= deadline {
                    return Err(PROFILE_LIST_BUSY.to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                if take()? {
                    return Ok(());
                }
            }
        };
        let on_worker = tokio::runtime::Handle::try_current().is_ok_and(|handle| {
            handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread
        });
        if on_worker {
            tokio::task::block_in_place(wait_for_it)?;
        } else {
            wait_for_it()?;
        }
        Ok(Self {
            exclusive: exclusive.get(),
            _file: file,
        })
    }
}

#[cfg(test)]
thread_local! {
    /// Plays a filesystem without file locks, for a test on any filesystem.
    pub(crate) static NO_FILE_LOCKS_IN_TEST: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Whether a failed lock says the filesystem has no file locks at all.
pub(crate) fn file_locks_unavailable(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::ENOLCK) {
        return true;
    }
    error.kind() == std::io::ErrorKind::Unsupported
}

/// Refusal of a write that reads and compares before it stores, on a filesystem
/// without file locks.
pub const NO_FILE_LOCKS: &str =
    "The AeroFTP data folder is on a filesystem without file locks, so this cannot be kept safe from another writer, and nothing was changed. Move the data folder to a local disk, then try again";

/// Refusal of a writer that waited for [`ProfileListLock`] in vain.
pub const PROFILE_LIST_BUSY: &str =
    "PROFILE_LIST_BUSY: another import or restore is still writing the saved profiles; nothing was saved, try again when it finishes";

/// [`replace_active_server_profiles`] for a list read while `expected_user`
/// was active. A read-modify-write takes seconds, and an account switched in
/// between (GUI or CLI) would otherwise receive the other account's list, so
/// the write is refused with [`ACCOUNT_CHANGED`] unless `expected_user` is
/// still active when it commits: the check and the rows share one IMMEDIATE
/// transaction, which an account switch (a `global_state` write) cannot
/// interleave. The transaction opens inside [`with_user_dek`], after the
/// session lock, the same order every partition writer takes. With
/// `expected_revision`, the one [`list_server_profiles_at_revision`] returned,
/// a list another writer saved since that read is refused with
/// [`PROFILES_CHANGED`] in the same transaction.
pub fn replace_expected_server_profiles(
    conn: &mut Connection,
    root_key: &[u8; 32],
    expected_user: i64,
    expected_revision: Option<&str>,
    profiles: &[Value],
) -> Result<Vec<Value>, String> {
    let conn: &Connection = conn;
    let _lock = ProfileListLock::acquire(conn)?;
    // Answer a switch away from a locked account as ACCOUNT_CHANGED, before
    // its DEK lookup can fail as USER_LOCKED.
    if active_user_id(conn)? != Some(expected_user) {
        return Err(ACCOUNT_CHANGED.to_string());
    }
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, expected_user, |user_id, dek| {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Start expected profile replace: {e}"))?;
        if active_user_id(&tx)? != Some(user_id) {
            return Err(ACCOUNT_CHANGED.to_string());
        }
        if let Some(expected) = expected_revision {
            if profile_rows_revision(&read_profile_rows(&tx, user_id)?) != expected {
                return Err(PROFILES_CHANGED.to_string());
            }
        }
        let stored = insert_profiles_with_dek(&tx, &root_secret, user_id, dek, profiles)?;
        tx.commit()
            .map_err(|e| format!("Commit expected profile replace: {e}"))?;
        Ok(stored)
    })
}

/// Read server profiles for a specific user id without changing active_user_id.
/// This is the back-end for CLI `--user` per-invocation scoping (MU-3): the
/// caller resolves the target user id (via `cli_find_user_by_name`) and the
/// active_user_id stored in `global_state` stays untouched.
pub fn list_server_profiles_for(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
) -> Result<Vec<Value>, String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        read_profiles_with_dek(conn, user_id, dek)
    })
}

/// Decrypt every server profile row for `user_id` with an already-resolved DEK.
/// Session-free: the caller is responsible for obtaining `dek` (via
/// [`with_user_dek`] or [`resolve_user_dek_scoped`]). Extracted so cross-user
/// relocation can read a target partition without clobbering the global
/// [`USER_SESSION`].
fn read_profiles_with_dek(
    conn: &Connection,
    user_id: i64,
    dek: &SecretKey,
) -> Result<Vec<Value>, String> {
    decrypt_profile_rows(&read_profile_rows(conn, user_id)?, dek)
}

/// One stored server profile: `(encrypted_blob, nonce)`.
type ProfileRow = (Vec<u8>, Vec<u8>);

/// The stored row of each server profile of `user_id`, in
/// list order, read by one statement.
fn read_profile_rows(conn: &Connection, user_id: i64) -> Result<Vec<ProfileRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT encrypted_blob, nonce
             FROM server_profiles
             WHERE user_id = ?1
             ORDER BY id ASC",
        )
        .map_err(|e| format!("Prepare profile list: {e}"))?;
    let rows = stmt
        .query_map(params![user_id], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|e| format!("Query profile list: {e}"))?;
    rows.map(|row| row.map_err(|e| format!("Read profile row: {e}")))
        .collect()
}

fn decrypt_profile_rows(rows: &[ProfileRow], dek: &SecretKey) -> Result<Vec<Value>, String> {
    rows.iter()
        .map(|(encrypted_blob, nonce)| decrypt_value(dek, nonce, encrypted_blob))
        .collect()
}

/// What a read of the list hands to its write-back: a SHA-256 over the stored
/// encrypted bytes, never over the decrypted text, because this value reaches
/// the renderer. Every save re-encrypts under new nonces, so a save of the same
/// list also counts as a change: a false conflict is refused like a real one and
/// costs one more read.
fn profile_rows_revision(rows: &[ProfileRow]) -> String {
    let mut hash = Sha256::new();
    for (encrypted_blob, nonce) in rows {
        hash.update((encrypted_blob.len() as u64).to_le_bytes());
        hash.update(encrypted_blob);
        hash.update((nonce.len() as u64).to_le_bytes());
        hash.update(nonce);
    }
    hex::encode(hash.finalize())
}

/// `user_id`'s profiles and the revision of exactly those rows, from one read.
pub fn list_server_profiles_at_revision(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
) -> Result<(Vec<Value>, String), String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        let rows = read_profile_rows(conn, user_id)?;
        Ok((
            decrypt_profile_rows(&rows, dek)?,
            profile_rows_revision(&rows),
        ))
    })
}

/// Let `apply` change `user_id`'s profiles and store the result, only if the
/// list is still the one `revision` was read from and `user_id` is still the
/// active account ([`PROFILES_CHANGED`], [`ACCOUNT_CHANGED`]): check, read and
/// write share one IMMEDIATE transaction. Returns the list sent and the list
/// stored (names made unique by the write). The caller holds [`ProfileListLock`].
pub fn update_server_profiles_at_revision(
    conn: &mut Connection,
    root_key: &[u8; 32],
    user_id: i64,
    revision: &str,
    apply: impl FnOnce(&mut Vec<Value>),
) -> Result<(Vec<Value>, Vec<Value>), String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    // The session before the reservation ([`SessionHold`]).
    let _session = hold_user_session(conn, user_id);
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Start import write: {e}"))?;
    if active_user_id(&tx)? != Some(user_id) {
        return Err(ACCOUNT_CHANGED.to_string());
    }
    let written = with_user_dek(&tx, &root_secret, user_id, |user_id, dek| {
        let rows = read_profile_rows(&tx, user_id)?;
        if profile_rows_revision(&rows) != revision {
            return Err(PROFILES_CHANGED.to_string());
        }
        let mut sent = decrypt_profile_rows(&rows, dek)?;
        apply(&mut sent);
        let stored = insert_profiles_with_dek(&tx, &root_secret, user_id, dek, &sent)?;
        Ok((sent, stored))
    })?;
    tx.commit()
        .map_err(|e| format!("Commit import write: {e}"))?;
    Ok(written)
}

/// Overwrite server profiles for a specific user id without changing
/// active_user_id. Companion of [`list_server_profiles_for`] (MU-3).
pub fn replace_server_profiles_for(
    conn: &mut Connection,
    root_key: &[u8; 32],
    user_id: i64,
    profiles: &[Value],
) -> Result<Vec<Value>, String> {
    let _lock = ProfileListLock::acquire(conn)?;
    replace_server_profiles_under_lock(conn, root_key, user_id, profiles)
}

fn replace_server_profiles_under_lock(
    conn: &mut Connection,
    root_key: &[u8; 32],
    user_id: i64,
    profiles: &[Value],
) -> Result<Vec<Value>, String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        write_profiles_with_dek(conn, &root_secret, user_id, dek, profiles)
    })
}

/// [`replace_active_server_profiles`] for a caller that already holds
/// [`ProfileListLock`]: the keystore restore, which holds it for the whole import.
pub(crate) fn replace_active_server_profiles_under_lock(
    conn: &mut Connection,
    root_key: &[u8; 32],
    profiles: &[Value],
) -> Result<Vec<Value>, String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    replace_server_profiles_under_lock(conn, root_key, user_id, profiles)
}

/// Overwrite every server profile row for `user_id` with an already-resolved
/// DEK. Session-free companion of [`read_profiles_with_dek`]: the caller
/// supplies both the `dek` (content key) and `root_secret` (used to derive the
/// deterministic `profile_uid` / `dedup_key` tags). Same DELETE-then-reinsert
/// semantics as [`replace_server_profiles_for`].
fn write_profiles_with_dek(
    conn: &Connection,
    root_secret: &SecretKey,
    user_id: i64,
    dek: &SecretKey,
    profiles: &[Value],
) -> Result<Vec<Value>, String> {
    // IMMEDIATE: the write reads the list it replaces before its DELETE. In a
    // deferred transaction on this WAL database, a save committed by another
    // process between that read and the DELETE leaves a stale snapshot, and
    // the DELETE fails with SQLITE_BUSY_SNAPSHOT, which no busy timeout waits
    // out. Taken after the session lock, as every partition writer does.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Start replace profiles: {e}"))?;
    let stored = insert_profiles_with_dek(&tx, root_secret, user_id, dek, profiles)?;
    tx.commit()
        .map_err(|e| format!("Commit replace profiles: {e}"))?;
    Ok(stored)
}

/// The rows of [`write_profiles_with_dek`], on a transaction the caller holds
/// and commits. Every write of a profile list ends here, so this is where no
/// two profiles of one name get stored: a profile created, edited or imported
/// onto a name another one holds gets the first free " (1)", " (2)", ...
/// (`util::profile_name::unique_profile_names`, against the list being
/// replaced). Returns the list stored.
fn insert_profiles_with_dek(
    tx: &Connection,
    root_secret: &SecretKey,
    user_id: i64,
    dek: &SecretKey,
    profiles: &[Value],
) -> Result<Vec<Value>, String> {
    // The list being replaced tells which profile had a name first. One that
    // cannot be read leaves the order of `profiles` to decide.
    let previous = read_profiles_with_dek(tx, user_id, dek).unwrap_or_else(|e| {
        tracing::warn!("Profile names checked without the previous list: {e}");
        Vec::new()
    });
    let (profiles, renamed) = crate::util::profile_name::unique_profile_names(&previous, profiles);
    for r in &renamed {
        tracing::info!(
            "Profile {} saved as {:?}: {:?} is already another profile's name",
            r.id,
            r.to,
            r.from
        );
    }
    insert_profile_rows(tx, root_secret, user_id, dek, &profiles)?;
    Ok(profiles)
}

/// `user_id`'s rows replaced by `profiles`, as given. Production writes reach
/// it only through [`insert_profiles_with_dek`], which settles the names
/// first; a test calls it to lay down a list an older build stored.
fn insert_profile_rows(
    tx: &Connection,
    root_secret: &SecretKey,
    user_id: i64,
    dek: &SecretKey,
    profiles: &[Value],
) -> Result<(), String> {
    tx.execute(
        "DELETE FROM server_profiles WHERE user_id = ?1",
        params![user_id],
    )
    .map_err(|e| format!("Delete previous profiles: {e}"))?;

    let now = now_ms();
    let mut seen_uids = HashSet::new();
    for (index, profile) in profiles.iter().enumerate() {
        let uid_seed = profile_uid_seed(profile, index, &mut seen_uids);
        let uid = user_crypto::metadata_tag(root_secret, b"profile-uid", &uid_seed)?;
        let token_uid = profile_token_tag(root_secret, &uid_seed)?;
        let key = profile_dedup_key(root_secret, profile, &uid_seed)?;
        let (encrypted_blob, nonce) = encrypt_value(dek, profile)?;
        tx.execute(
            "INSERT INTO server_profiles(
                 user_id, profile_uid, dedup_key, name, encrypted_blob, nonce,
                 aead_alg, created_at, updated_at, token_uid
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'aes-256-gcm', ?7, ?7, ?8)",
            params![
                user_id,
                uid,
                key,
                uid,
                encrypted_blob,
                nonce,
                now,
                token_uid
            ],
        )
        .map_err(|e| format!("Insert profile: {e}"))?;
    }
    // Every tag of this list is now this machine's.
    tx.execute(
        "DELETE FROM global_state WHERE key = ?1",
        params![profile_tags_pending_key(user_id)],
    )
    .map_err(|e| format!("Clear pending profile tags: {e}"))?;
    crate::bridge_import_scan::adopt_oauth_apps(tx, profiles)?;
    Ok(())
}

/// Marks an account whose profile tags may have been made with another
/// machine's key (a keystore restore of another machine's partition): until
/// its list is written here again, a holder lookup does not see its profiles.
const PROFILE_TAGS_PENDING_PREFIX: &str = "profile_tags_pending_";

fn profile_tags_pending_key(user_id: i64) -> String {
    format!("{PROFILE_TAGS_PENDING_PREFIX}{user_id}")
}

/// Whether an existing account's profile tags are still pending: a lookup of
/// who holds a profile id cannot be trusted until they are rebuilt.
fn profile_tags_pending(conn: &Connection) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM global_state g JOIN users u
         ON g.key = ?1 || u.id)",
        params![PROFILE_TAGS_PENDING_PREFIX],
        |row| row.get(0),
    )
    .map_err(|e| format!("Read pending profile tags: {e}"))
}

/// After a keystore restore wrote `user_partitions.db` whole: the profile
/// tags (`profile_uid`, `dedup_key`) are keyed by the root of the machine the
/// backup was made on, and the transport re-key only re-wraps the DEKs. Every
/// account this machine's key opens gets its list written again, which tags
/// it with this machine's root; an account it cannot open now (a passphrase
/// account, or one with no usable transport DEK) is marked pending, and is
/// tagged by the first write of its list or at its unlock
/// ([`migrate_credentials_for_user`]). While any is pending, the passes that
/// infer from the tags which profiles another account holds wait. A restore
/// of this machine's own partition (an account already opened by this key,
/// none re-keyed or unreadable) has this machine's tags and is left alone.
/// Returns the accounts left pending.
pub fn retag_restored_profiles(
    conn: &Connection,
    local_root_key: &[u8; 32],
    rekey: &TransportRekeyReport,
) -> Result<usize, String> {
    if rekey.already_local > 0 && rekey.rekeyed == 0 && rekey.unreadable == 0 {
        return Ok(0);
    }
    let root_secret = user_crypto::secret_key_from_bytes(local_root_key);
    let mut pending = 0;
    for user in list_users(conn)? {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Start profile tags: {e}"))?;
        let row = read_user_key_row(&tx, user.id)?;
        let opened = if row.has_passphrase {
            None
        } else {
            unwrap_user_dek_with_key(&row, &root_secret).ok()
        };
        let listed = opened.and_then(|dek| {
            read_profiles_with_dek(&tx, user.id, &dek)
                .ok()
                .map(|profiles| (profiles, dek))
        });
        match listed {
            Some((profiles, dek)) => {
                insert_profile_rows(&tx, &root_secret, user.id, &dek, &profiles)?
            }
            None => {
                upsert_global_state(&tx, &profile_tags_pending_key(user.id), "1", now_ms())?;
                pending += 1;
            }
        }
        tx.commit()
            .map_err(|e| format!("Commit profile tags: {e}"))?;
    }
    Ok(pending)
}

/// A pending account's list tagged with this machine's root, now that its
/// DEK is reachable. Nothing to do when its tags are not pending, or while
/// the account is locked or waits for a security reset.
fn retag_own_profiles_if_pending(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
) -> Result<(), String> {
    let pending = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM global_state WHERE key = ?1)",
            params![profile_tags_pending_key(user_id)],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|e| format!("Read pending profile tags: {e}"))?;
    if !pending {
        return Ok(());
    }
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let retagged = with_user_dek(conn, &root_secret, user_id, |uid, dek| {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Start profile tags: {e}"))?;
        let profiles = read_profiles_with_dek(&tx, uid, dek)?;
        insert_profile_rows(&tx, &root_secret, uid, dek, &profiles)?;
        tx.commit().map_err(|e| format!("Commit profile tags: {e}"))
    });
    match retagged {
        Err(e) if e == "USER_LOCKED" || e == "SECURITY_RESET_PENDING" => Ok(()),
        other => other,
    }
}

/// Gives the rows of `user_id`'s list written before profiles had a token
/// tag ([`profile_token_tag`]) theirs, now that its DEK is reachable. Only
/// that column is written: the list, its bytes and so its revision stay as
/// they are. Nothing to do when every row has one, or while the account is
/// locked or waits for a security reset; until then the keys of the other
/// accounts that lowercase a profile id wait for it, and nothing else does
/// ([`align_private_tokens_of`]).
fn fill_profile_token_tags(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
) -> Result<(), String> {
    let missing: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM server_profiles
             WHERE user_id = ?1 AND token_uid IS NULL)",
            params![user_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Read profile token tags: {e}"))?;
    if !missing {
        return Ok(());
    }
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let filled = with_user_dek(conn, &root_secret, user_id, |uid, dek| {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Start profile token tags: {e}"))?;
        let rows: Vec<(i64, Vec<u8>, Vec<u8>)> = tx
            .prepare(
                "SELECT id, encrypted_blob, nonce FROM server_profiles
                 WHERE user_id = ?1 ORDER BY id ASC",
            )
            .and_then(|mut statement| {
                statement
                    .query_map(params![uid], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .map_err(|e| format!("Read profile rows: {e}"))?;
        // The seeds [`insert_profile_rows`] tagged the rows with, in its order.
        let mut seen = HashSet::new();
        for (index, (row_id, encrypted_blob, nonce)) in rows.iter().enumerate() {
            let profile = decrypt_value(dek, nonce, encrypted_blob)?;
            let seed = profile_uid_seed(&profile, index, &mut seen);
            tx.execute(
                "UPDATE server_profiles SET token_uid = ?1 WHERE id = ?2",
                params![profile_token_tag(&root_secret, &seed)?, row_id],
            )
            .map_err(|e| format!("Write profile token tag: {e}"))?;
        }
        tx.commit()
            .map_err(|e| format!("Commit profile token tags: {e}"))
    });
    match filled {
        Err(e) if e == "USER_LOCKED" || e == "SECURITY_RESET_PENDING" => Ok(()),
        other => other,
    }
}

/// An account other than the legacy owner whose passes read from the tags
/// which profiles the others hold waits while any account's tags are
/// pending ([`retag_restored_profiles`]). The owner is read only while tags
/// are pending: a reader of an account's own row consults it no sooner.
fn waits_for_profile_tags(conn: &Connection, user_id: i64) -> Result<bool, String> {
    Ok(profile_tags_pending(conn)? && nonprofile_secret_owner_unpersisted(conn)? != Some(user_id))
}

/// Puts right the profile lists of a partition database written whole rather
/// than through the writes above (a keystore restore of a backup made before
/// profile names were unique): in a list that holds two profiles of one name
/// the first keeps it and the others get " (1)", " (2)", ..., which is what
/// that list's next write would store. Covers every account this machine's
/// key opens; a passphrase account is put right by its first write after
/// unlock. Returns the profiles renamed and the passphrase accounts left to
/// that write. The caller, a keystore restore, holds [`ProfileListLock`].
pub fn put_right_profile_names_in_db(
    store: &CredentialStore,
    db_path: &std::path::Path,
) -> Result<(usize, usize), String> {
    if !db_path.is_file() {
        return Ok((0, 0));
    }
    let mut conn = open_restored_db(db_path)?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = put_right_profile_names(&mut conn, &root_key);
    root_key.zeroize();
    result
}

fn put_right_profile_names(
    conn: &mut Connection,
    root_key: &[u8; 32],
) -> Result<(usize, usize), String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let (mut renamed, mut left) = (0, 0);
    for user in list_users(conn)? {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("Start profile name check: {e}"))?;
        let row = read_user_key_row(&tx, user.id)?;
        if row.has_passphrase {
            left += 1;
            continue;
        }
        // An account this machine cannot open is reported by the re-key.
        let Ok(dek) = unwrap_user_dek_with_key(&row, &root_secret) else {
            continue;
        };
        let profiles = read_profiles_with_dek(&tx, user.id, &dek)?;
        let (_, renames) = crate::util::profile_name::unique_profile_names(&profiles, &profiles);
        if renames.is_empty() {
            continue;
        }
        insert_profiles_with_dek(&tx, &root_secret, user.id, &dek, &profiles)?;
        tx.commit()
            .map_err(|e| format!("Commit profile names: {e}"))?;
        renamed += renames.len();
    }
    Ok((renamed, left))
}

/// After a keystore restore wrote `user_partitions.db` whole: bring every
/// profile of every account this machine's key opens to its safe state
/// ([`crate::profile_security::make_profile_list_safe`]), so no option from the
/// backup turns a security check off (CodeRabbit on #1524). Returns what was
/// reset and the passphrase accounts that cannot be opened here. The caller, a
/// keystore restore, holds [`ProfileListLock`].
#[derive(Debug, Default)]
pub struct ProfileResetReport {
    pub reset: Vec<crate::profile_security::ProfileReset>,
    pub left: usize,
    pub failed: usize,
    pub quarantine_path: Option<PathBuf>,
    persistent_failure: bool,
}

fn security_reset_pending_key(user_id: i64) -> String {
    format!("security_reset_pending_{user_id}")
}

fn security_reset_pending(conn: &Connection, user_id: i64) -> Result<bool, String> {
    conn.query_row(
        "SELECT 1 FROM global_state WHERE key = ?1",
        params![security_reset_pending_key(user_id)],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
    .map_err(|e| format!("Read pending security reset: {e}"))
}

#[cfg(test)]
thread_local! {
    pub(crate) static FAIL_RESET_PERSISTENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static RESET_PERSISTENCE_FAILURES: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn forget_security_reset_blocks_for_tests() {
    assert_test_lock_held();
    SECURITY_RESET_BLOCKS.lock().unwrap().clear();
    clear_user_session();
}

static SECURITY_RESET_BLOCKS: std::sync::LazyLock<Mutex<HashSet<(String, i64)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));

fn security_reset_block_key(conn: &Connection, user_id: i64) -> (String, i64) {
    let database = conn
        .path()
        .filter(|database| !database.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("memory:{:p}", conn));
    (database, user_id)
}

fn security_reset_marker_path(conn: &Connection, user_id: i64) -> Option<PathBuf> {
    conn.path()
        .filter(|database| !database.is_empty())
        .map(|database| PathBuf::from(format!("{database}.security-reset-blocked-{user_id}")))
}

fn block_security_reset(conn: &Connection, user_id: i64) {
    if let Ok(mut blocks) = SECURITY_RESET_BLOCKS.lock() {
        blocks.insert(security_reset_block_key(conn, user_id));
    }
    #[cfg(test)]
    if FAIL_RESET_PERSISTENCE.get() {
        RESET_PERSISTENCE_FAILURES.set(RESET_PERSISTENCE_FAILURES.get() | 1);
        return;
    }
    if let Some(path) = security_reset_marker_path(conn, user_id) {
        if let Err(e) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            tracing::warn!("Persistent security reset block {}: {e}", path.display());
        }
    }
}

fn unblock_security_reset(conn: &Connection, user_id: i64) {
    if let Some(path) = security_reset_marker_path(conn, user_id) {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(
                    "Clear persistent security reset block {}: {e}",
                    path.display()
                );
                return;
            }
        }
    }
    if let Ok(mut blocks) = SECURITY_RESET_BLOCKS.lock() {
        blocks.remove(&security_reset_block_key(conn, user_id));
    }
}

fn security_reset_blocked(conn: &Connection, user_id: i64) -> bool {
    let blocked = SECURITY_RESET_BLOCKS
        .lock()
        .map(|blocks| blocks.contains(&security_reset_block_key(conn, user_id)))
        .unwrap_or(true);
    blocked || security_reset_marker_path(conn, user_id).is_some_and(|path| path.exists())
}

fn mark_security_reset_pending(conn: &mut Connection, user_id: i64) -> Result<(), String> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Start pending security reset: {e}"))?;
    upsert_global_state(&tx, &security_reset_pending_key(user_id), "1", now_ms())?;
    tx.commit()
        .map_err(|e| format!("Commit pending security reset: {e}"))
}

fn quarantine_security_reset_db(path: &std::path::Path) -> Result<PathBuf, String> {
    #[cfg(test)]
    if FAIL_RESET_PERSISTENCE.get() {
        RESET_PERSISTENCE_FAILURES.set(RESET_PERSISTENCE_FAILURES.get() | 2);
        return Err("forced quarantine rename failure".to_string());
    }
    quarantine_security_reset_db_with(path, |source, destination| {
        std::fs::rename(source, destination)
    })
}

fn quarantine_security_reset_db_with(
    path: &std::path::Path,
    rename: impl Fn(&std::path::Path, &std::path::Path) -> std::io::Result<()>,
) -> Result<PathBuf, String> {
    let target = PathBuf::from(format!(
        "{}.security-reset-failed-{}",
        path.display(),
        uuid::Uuid::new_v4()
    ));
    for suffix in ["", "-wal", "-shm"] {
        let source = PathBuf::from(format!("{}{suffix}", path.display()));
        if source.exists() {
            let destination = PathBuf::from(format!("{}{suffix}", target.display()));
            rename(&source, &destination).map_err(|e| {
                format!(
                    "Quarantine restored partition database {}: {e}",
                    source.display()
                )
            })?;
        }
    }
    tracing::warn!(
        "Restored partition database preserved in quarantine at {}",
        target.display()
    );
    Ok(target)
}

pub fn reset_profile_relaxations_in_db(
    store: &CredentialStore,
    db_path: &std::path::Path,
    except_user: Option<i64>,
) -> Result<ProfileResetReport, String> {
    if !db_path.is_file() {
        return Ok(ProfileResetReport::default());
    }
    let mut conn = open_restored_db(db_path)?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let mut result = reset_profile_relaxations(&mut conn, &root_key, except_user);
    root_key.zeroize();
    let needs_quarantine = result
        .as_ref()
        .is_ok_and(|report| report.persistent_failure);
    if needs_quarantine {
        drop(conn);
        let quarantine_path = quarantine_security_reset_db(db_path)?;
        if let Ok(report) = result.as_mut() {
            report.quarantine_path = Some(quarantine_path);
        }
    }
    result
}

fn reset_profile_relaxations(
    conn: &mut Connection,
    root_key: &[u8; 32],
    except_user: Option<i64>,
) -> Result<ProfileResetReport, String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let mut report = ProfileResetReport::default();
    for user in list_users(conn)? {
        if except_user == Some(user.id) {
            continue;
        }
        let database_path = conn.path().map(str::to_string);
        let tx = match conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) {
            Ok(tx) => tx,
            Err(e) => {
                report.failed += 1;
                clear_user_session();
                tracing::warn!("Security reset: account {}: {e}", user.id);
                if let Some(path) = database_path {
                    match Connection::open(path) {
                        Ok(mut error_conn) => {
                            block_security_reset(&error_conn, user.id);
                            if let Err(marker) =
                                mark_security_reset_pending(&mut error_conn, user.id)
                            {
                                report.persistent_failure = true;
                                tracing::warn!(
                                    "Security reset start marker: account {}: {marker}",
                                    user.id
                                );
                            }
                        }
                        Err(marker) => {
                            report.persistent_failure = true;
                            tracing::warn!(
                                "Security reset start connection: account {}: {marker}",
                                user.id
                            );
                        }
                    }
                }
                continue;
            }
        };
        let row = match read_user_key_row(&tx, user.id) {
            Ok(row) => row,
            Err(e) => {
                report.failed += 1;
                tracing::warn!("Security reset: account {}: {e}", user.id);
                continue;
            }
        };
        if row.has_passphrase {
            report.left += 1;
            let marked =
                upsert_global_state(&tx, &security_reset_pending_key(user.id), "1", now_ms())
                    .and_then(|_| {
                        tx.commit()
                            .map_err(|e| format!("Commit pending reset: {e}"))
                    });
            if let Err(e) = marked {
                report.failed += 1;
                report.persistent_failure = true;
                block_security_reset(conn, user.id);
                clear_user_session();
                tracing::warn!("Security reset marker: account {}: {e}", user.id);
            }
            continue;
        }
        let Ok(dek) = unwrap_user_dek_with_key(&row, &root_secret) else {
            report.left += 1;
            continue;
        };
        let mut profiles = match read_profiles_with_dek(&tx, user.id, &dek) {
            Ok(profiles) => profiles,
            Err(e) => {
                report.failed += 1;
                tracing::warn!("Security reset: account {}: {e}", user.id);
                continue;
            }
        };
        let changed = crate::profile_security::make_profile_list_safe(&mut profiles);
        if changed.is_empty() {
            continue;
        }
        let written = insert_profiles_with_dek(&tx, &root_secret, user.id, &dek, &profiles)
            .and_then(|_| {
                tx.execute(
                    "DELETE FROM global_state WHERE key = ?1",
                    params![security_reset_pending_key(user.id)],
                )
                .map(|_| ())
                .map_err(|e| format!("Clear completed security reset: {e}"))
            });
        if let Err(e) = written {
            drop(tx);
            report.failed += 1;
            block_security_reset(conn, user.id);
            clear_user_session();
            tracing::warn!("Security reset: account {}: {e}", user.id);
            if let Err(e) = mark_security_reset_pending(conn, user.id) {
                report.persistent_failure = true;
                tracing::warn!("Security reset rollback marker: account {}: {e}", user.id);
            }
            continue;
        }
        match tx.commit() {
            Ok(_) => {
                unblock_security_reset(conn, user.id);
                report.reset.extend(changed);
            }
            Err(e) => {
                report.failed += 1;
                block_security_reset(conn, user.id);
                clear_user_session();
                tracing::warn!("Security reset: account {}: {e}", user.id);
                if let Err(e) = mark_security_reset_pending(conn, user.id) {
                    report.persistent_failure = true;
                    tracing::warn!("Security reset rollback marker: account {}: {e}", user.id);
                }
            }
        }
    }
    Ok(report)
}

/// Re-read `user_id`'s server profiles, let `apply` change them, and write
/// them back only when it reports a change, all in one IMMEDIATE transaction:
/// no other writer (the CLI, another window) can save between the read and the
/// write and have that save overwritten. Returns the stored list afterwards,
/// the one written or, when `apply` reports no change, the one read (whatever
/// `apply` did to its copy), and whether it was written.
pub fn update_server_profiles_for(
    conn: &mut Connection,
    root_key: &[u8; 32],
    user_id: i64,
    apply: impl FnOnce(&mut Vec<Value>) -> bool,
) -> Result<(Vec<Value>, bool), String> {
    let _lock = ProfileListLock::acquire(conn)?;
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    // The session before the reservation ([`SessionHold`]).
    let _session = hold_user_session(conn, user_id);
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Start update profiles: {e}"))?;
    let (stored, written) = with_user_dek(&tx, &root_secret, user_id, |user_id, dek| {
        let read = read_profiles_with_dek(&tx, user_id, dek)?;
        let mut profiles = read.clone();
        if !apply(&mut profiles) {
            return Ok((read, false));
        }
        let stored = insert_profiles_with_dek(&tx, &root_secret, user_id, dek, &profiles)?;
        Ok((stored, true))
    })?;
    if written {
        tx.commit()
            .map_err(|e| format!("Commit update profiles: {e}"))?;
    }
    Ok((stored, written))
}

/// [`update_server_profiles_for`] for the active user.
pub fn update_active_server_profiles(
    conn: &mut Connection,
    root_key: &[u8; 32],
    apply: impl FnOnce(&mut Vec<Value>) -> bool,
) -> Result<(Vec<Value>, bool), String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    update_server_profiles_for(conn, root_key, user_id, apply)
}

/// The prefix of a relocation error that came after its Move committed: the
/// profile is saved in the target and gone from the source, and only a
/// cleanup of the source (its Proton session) failed. A caller reports the
/// Move as done, with the error as a warning ([`relocation_committed`]).
pub const RELOCATION_COMMITTED: &str = "RELOCATION_COMMITTED";

/// Whether `error`, a relocation's, came after its Move committed
/// ([`RELOCATION_COMMITTED`]).
pub fn relocation_committed(error: &str) -> bool {
    error.starts_with(RELOCATION_COMMITTED)
}

/// Outcome of a cross-user profile relocation (N4). Production GUI/CLI callers
/// receive it after the shared profile/credential operation has completed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRelocation {
    /// Source profile id, as it existed in the source partition.
    pub source_profile_id: String,
    /// Fresh id the relocated copy received in the target partition. Equal to
    /// `source_profile_id` only when the caller deliberately reuses it.
    pub new_profile_id: String,
    /// Display name of the relocated profile (for the confirmation toast): the
    /// name the target stored it under, which is " (1)", " (2)", ... when a
    /// profile there already holds the source's name.
    pub profile_name: String,
    /// Target user the profile now lives in.
    pub target_user_id: i64,
    /// True for Move/Cut (the source row was deleted), false for Copy.
    pub moved: bool,
    /// True when the target partition already contained the same *account*
    /// (relocate identity surface + credential fingerprint; EF-19). For a Copy
    /// this means the insert was skipped to avoid a duplicate; for a Move the
    /// profile is still materialised (see `inserted`) so the source can be
    /// removed without losing the only copy.
    pub already_present: bool,
    /// True when a fresh profile row was actually written into the target
    /// partition. Always true for a Move (it must materialise before the source
    /// is deleted, #366), and true for a Copy unless `already_present` skipped
    /// it. Gates the credential mirror so a moved profile keeps its secret.
    pub inserted: bool,
    /// Wire protocol of the source profile (e.g. `pcloud`, `jottacloud`, `sftp`).
    /// Populated by [`relocate_server_profile`] so the credential dual can resolve
    /// OAuth / Jottacloud vault keys via [`relocate_credential_key_candidates`]
    /// without changing caller signatures. Empty when unknown (serde default).
    #[serde(default)]
    pub protocol: String,
    /// Secrets of the profile that did not reach the target, by key. The profile is
    /// there, but it will ask for them again; on a Move they stay with the source.
    #[serde(default)]
    pub secrets_not_copied: Vec<String>,
}

/// Copy or move a single server profile from `source_user_id` into
/// `target_user_id` (N4, Ehud wishlist #270). The backend stays authoritative:
/// the caller passes only an id, the genuine source blob is read from the
/// vault, re-keyed under `new_profile_id`, and inserted into the target
/// partition. This partition-only primitive also supports existing unit
/// fixtures; GUI/CLI production entry points use the combined guarded operation
/// below to relocate credentials together with these rows.
///
/// Security: writing into a passphrase-protected target requires its
/// passphrase (`TARGET_PASSPHRASE_REQUIRED` otherwise); the target DEK is
/// resolved session-free so the source user's primed session is never
/// disturbed. When the target already holds the same account (surface +
/// credential fingerprint; EF-19) the Copy insert is skipped (`already_present`);
/// for `remove_from_source` a Move always materialises first (#366) so the
/// source can be deleted without losing the only copy.
#[allow(clippy::too_many_arguments)]
pub fn relocate_server_profile(
    conn: &mut Connection,
    root_key: &[u8; 32],
    source_user_id: i64,
    target_user_id: i64,
    profile_id: &str,
    new_profile_id: &str,
    target_passphrase: Option<&str>,
    remove_from_source: bool,
) -> Result<ProfileRelocation, String> {
    let _lock = ProfileListLock::acquire(conn)?;
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let target_dek =
        resolve_user_dek_scoped(conn, &root_secret, target_user_id, target_passphrase)?;
    with_user_dek(conn, &root_secret, source_user_id, |_, source_dek| {
        let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| format!("Start profile relocation: {error}"))?;
        let relocation = relocate_server_profile_in_transaction(
            &tx,
            root_key,
            &root_secret,
            source_user_id,
            source_dek,
            target_user_id,
            &target_dek,
            profile_id,
            new_profile_id,
            remove_from_source,
        )?;
        tx.commit()
            .map_err(|error| format!("Commit profile relocation: {error}"))?;
        Ok(relocation)
    })
}

/// Production GUI/CLI entry point. A native Proton Drive session belongs to
/// one account, so it never travels: the target is told the profile asks to
/// sign in again (`secrets_not_copied`), and a Move drops the source's copy
/// locally, without a sign-out on Proton that would end it for any other
/// profile of the source account holding it. The remaining per-profile
/// secrets keep their existing best-effort dual after the partition commit.
#[allow(clippy::too_many_arguments)]
fn relocate_server_profile_and_credentials(
    conn: &mut Connection,
    store: &CredentialStore,
    root_key: &[u8; 32],
    source_user_id: i64,
    target_user_id: i64,
    profile_id: &str,
    new_profile_id: &str,
    target_passphrase: Option<&str>,
    remove_from_source: bool,
) -> Result<ProfileRelocation, String> {
    if source_user_id == target_user_id {
        return Err("RELOCATE_SAME_USER".into());
    }
    if new_profile_id.trim().is_empty() {
        return Err("NEW_PROFILE_ID_REQUIRED".into());
    }
    let _lock = ProfileListLock::acquire(conn)?;
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let target_dek =
        resolve_user_dek_scoped(conn, &root_secret, target_user_id, target_passphrase)?;
    let source_key = format!("{PROTON_SESSION_PREFIX}{profile_id}");
    let new_key = format!("{PROTON_SESSION_PREFIX}{new_profile_id}");
    // A Move of a native Proton profile, or of one that holds a session key,
    // holds the source's session from before it looks whether there is one
    // until it has removed it: a sign-in, a restore or a refresh of the
    // profile in between waits for the Move, and a lease it cannot take stops
    // the Move here, before anything changed. A Proton profile with no session
    // yet is held too, since a sign-in of it may be open. Any other profile
    // takes no Proton lock. Taken outside any session hold, since a refresh
    // in flight may need the account's session to save.
    #[cfg(feature = "proton-native")]
    let held = (remove_from_source
        && moves_a_proton_session(
            conn,
            store,
            root_key,
            source_user_id,
            profile_id,
            &source_key,
        )?)
    .then(|| {
        use crate::providers::proton_native::vault;
        vault::hold_session_blocking(
            vault::SessionOwner::for_user(Some(source_user_id)),
            profile_id,
        )
    })
    .transpose()
    .map_err(|error| error.to_string())?;
    // The source's session, read as its own readers read it: its row, or the
    // vault copy only for the legacy owner.
    let source_has_session =
        with_user_dek(conn, &root_secret, source_user_id, |_, dek| {
            Ok(read_credential_with_dek(conn, source_user_id, dek, &source_key)?.is_some())
        })? || (!vault_copy_is_another_accounts(conn, source_user_id, &source_key)?
            && get_optional_secret(store, &source_key)?.is_some());
    #[cfg(test)]
    relocation_looked();
    let mut relocation = with_user_dek(conn, &root_secret, source_user_id, |_, source_dek| {
        let tx = Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| format!("Start profile relocation: {error}"))?;
        let relocation = relocate_server_profile_in_transaction(
            &tx,
            root_key,
            &root_secret,
            source_user_id,
            source_dek,
            target_user_id,
            &target_dek,
            profile_id,
            new_profile_id,
            remove_from_source,
        )?;
        // Without native Proton nothing refreshes the session, so its row goes
        // with the profile.
        #[cfg(not(feature = "proton-native"))]
        if relocation.moved {
            delete_user_credential_for(&tx, source_user_id, &source_key)?;
        }
        // A sign-in of the profile still open when the Move commits ends after
        // it, under the key the Move holds until it returns: marked with the
        // list it leaves, the id is refused that sign-in's save.
        #[cfg(feature = "proton-native")]
        if relocation.moved {
            mark_moved_away(&tx, source_user_id, profile_id)?;
        }
        tx.commit()
            .map_err(|error| format!("Commit profile relocation: {error}"))?;
        Ok(relocation)
    })?;
    // This loop skips native sessions entirely.
    relocation.secrets_not_copied = relocate_server_credential_dual(
        conn,
        store,
        root_key,
        source_user_id,
        &relocation,
        target_passphrase,
    );
    if source_has_session && relocation.inserted {
        relocation.secrets_not_copied.push(new_key);
    }
    // A Move drops the source's session locally under the hold it took before
    // it looked: a refresh in flight saved its pair before, and nothing writes
    // the session back into the account the profile left. The legacy owner's
    // vault copy goes with it.
    #[cfg(feature = "proton-native")]
    if let Some(held) = held
        .as_ref()
        .filter(|_| source_has_session && relocation.moved)
    {
        held.discard()
            .map_err(|error| {
                format!(
                    "{RELOCATION_COMMITTED}: profile {} is saved in target user {}; removal of the source's Proton session failed: {error}",
                    relocation.new_profile_id, target_user_id,
                )
            })?;
    }
    #[cfg(not(feature = "proton-native"))]
    if source_has_session && relocation.moved {
        // The Move already committed: an ownership lookup that fails is
        // reported like the vault deletion's, and the vault entry stays.
        let vault_is_theirs = vault_copy_is_another_accounts(conn, source_user_id, &source_key)
            .map_err(|error| {
                format!(
                    "{RELOCATION_COMMITTED}: profile {} is saved in target user {}; the vault copy's owner could not be read: {error}",
                    relocation.new_profile_id, target_user_id,
                )
            })?;
        if !vault_is_theirs {
            delete_optional_secret(store, &source_key).map_err(|error| {
                format!(
                    "{RELOCATION_COMMITTED}: profile {} is saved in target user {}; removal of the source's Proton session from the vault failed: {error}",
                    relocation.new_profile_id, target_user_id,
                )
            })?;
        }
    }
    Ok(relocation)
}

/// Whether the Move of `profile_id` out of `source_user_id` may carry a native
/// Proton session, and so holds it: the profile is a Proton one in the source's
/// list, read under the profile list lock the Move holds, or the source holds
/// its session key, seen without decrypting it.
#[cfg(feature = "proton-native")]
fn moves_a_proton_session(
    conn: &Connection,
    store: &CredentialStore,
    root_key: &[u8; 32],
    source_user_id: i64,
    profile_id: &str,
    session_key: &str,
) -> Result<bool, String> {
    let proton = list_server_profiles_for(conn, root_key, source_user_id)?
        .iter()
        .find(|profile| value_str(profile, &["id", "uid", "profileUid"]) == Some(profile_id))
        .and_then(|profile| value_str(profile, &["protocol"]))
        .is_some_and(|protocol| protocol.eq_ignore_ascii_case("proton"));
    Ok(proton
        || partition_row_exists(conn, source_user_id, session_key)?
        || (!vault_copy_is_another_accounts(conn, source_user_id, session_key)?
            && get_optional_secret(store, session_key)?.is_some()))
}

/// Marks a profile id a Move took out of an account ([`profile_moved_away`]).
#[cfg(feature = "proton-native")]
const MOVED_AWAY_PREFIX: &str = "proton_profile_moved_away_";

#[cfg(feature = "proton-native")]
fn moved_away_key(user_id: i64, profile_id: &str) -> String {
    format!("{MOVED_AWAY_PREFIX}{user_id}_{profile_id}")
}

#[cfg(feature = "proton-native")]
fn mark_moved_away(conn: &Connection, user_id: i64, profile_id: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO global_state(key, value, updated_at)
         VALUES (?1, '1', ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![moved_away_key(user_id, profile_id), now_ms()],
    )
    .map_err(|e| format!("Write moved profile marker: {e}"))?;
    Ok(())
}

/// Whether a Move took profile `profile_id` out of account `user` and the
/// account has not saved it again since: a sign-in begun before the Move and
/// ended after it would save a session for a profile the account no longer
/// has. An id the account never had is not marked, so a sign-in of a profile
/// not saved yet goes ahead. The mark goes once the account lists the id.
#[cfg(feature = "proton-native")]
pub(crate) fn profile_moved_away(
    store: &CredentialStore,
    user: i64,
    profile_id: &str,
) -> Result<bool, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let key = moved_away_key(user, profile_id);
    let marked = conn
        .query_row(
            "SELECT 1 FROM global_state WHERE key = ?1",
            params![key],
            |_| Ok(()),
        )
        .optional()
        .map_err(|e| format!("Read moved profile marker: {e}"))?
        .is_some();
    if !marked {
        return Ok(false);
    }
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let listed = list_server_profiles_for(&conn, &root_key, user)?
        .iter()
        .any(|profile| value_str(profile, &["id", "uid", "profileUid"]) == Some(profile_id));
    if listed {
        conn.execute("DELETE FROM global_state WHERE key = ?1", params![key])
            .map_err(|e| format!("Clear moved profile marker: {e}"))?;
    }
    Ok(!listed)
}

#[cfg(test)]
thread_local! {
    /// Runs once on the thread of a Copy or a Move, after it looked whether the
    /// source holds a native session and before it commits, for a test to
    /// play another writer there.
    static RELOCATION_LOOKED: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

#[cfg(test)]
fn relocation_looked() {
    if let Some(hook) = RELOCATION_LOOKED.take() {
        hook();
    }
}

/// Both profile lists are changed in the caller's transaction, after the
/// source session is held. Production relocation also writes the native
/// credential in this transaction while its key and UID guards remain held.
#[allow(clippy::too_many_arguments)]
fn relocate_server_profile_in_transaction(
    conn: &Connection,
    root_key: &[u8; 32],
    root_secret: &SecretKey,
    source_user_id: i64,
    source_dek: &SecretKey,
    target_user_id: i64,
    target_dek: &SecretKey,
    profile_id: &str,
    new_profile_id: &str,
    remove_from_source: bool,
) -> Result<ProfileRelocation, String> {
    if source_user_id == target_user_id {
        return Err("RELOCATE_SAME_USER".to_string());
    }
    if new_profile_id.trim().is_empty() {
        return Err("NEW_PROFILE_ID_REQUIRED".to_string());
    }
    // The target must exist before we read the (potentially large) source list.
    read_user_key_row(conn, target_user_id)?;

    // 1. Read the source (active, already-unlocked) partition via its session.
    let source_profiles = read_profiles_with_dek(conn, source_user_id, source_dek)?;
    let source = source_profiles
        .iter()
        .find(|p| value_str(p, &["id", "uid", "profileUid"]) == Some(profile_id))
        .cloned()
        .ok_or_else(|| format!("PROFILE_NOT_FOUND: {profile_id}"))?;
    let mut profile_name = value_str(&source, &["name", "label", "host", "hostname"])
        .unwrap_or(profile_id)
        .to_string();
    let protocol = value_str(&source, &["protocol"]).unwrap_or("").to_string();

    // 2. Clone the blob under a fresh id and drop the per-account session field.
    let mut relocated = source.clone();
    if let Value::Object(map) = &mut relocated {
        map.insert("id".into(), Value::String(new_profile_id.to_string()));
        map.remove("lastConnected");
    }

    // 4. Account-identity probe against the target (EF-19 Option B).
    //    Do NOT reuse storage `dedup_key` / `profile_dedup_key` here: that key
    //    strips secrets and collapses distinct S3 / OAuth / WebDAV accounts
    //    that share an empty or placeholder blob username into a false
    //    "already saved". Skip a Copy only when the target holds the same
    //    account surface AND the same credential fingerprint.
    let source_seed = value_str(&source, &["id", "uid", "profileUid"]).unwrap_or(profile_id);
    let source_fp = relocate_credential_fingerprint(
        conn,
        root_key,
        root_secret,
        source_user_id,
        Some(source_dek),
        &source,
        source_seed,
    )?;
    let target_profiles = read_profiles_with_dek(conn, target_user_id, target_dek)?;
    let mut already_present = false;
    for existing in &target_profiles {
        let seed = value_str(existing, &["id", "uid", "profileUid"]).unwrap_or("");
        let existing_fp = relocate_credential_fingerprint(
            conn,
            root_key,
            root_secret,
            target_user_id,
            Some(target_dek),
            existing,
            seed,
        )?;
        if relocate_accounts_match(&source, &source_fp, existing, &existing_fp) {
            already_present = true;
            break;
        }
    }

    // 5. Materialise the profile in the target (prepend, like Duplicate).
    //    Copy de-dups: when an equivalent server is already saved there the
    //    insert is skipped to avoid a duplicate. A Move ALWAYS inserts, so the
    //    moved profile is guaranteed to exist in the target before the source
    //    row is removed in step 6. This is the #366 data-loss fix: previously
    //    the insert was skipped on `already_present` while the source was still
    //    deleted, so a stale or false-positive dedup probe destroyed the only
    //    copy. Insert never being skipped on a Move makes that impossible.
    let inserted = if remove_from_source || !already_present {
        let mut new_list = Vec::with_capacity(target_profiles.len() + 1);
        new_list.push(relocated);
        new_list.extend(target_profiles);
        let stored =
            insert_profiles_with_dek(conn, root_secret, target_user_id, target_dek, &new_list)?;
        // A profile of the target that holds the name keeps it; the copy is
        // stored as " (1)", " (2)", ..., and that is the name to confirm.
        if let Some(name) = crate::util::profile_name::name_of(&stored, new_profile_id) {
            profile_name = name.to_string();
        }
        true
    } else {
        false
    };

    // 6. Move/Cut: drop the source row from the active partition. Safe now: a
    //    Move always inserted above, so the profile lives in the target before
    //    we delete it here. A failed insert returns early via `?`, leaving the
    //    source untouched.
    if remove_from_source {
        let remaining: Vec<Value> = source_profiles
            .into_iter()
            .filter(|p| value_str(p, &["id", "uid", "profileUid"]) != Some(profile_id))
            .collect();
        insert_profiles_with_dek(conn, root_secret, source_user_id, source_dek, &remaining)?;
    }

    Ok(ProfileRelocation {
        source_profile_id: profile_id.to_string(),
        new_profile_id: new_profile_id.to_string(),
        profile_name,
        target_user_id,
        moved: remove_from_source,
        already_present,
        inserted,
        protocol,
        secrets_not_copied: Vec::new(),
    })
}

/// Partition credential_type for a relocate key (migrate precedent at
/// `copy_user_secrets_with_dek`: `server` / `oauth` / `jottacloud_refresh`,
/// plus the crypt/Filen/OneDrive prefixes from [`relocate_all_secret_key_candidates`]).
pub(crate) fn relocate_secret_kind(credential_key: &str) -> &'static str {
    if credential_key.starts_with("server_modes_") {
        "server_modes"
    } else if credential_key.starts_with("server_") {
        "server"
    } else if credential_key.starts_with("jottacloud_refresh_") {
        "jottacloud_refresh"
    } else if credential_key.starts_with("oauth_") {
        "oauth"
    } else if credential_key.starts_with("aerocrypt_overlay_pw_") {
        "aerocrypt_overlay_pw"
    } else if credential_key.starts_with("aerocrypt_overlay_salt_") {
        "aerocrypt_overlay_salt"
    } else if credential_key.starts_with("aerocrypt_overlay_keyfile_path_") {
        "aerocrypt_overlay_keyfile_path"
    } else if credential_key.starts_with("aerocrypt_overlay_config_") {
        "aerocrypt_overlay_config"
    } else if credential_key.starts_with("filen_api_key_") {
        "filen_api_key"
    } else if credential_key.starts_with("onedrive_drive_id_") {
        "onedrive_drive_id"
    } else if credential_key.starts_with("onedrive_drive_type_") {
        "onedrive_drive_type"
    } else if credential_key.starts_with("proton_native_session_") {
        "proton_native_session"
    } else {
        // Unknown prefix from a future per_profile_vault_keys entry: still
        // dual-write under a stable generic type rather than dropping it.
        "server"
    }
}

/// Relocate one vault/partition secret under the #366 gating.
///
/// Order is non-negotiable for every key:
/// 1. when `relocation.inserted` → copy source → new (vault + target partition)
/// 2. when `relocation.moved` → delete the source key
///
/// `store: None` skips vault ops only (unit-test seam: partition-only dual).
/// Production always passes `Some(store)`. Behavior with a store for `server_*`
/// matches the pre-F4 body byte-for-byte aside from multi-key iteration.
#[allow(clippy::too_many_arguments)]
fn relocate_secret_key_dual(
    conn: &Connection,
    store: Option<&CredentialStore>,
    root_key: &[u8; 32],
    source_user_id: i64,
    relocation: &ProfileRelocation,
    target_passphrase: Option<&str>,
    source_key: &str,
    new_key: &str,
    kind: &str,
) -> bool {
    // Native sessions require the enclosing profile transaction and key/UID
    // guards. Never let this generic best-effort helper mutate one directly.
    if source_key.starts_with("proton_native_session_")
        || new_key.starts_with("proton_native_session_")
    {
        return true;
    }
    // Copy onto the new id only when a fresh target row was inserted; a dedup
    // no-op (Copy into a target that already has the drive) leaves the target's
    // existing secret untouched. A Move always inserts (#366), so its secret
    // always follows the profile into the target before the source is dropped.
    let mut copied = true;
    if relocation.inserted {
        let secret = match store {
            Some(store) => {
                read_credential_with_fallback(conn, store, root_key, source_user_id, source_key)
            }
            None => get_user_credential_for(conn, root_key, source_user_id, source_key),
        };
        match secret {
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("Relocate: {source_key} could not be read: {e}");
                copied = false;
            }
            Ok(Some(secret)) => {
                // Vault stays in sync (source of truth + fallback + downgrade safe),
                // except for a target's private token: the vault key of that
                // name is the legacy owner's, so the target's row alone holds it.
                let target_private =
                    vault_copy_is_another_accounts(conn, relocation.target_user_id, new_key)
                        .unwrap_or(true);
                let in_vault = !target_private
                    && store.is_some_and(|store| store.store(new_key, &secret).is_ok());
                // Mirror onto the target partition under its scoped DEK.
                let root_secret = user_crypto::secret_key_from_bytes(root_key);
                let in_partition = resolve_user_dek_scoped(
                    conn,
                    &root_secret,
                    relocation.target_user_id,
                    target_passphrase,
                )
                .and_then(|target_dek| {
                    set_user_credential_with_dek(
                        conn,
                        relocation.target_user_id,
                        &target_dek,
                        new_key,
                        kind,
                        &secret,
                    )
                })
                .is_ok();
                // A target that is not the secret's legacy owner reads only its
                // partition, so a vault copy alone would not reach it.
                let target_reads_vault = nonprofile_secret_owner(conn)
                    .is_ok_and(|owner| owner == Some(relocation.target_user_id));
                copied = in_partition || (in_vault && target_reads_vault);
            }
        }
    }

    // Move/Cut: the source profile row is gone, so its orphaned secret is
    // removed from both the vault and the source partition, unless it was not
    // copied: an orphan can be cleaned up later, a lost secret cannot come back.
    if relocation.moved {
        if !copied {
            tracing::warn!("Relocate: {source_key} kept, its copy {new_key} was not written");
            return false;
        }
        // A source's private token was its row alone: the vault key of that
        // name is the legacy owner's and stays.
        let source_private =
            vault_copy_is_another_accounts(conn, source_user_id, source_key).unwrap_or(true);
        if let Some(store) = store.filter(|_| !source_private) {
            let _ = store.delete(source_key);
        }
        let _ = delete_user_credential_for(conn, source_user_id, source_key);
    }
    copied
}

/// MUV-3 cross-user credential relocation: the partition profile row is already
/// moved by [`relocate_server_profile`]; this carries every per-profile secret
/// resolved by [`relocate_all_secret_key_candidates`] (full
/// [`crate::per_profile_vault_keys`] set: `server_*`, OAuth / Jottacloud, Crypt
/// overlay secrets, Filen, OneDrive hints). The vault stays the source of truth
/// (copy onto the new id, drop the orphan on a Move), and each secret is mirrored
/// onto the TARGET user's partition under its own scoped DEK (resolved with
/// `target_passphrase`, never the source session). Best-effort on the partition
/// mirror: a locked/uncovered target falls back to the dual-written vault. Caller
/// must invoke this while `root_key` / `target_passphrase` are still live
/// (before zeroize).
///
/// #366: for every key, `inserted` gates copy before `moved` gates delete.
/// Crypt overlay secrets were previously omitted from the dual (identity-only
/// candidates), so a Copy/Move to another user kept the Crypt badges on the
/// profile JSON while the overlay password/salt/config stayed on the source id —
/// connect showed no decrypt toggles working and ciphertext names.
fn relocate_server_credential_dual(
    conn: &Connection,
    store: &CredentialStore,
    root_key: &[u8; 32],
    source_user_id: i64,
    relocation: &ProfileRelocation,
    target_passphrase: Option<&str>,
) -> Vec<String> {
    let source_keys =
        relocate_all_secret_key_candidates(&relocation.protocol, &relocation.source_profile_id);
    let new_keys =
        relocate_all_secret_key_candidates(&relocation.protocol, &relocation.new_profile_id);
    let mut not_copied = Vec::new();
    for (source_key, new_key) in source_keys.iter().zip(new_keys.iter()) {
        if source_key.starts_with("proton_native_session_") {
            continue;
        }
        let copied = relocate_secret_key_dual(
            conn,
            Some(store),
            root_key,
            source_user_id,
            relocation,
            target_passphrase,
            source_key,
            new_key,
            relocate_secret_kind(source_key),
        );
        if !copied {
            not_copied.push(new_key.clone());
        }
    }
    not_copied
}

/// Read a single user_settings scope for the given user, decrypted as JSON.
/// Returns `Ok(None)` when no row exists. Internal/encryption scope names that
/// are reserved (start with `__`) are filtered out at the CLI/Tauri boundary,
/// not here, so callers can still introspect legacy backups.
pub fn get_user_setting_for(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    scope: &str,
) -> Result<Option<Value>, String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        let row: Option<(Vec<u8>, Vec<u8>)> = conn
            .query_row(
                "SELECT encrypted_blob, nonce FROM user_settings
                 WHERE user_id = ?1 AND scope = ?2",
                params![user_id, scope],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("Read user setting [{scope}]: {e}"))?;
        match row {
            None => Ok(None),
            Some((blob, nonce)) => Ok(Some(decrypt_value(dek, &nonce, &blob)?)),
        }
    })
}

pub fn set_user_setting_for(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    scope: &str,
    value: &Value,
) -> Result<(), String> {
    if scope.is_empty() {
        return Err("USER_SETTING_SCOPE_REQUIRED".to_string());
    }
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        let (encrypted_blob, nonce) = encrypt_value(dek, value)?;
        conn.execute(
            "INSERT INTO user_settings(
                 user_id, scope, encrypted_blob, nonce, aead_alg, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, 'aes-256-gcm', ?5)
             ON CONFLICT(user_id, scope) DO UPDATE SET
                 encrypted_blob = excluded.encrypted_blob,
                 nonce          = excluded.nonce,
                 aead_alg       = excluded.aead_alg,
                 updated_at     = excluded.updated_at",
            params![user_id, scope, encrypted_blob, nonce, now_ms()],
        )
        .map_err(|e| format!("Upsert user setting [{scope}]: {e}"))?;
        Ok(())
    })
}

pub fn delete_user_setting_for(conn: &Connection, user_id: i64, scope: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM user_settings WHERE user_id = ?1 AND scope = ?2",
        params![user_id, scope],
    )
    .map_err(|e| format!("Delete user setting [{scope}]: {e}"))?;
    Ok(())
}

pub fn get_active_user_setting(
    conn: &Connection,
    root_key: &[u8; 32],
    scope: &str,
) -> Result<Option<Value>, String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    get_user_setting_for(conn, root_key, user_id, scope)
}

pub fn set_active_user_setting(
    conn: &Connection,
    root_key: &[u8; 32],
    scope: &str,
    value: &Value,
) -> Result<(), String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    set_user_setting_for(conn, root_key, user_id, scope, value)
}

pub fn delete_active_user_setting(conn: &Connection, scope: &str) -> Result<(), String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    delete_user_setting_for(conn, user_id, scope)
}

// --- MUV-1: per-user credentials (raw secrets under the user DEK) -----------
//
// Companion of `user_settings`, but for raw secrets (server passwords, OAuth
// token blobs, API keys, PEM). Secrets are updated one at a time (an OAuth
// refresh rewrites a single key), so the storage model is upsert-per-key with a
// composite primary key `(user_id, credential_id)`, NOT the delete-all + insert
// pattern used for `server_profiles`. The secret is encrypted with the user's
// DEK exactly like a profile blob; only the active user (or a primed session
// for a passphrase account) can read or write its own credentials. Callers reach
// it through the dual-write and fallback helpers below (MUV-2..5) or with a key
// they build themselves; no IPC command reads or writes it by an id the webview
// chooses, which is what keeps backend-private rows such as `mcp_*` private.

/// Upsert one secret into a user's partition, encrypted with their DEK.
///
/// `secret` is treated as opaque bytes (it may be raw, e.g. a password, or
/// JSON, e.g. an OAuth token blob). The row is keyed by
/// `(user_id, credential_id)`; a second call with the same id overwrites the
/// previous value. Requires the user's DEK: a passphrase account must already
/// be unlocked in the session, otherwise this returns `USER_LOCKED`.
pub fn set_user_credential_for(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    #[cfg(test)]
    if PARTITION_WRITE_REFUSED.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err("the account store refused the row (test)".to_string());
    }
    if credential_id.is_empty() {
        return Err("CREDENTIAL_ID_REQUIRED".to_string());
    }
    if credential_type.is_empty() {
        return Err("CREDENTIAL_TYPE_REQUIRED".to_string());
    }
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        set_user_credential_with_dek(conn, user_id, dek, credential_id, credential_type, secret)
    })
}

/// Atomically write related owned credentials. Resolve and hold the user's DEK
/// before taking SQLite's write reservation; never reacquire USER_SESSION from
/// inside an open write transaction (normal writers use session then SQLite).
pub(crate) fn set_user_credentials_for(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credentials: &[(&str, &str, &str)],
) -> Result<(), String> {
    for (id, kind, _) in credentials {
        if id.is_empty() {
            return Err("CREDENTIAL_ID_REQUIRED".to_string());
        }
        if kind.is_empty() {
            return Err("CREDENTIAL_TYPE_REQUIRED".to_string());
        }
    }
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Begin owned credential batch: {e}"))?;
        for (id, kind, secret) in credentials {
            set_user_credential_with_dek(&tx, user_id, dek, id, kind, secret)?;
        }
        tx.commit()
            .map_err(|e| format!("Commit owned credential batch: {e}"))
    })
}

/// Upsert one credential row with an already-resolved DEK. Lets callers that
/// resolved the DEK out-of-session (e.g. a cross-user relocation that unwrapped
/// the target via its passphrase, MUV-3) write a credential without going back
/// through the session-based [`with_user_dek`].
fn set_user_credential_with_dek(
    conn: &Connection,
    user_id: i64,
    dek: &SecretKey,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    let (encrypted_blob, nonce) = user_crypto::encrypt_blob(dek, secret.as_bytes())?;
    conn.execute(
        "INSERT INTO user_credentials(
             user_id, credential_id, credential_type, encrypted_blob, nonce,
             aead_alg, updated_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, 'aes-256-gcm', ?6)
         ON CONFLICT(user_id, credential_id) DO UPDATE SET
             credential_type = excluded.credential_type,
             encrypted_blob  = excluded.encrypted_blob,
             nonce           = excluded.nonce,
             aead_alg        = excluded.aead_alg,
             updated_at      = excluded.updated_at",
        params![
            user_id,
            credential_id,
            credential_type,
            encrypted_blob,
            nonce.to_vec(),
            now_ms()
        ],
    )
    .map_err(|e| format!("Upsert user credential: {e}"))?;
    Ok(())
}

/// Whether the row `credential_id` of `user_id` holds the very ciphertext a
/// sign-in of that account wrote, or a refresh of it since
/// ([`stamp_sign_in`]). A row any other writer rewrote (a restore, an
/// import, a migration copy, a relocation, the alignment) no longer matches
/// its stamp.
pub(crate) fn signed_in_row(
    conn: &Connection,
    user_id: i64,
    credential_id: &str,
) -> Result<bool, String> {
    let row: Option<(Vec<u8>, Vec<u8>, String)> = conn
        .query_row(
            "SELECT c.encrypted_blob, c.nonce, s.blob_digest
             FROM user_credentials c
             JOIN private_token_sign_ins s
                 ON s.user_id = c.user_id AND s.credential_id = c.credential_id
             WHERE c.user_id = ?1 AND c.credential_id = ?2",
            params![user_id, credential_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| format!("Read sign-in stamp: {e}"))?;
    Ok(row.is_some_and(|(blob, nonce, digest)| credential_blob_digest(&blob, &nonce) == digest))
}

/// Removes the sign-in stamp of `credential_id` of `user_id`, if any, in the
/// caller's transaction.
fn forget_sign_in(conn: &Connection, user_id: i64, credential_id: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM private_token_sign_ins WHERE user_id = ?1 AND credential_id = ?2",
        params![user_id, credential_id],
    )
    .map(|_| ())
    .map_err(|e| format!("Forget sign-in stamp: {e}"))
}

/// Stamps the row `credential_id` of `user_id`, as it is now, as written by
/// that account's own sign-in: in the caller's transaction, the one that
/// wrote the row.
fn stamp_sign_in(conn: &Connection, user_id: i64, credential_id: &str) -> Result<(), String> {
    let (blob, nonce): (Vec<u8>, Vec<u8>) = conn
        .query_row(
            "SELECT encrypted_blob, nonce FROM user_credentials
             WHERE user_id = ?1 AND credential_id = ?2",
            params![user_id, credential_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| format!("Read signed-in credential: {e}"))?;
    conn.execute(
        "INSERT INTO private_token_sign_ins(user_id, credential_id, blob_digest)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(user_id, credential_id) DO UPDATE SET blob_digest = excluded.blob_digest",
        params![
            user_id,
            credential_id,
            credential_blob_digest(&blob, &nonce)
        ],
    )
    .map_err(|e| format!("Write sign-in stamp: {e}"))?;
    Ok(())
}

/// [`set_user_credential_with_dek`] for a sign-in of `user_id`'s own: a
/// private token ([`private_token_type`]) is stamped in the same write, so a
/// stamped row always has its account's sign-in behind it. `conn` is the
/// caller's transaction.
fn set_signed_in_credential_with_dek(
    conn: &Connection,
    user_id: i64,
    dek: &SecretKey,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    set_user_credential_with_dek(conn, user_id, dek, credential_id, credential_type, secret)?;
    if private_token_type(credential_id).is_some() {
        stamp_sign_in(conn, user_id, credential_id)?;
    }
    Ok(())
}

/// [`set_user_credential_with_dek`] for a refresh of the row: a row a sign-in
/// stamped stays stamped, any other stays unstamped. `conn` is the caller's
/// transaction.
fn set_refreshed_credential_with_dek(
    conn: &Connection,
    user_id: i64,
    dek: &SecretKey,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    let signed_in = signed_in_row(conn, user_id, credential_id)?;
    set_user_credential_with_dek(conn, user_id, dek, credential_id, credential_type, secret)?;
    if signed_in {
        stamp_sign_in(conn, user_id, credential_id)?;
    }
    Ok(())
}

/// [`set_user_credential_for`] for a sign-in of `user_id`'s own
/// ([`set_signed_in_credential_with_dek`]), the row and its stamp in one
/// transaction.
pub(crate) fn set_signed_in_credential_for(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    if credential_id.is_empty() {
        return Err("CREDENTIAL_ID_REQUIRED".to_string());
    }
    if credential_type.is_empty() {
        return Err("CREDENTIAL_TYPE_REQUIRED".to_string());
    }
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Begin signed-in credential write: {e}"))?;
        set_signed_in_credential_with_dek(
            &tx,
            user_id,
            dek,
            credential_id,
            credential_type,
            secret,
        )?;
        tx.commit()
            .map_err(|e| format!("Commit signed-in credential write: {e}"))
    })
}

/// [`set_signed_in_credential_for`] only while the row holds `expected`
/// (`None`: no row). Read, comparison and write share one IMMEDIATE
/// transaction, as in [`swap_user_credential_if`], so a value another writer
/// stored since (a refresh of the token) is never replaced. Whether it was
/// written.
pub(crate) fn set_signed_in_credential_if(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    expected: Option<&str>,
    secret: &str,
) -> Result<bool, String> {
    if credential_id.is_empty() {
        return Err("CREDENTIAL_ID_REQUIRED".to_string());
    }
    if credential_type.is_empty() {
        return Err("CREDENTIAL_TYPE_REQUIRED".to_string());
    }
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Begin conditional signed-in credential write: {e}"))?;
        let current = read_credential_with_dek(&tx, user_id, dek, credential_id)?;
        let holds = match (current.as_ref().map(|v| v.as_str()), expected) {
            (None, None) => true,
            (Some(current), Some(expected)) => {
                use subtle::ConstantTimeEq;
                bool::from(current.as_bytes().ct_eq(expected.as_bytes()))
            }
            _ => false,
        };
        if !holds {
            return Ok(false);
        }
        set_signed_in_credential_with_dek(
            &tx,
            user_id,
            dek,
            credential_id,
            credential_type,
            secret,
        )?;
        tx.commit()
            .map_err(|e| format!("Commit conditional signed-in credential write: {e}"))?;
        Ok(true)
    })
}

/// Forgets every sign-in stamp of the partition database at `db_path`: a
/// restored database carries its rows, never the sign-ins behind them.
pub fn forget_sign_in_stamps_in_db(db_path: &std::path::Path) -> Result<(), String> {
    // The schema step gives a backup of a build before the stamps their table.
    open_restored_db(db_path)?
        .execute("DELETE FROM private_token_sign_ins", [])
        .map_err(|e| format!("Forget sign-in stamps: {e}"))?;
    Ok(())
}

/// Read one secret from a user's partition, decrypted with their DEK.
///
/// Returns `Ok(None)` when no row exists. The decrypted secret is wrapped in
/// `Zeroizing<String>` so it is scrubbed from memory on drop, matching
/// `CredentialStore::get_secret`. Requires the user's DEK (see
/// [`set_user_credential_for`]).
pub fn get_user_credential_for(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        read_credential_with_dek(conn, user_id, dek, credential_id)
    })
}

/// [`get_user_credential_for`] with the digest of the ciphertext decrypted
/// ([`read_credential_and_digest_with_dek`]).
fn get_user_credential_and_digest_for(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
) -> Result<Option<RowRead>, String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        read_credential_and_digest_with_dek(conn, user_id, dek, credential_id)
    })
}

/// Whether `digest`, the digest of a row of `credential_id` of `user_id` as
/// a reader decrypted it, is the one its account's sign-in stamped
/// ([`stamp_sign_in`]): the stamp is compared with the bytes read, never with
/// the row as a second query finds it after another writer rewrote it.
fn stamp_matches(
    conn: &Connection,
    user_id: i64,
    credential_id: &str,
    digest: &str,
) -> Result<bool, String> {
    let stamp: Option<String> = conn
        .query_row(
            "SELECT blob_digest FROM private_token_sign_ins
             WHERE user_id = ?1 AND credential_id = ?2",
            params![user_id, credential_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Read sign-in stamp: {e}"))?;
    Ok(stamp.as_deref() == Some(digest))
}

/// A write that needs the vault's copy of a key found the vault closed: it
/// was locked since the caller read what it is writing over. The caller
/// reports it as it is, never as a change of the credential's owner.
pub(crate) const VAULT_NOT_OPEN: &str = "VAULT_NOT_OPEN: the credential vault is not open";

#[cfg(test)]
thread_local! {
    /// Runs once on this thread in the next [`replace_user_credential_where`],
    /// inside its transaction, after the row is read and before it is checked:
    /// the vault locked, or another writer, in between (test only).
    pub(crate) static BEFORE_REFRESH_ROW_CHECK: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

/// What a conditional refresh of a row did ([`replace_user_credential_where`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshOutcome {
    /// The row held a value the refresh accepts, and now holds the replacement.
    Written,
    /// The row is gone or holds another value: another writer won, and the
    /// refresh's result is stale.
    Changed,
    /// The row still holds a value the refresh accepts, but its readers
    /// withhold it now ([`checked_row`]: a keystore restore set the wait since
    /// the refresh read it). Nothing is written, and the result is not stale:
    /// the caller keeps it for a write once the row is readable again.
    Withheld,
}

/// Conditional refresh of an existing owned credential. Resolve the DEK before
/// SQLite's write reservation, preserving the session -> SQLite lock order.
/// A missing or replaced row is never recreated with a stale refresh result,
/// and a row its readers withhold is not written over
/// ([`replace_user_credential_where`]), checked against the open vault: a
/// vault closed meanwhile is [`VAULT_NOT_OPEN`]. Whether it was written.
pub(crate) fn replace_user_credential_if_unchanged(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    update: (&str, &str),
    require_active_user: bool,
) -> Result<bool, String> {
    replace_user_credential_if_unchanged_outcome(
        conn,
        root_key,
        user_id,
        credential_id,
        credential_type,
        update,
        require_active_user,
    )
    .map(|outcome| outcome == RefreshOutcome::Written)
}

/// [`replace_user_credential_if_unchanged`], saying why nothing was written
/// ([`RefreshOutcome`]): a refresh keeps a result whose row is withheld.
pub(crate) fn replace_user_credential_if_unchanged_outcome(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    update: (&str, &str),
    require_active_user: bool,
) -> Result<RefreshOutcome, String> {
    use subtle::ConstantTimeEq;
    replace_user_credential_where(
        conn,
        root_key,
        user_id,
        credential_id,
        credential_type,
        &|current| bool::from(current.as_bytes().ct_eq(update.0.as_bytes())),
        update.1,
        require_active_user,
        &|key: &str| match CredentialStore::from_cache() {
            Some(store) => get_optional_secret(&store, key),
            None => Err(VAULT_NOT_OPEN.to_string()),
        },
    )
}

/// Put `value` in the row `credential_id` of `user_id` (`None`: remove it),
/// only while the row holds `expected` (`None`: no row). Read, comparison and
/// write share one IMMEDIATE transaction, so a value another writer stored in
/// between (a refresh of the token) is never replaced. Whether it was written.
pub(crate) fn swap_user_credential_if(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    expected: Option<&str>,
    value: Option<&str>,
) -> Result<bool, String> {
    swap_user_credential_if_one_of(
        conn,
        root_key,
        user_id,
        credential_id,
        credential_type,
        &[expected],
        value,
    )
}

/// [`swap_user_credential_if`] while the row holds any one of `expected`
/// (each `None`: no row), in the same one transaction.
pub(crate) fn swap_user_credential_if_one_of(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    expected: &[Option<&str>],
    value: Option<&str>,
) -> Result<bool, String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Begin conditional credential write: {e}"))?;
        let current = read_credential_with_dek(&tx, user_id, dek, credential_id)?;
        let holds = expected.iter().any(|expected| {
            match (current.as_ref().map(|v| v.as_str()), expected) {
                (None, None) => true,
                (Some(current), Some(expected)) => {
                    use subtle::ConstantTimeEq;
                    bool::from(current.as_bytes().ct_eq(expected.as_bytes()))
                }
                _ => false,
            }
        });
        if !holds {
            return Ok(false);
        }
        match value {
            Some(value) => set_user_credential_with_dek(
                &tx,
                user_id,
                dek,
                credential_id,
                credential_type,
                value,
            )?,
            None => delete_user_credential_for(&tx, user_id, credential_id)?,
        }
        tx.commit()
            .map_err(|e| format!("Commit conditional credential write: {e}"))?;
        Ok(true)
    })
}

/// [`replace_user_credential_if_unchanged`] for a row that must still hold a
/// value `still_holds` accepts, not one exact value: the row is replaced by
/// `replacement` only then, read and written in one IMMEDIATE transaction,
/// and a missing row is never created. The row is read as its readers read it
/// ([`checked_row`], the vault through `read_secret`): a refresh that read it
/// before a restore set the wait does not write over a row the readers
/// withhold now, with its bytes unchanged ([`RefreshOutcome::Withheld`]).
#[allow(clippy::too_many_arguments)]
fn replace_user_credential_where(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    still_holds: &dyn Fn(&str) -> bool,
    replacement: &str,
    require_active_user: bool,
    read_secret: &SecretReader<'_>,
) -> Result<RefreshOutcome, String> {
    if credential_id.is_empty() || credential_type.is_empty() {
        return Err("CREDENTIAL_ID_AND_TYPE_REQUIRED".to_owned());
    }
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Begin conditional credential refresh: {e}"))?;
        if require_active_user && active_user_id(&tx)? != Some(user_id) {
            return Err(ACCOUNT_CHANGED.to_owned());
        }
        let read = read_credential_and_digest_with_dek(&tx, user_id, dek, credential_id)?;
        #[cfg(test)]
        run_dual_write_hook(&BEFORE_REFRESH_ROW_CHECK);
        // Whether the row as stored still holds what the refresh accepts: one
        // that does, which its readers withhold now, is withheld, not changed.
        let read_held = read.as_ref().is_some_and(|(secret, _)| still_holds(secret));
        let current = checked_row(&tx, user_id, credential_id, read, read_secret)?;
        if !current.as_ref().is_some_and(|secret| still_holds(secret)) {
            let withheld = current.is_none() && read_held;
            return Ok(if withheld {
                RefreshOutcome::Withheld
            } else {
                RefreshOutcome::Changed
            });
        }
        set_refreshed_credential_with_dek(
            &tx,
            user_id,
            dek,
            credential_id,
            credential_type,
            replacement,
        )?;
        tx.commit()
            .map_err(|e| format!("Commit conditional credential refresh: {e}"))?;
        Ok(RefreshOutcome::Written)
    })
}

/// Delete `user_id`'s row of `credential_id` only while it holds a value
/// `still_holds` accepts: read and deleted in one IMMEDIATE transaction, so a
/// value written since (an import while a refused chain was with Jotta) is
/// never removed. Whether it was deleted; a missing row is `Ok(false)`.
pub(crate) fn delete_user_credential_where(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    still_holds: &dyn Fn(&str) -> bool,
) -> Result<bool, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    if !partition_row_exists(&conn, user_id, credential_id)? {
        return Ok(false);
    }
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    with_user_dek(&conn, &root_secret, user_id, |user_id, dek| {
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Begin conditional credential delete: {e}"))?;
        let current = read_credential_with_dek(&tx, user_id, dek, credential_id)?;
        if !current.as_ref().is_some_and(|secret| still_holds(secret)) {
            return Ok(false);
        }
        delete_user_credential_for(&tx, user_id, credential_id)?;
        tx.commit()
            .map_err(|e| format!("Commit conditional credential delete: {e}"))?;
        Ok(true)
    })
}

/// A refreshed secret written to `user_id`'s row only while the row holds a
/// value `still_holds` accepts ([`replace_user_credential_where`]); a missing
/// row is never created, nor its account's key asked for. The migrations run
/// first, as for [`store_credential_for_user`], so a copy their alignment
/// drops is gone before the look. A row that is gone is
/// [`RefreshOutcome::Changed`].
#[cfg(feature = "proton-native")]
pub(crate) fn refresh_user_credential(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    still_holds: &dyn Fn(&str) -> bool,
    replacement: &str,
) -> Result<RefreshOutcome, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    if !partition_row_exists(&conn, user_id, credential_id)? {
        return Ok(RefreshOutcome::Changed);
    }
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    replace_user_credential_where(
        &conn,
        &root_key,
        user_id,
        credential_id,
        credential_type,
        still_holds,
        replacement,
        false,
        &|key: &str| get_optional_secret(store, key),
    )
}

/// Replace the copy of `credential_id` that [`resolve_credential_for_user_owner_aware`]
/// (`user`, or [`resolve_active_credential_owner_aware`] for `None`) reads,
/// only while it still holds `expected` (`None`: no copy at all, which is then
/// created). Read, comparison and write share one lock: the partition's
/// IMMEDIATE transaction, or the vault's cross-process write lock, taken
/// inside that transaction when the legacy owner reads the vault, so no row
/// appears in between. `Ok(false)`: the copy changed since `expected` was
/// read, or, for a refresh (`signed_in` false), its readers now withhold it
/// ([`checked_row`]), and nothing is written. The legacy owner's other copy
/// (the vault key after a row write, the row after a vault-only write) then
/// follows on a best-effort basis, by its own compare-and-write on what it
/// holds, while `same_chain` says it holds the chain `expected` held (the
/// same chain in other bytes included); a copy that holds another chain is
/// left as it is.
/// A follow that fails is [`COPY_NOT_FOLLOWED`]: the copy read was written,
/// the other was not.
///
/// `unmoved` names, for `owner` ([`chain_deletions`]), each key whose
/// deletions must still be the given generation with none running: checked
/// in the same transaction, the one a deletion is recorded in, so a deletion
/// that began after the caller last looked stops the write with
/// [`CHAIN_DELETED`]. A vault-only write is made inside that transaction too
/// (lock order: the database, then the vault; no vault lock is ever held
/// while the database is waited for).
#[allow(clippy::too_many_arguments)]
pub(crate) fn replace_owner_aware_if_unchanged(
    store: &CredentialStore,
    user: Option<i64>,
    credential_id: &str,
    credential_type: &str,
    expected: Option<&str>,
    replacement: &str,
    same_chain: &dyn Fn(&str, &str) -> bool,
    owner: Option<i64>,
    unmoved: &[(&str, u64)],
    signed_in: bool,
) -> Result<bool, String> {
    let unmoved_in = |conn: &Connection| -> Result<(), String> {
        let now = now_ms();
        for (account, generation) in unmoved {
            let mut record =
                DeletionRecord::read(conn, &chain_deletion_key(store, owner, account)?)?;
            record.settle(now);
            if record.generation != *generation || !record.running.is_empty() {
                return Err(CHAIN_DELETED.to_string());
            }
        }
        Ok(())
    };
    let vault = || -> Result<bool, String> {
        match expected {
            Some(expected) => {
                store.replace_secret_if_unchanged(credential_id, expected, replacement)
            }
            None => store.store_secret_if_absent(credential_id, replacement),
        }
        .map_err(|e| format!("Conditional vault write: {e}"))
    };
    // A write with nothing to check needs no transaction of its own.
    let vault_checked = || -> Result<bool, String> {
        if unmoved.is_empty() {
            return vault();
        }
        init_or_migrate_cli(store)?;
        let conn = open_or_init_cli()?;
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Begin conditional credential write: {e}"))?;
        unmoved_in(&tx)?;
        let written = vault()?;
        tx.commit()
            .map_err(|e| format!("Commit conditional credential write: {e}"))?;
        Ok(written)
    };
    if user == Some(LEGACY_ONLY_ACCOUNT) {
        let written = vault_checked()?;
        // The legacy owner's row of that key, when it still holds the chain
        // the vault held, is the same chain: it follows, so a reader of the
        // row is not left with a token Jotta consumed. A row that holds
        // another chain is left as it is.
        if let Some(expected) = expected.filter(|_| written) {
            let follow = || -> Result<(), String> {
                #[cfg(test)]
                if FOLLOW_REFUSED.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err("refused (test)".to_string());
                }
                init_or_migrate_cli(store)?;
                let conn = open_or_init_cli()?;
                let Some(owner) = nonprofile_secret_owner(&conn)? else {
                    return Ok(());
                };
                let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
                follow_copy(
                    || get_user_credential_for(&conn, &root_key, owner, credential_id),
                    &|copy| same_chain(expected, copy),
                    |row| {
                        #[cfg(test)]
                        rewrite_before_follow(store, Some(owner), credential_id, row);
                        replace_user_credential_if_unchanged(
                            &conn,
                            &root_key,
                            owner,
                            credential_id,
                            credential_type,
                            (row, replacement),
                            false,
                        )
                    },
                )
            };
            if let Err(e) = follow() {
                log::warn!("Conditional credential write: the owner's row did not follow: {e}");
                return Err(COPY_NOT_FOLLOWED.to_string());
            }
        }
        return Ok(written);
    }
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let Some(user_id) = (match user {
        Some(user) => Some(user),
        // No active account: the vault, as the active reader reads it then
        // (`resolve_active_credential_owner_aware`).
        None => active_user_id(&conn)?,
    }) else {
        return vault_checked();
    };
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    with_user_dek(&conn, &root_secret, user_id, |user_id, dek| {
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Begin conditional credential write: {e}"))?;
        if user.is_none() && active_user_id(&tx)? != Some(user_id) {
            return Err(ACCOUNT_CHANGED.to_owned());
        }
        unmoved_in(&tx)?;
        let is_owner = nonprofile_secret_owner(&tx)? == Some(user_id);
        // A sign-in compares against the row as its reader saw it: a row the
        // readers withhold ([`checked_row`]) read as no row, so the sign-in
        // that replaces it is not refused for bytes it was never shown. A
        // refresh never writes over such a row, checked here, in the write's
        // own transaction: the row may have turned withheld since the
        // refresh read it (a restore meanwhile set the wait), with its bytes
        // unchanged.
        let read_secret = |key: &str| get_optional_secret(store, key);
        let current = match read_credential_and_digest_with_dek(&tx, user_id, dek, credential_id)? {
            Some((row, digest))
                if !is_owner
                    && unverified_while_tags_pending(
                        &tx,
                        user_id,
                        credential_id,
                        &row,
                        &digest,
                        &read_secret,
                    )? =>
            {
                if !signed_in {
                    return Ok(false);
                }
                None
            }
            other => other.map(|(row, _)| row),
        };
        let reads_vault = current.is_none() && is_owner;
        if reads_vault {
            let written = vault()?;
            tx.commit()
                .map_err(|e| format!("Commit conditional credential write: {e}"))?;
            return Ok(written);
        }
        use subtle::ConstantTimeEq;
        let unchanged = match (&current, expected) {
            (Some(current), Some(expected)) => {
                bool::from(current.as_bytes().ct_eq(expected.as_bytes()))
            }
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            return Ok(false);
        }
        if signed_in {
            set_signed_in_credential_with_dek(
                &tx,
                user_id,
                dek,
                credential_id,
                credential_type,
                replacement,
            )?;
        } else {
            set_refreshed_credential_with_dek(
                &tx,
                user_id,
                dek,
                credential_id,
                credential_type,
                replacement,
            )?;
        }
        let owner = is_owner;
        tx.commit()
            .map_err(|e| format!("Commit conditional credential write: {e}"))?;
        // The legacy owner's vault copy, read by a vault-only reader
        // (`LEGACY_ONLY_ACCOUNT`), follows while it holds the same chain;
        // after the commit, so no vault lock is taken inside the
        // transaction. A copy that holds another chain stays.
        if let Some(expected) = expected.filter(|_| owner) {
            let follow = || -> Result<(), String> {
                #[cfg(test)]
                if FOLLOW_REFUSED.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err("refused (test)".to_string());
                }
                follow_copy(
                    || get_optional_secret(store, credential_id),
                    &|copy| same_chain(expected, copy),
                    |copy| {
                        #[cfg(test)]
                        rewrite_before_follow(store, None, credential_id, copy);
                        store
                            .replace_secret_if_unchanged(credential_id, copy, replacement)
                            .map_err(|e| e.to_string())
                    },
                )
            };
            if let Err(e) = follow() {
                log::warn!("Conditional credential write: the vault copy did not follow: {e}");
                return Err(COPY_NOT_FOLLOWED.to_string());
            }
        }
        Ok(true)
    })
}

/// Make the legacy owner's other copy of a chain follow a write: `read` it,
/// and while it holds the same chain (`same_chain`), `replace` it on what it
/// holds. A copy rewritten between the read and the replacement (`Ok(false)`)
/// is read again: still the same chain in other bytes, it is replaced on what
/// it holds now; another chain is left as it is. A copy that keeps being
/// rewritten did not follow, and that is an error, so the rotation stays
/// pending for its reader.
fn follow_copy<C: std::ops::Deref<Target = String>>(
    read: impl Fn() -> Result<Option<C>, String>,
    same_chain: &dyn Fn(&str) -> bool,
    replace: impl Fn(&str) -> Result<bool, String>,
) -> Result<(), String> {
    for _ in 0..3 {
        let Some(copy) = read()?.filter(|copy| same_chain(copy)) else {
            return Ok(());
        };
        if replace(&copy)? {
            return Ok(());
        }
    }
    Err("the copy kept being rewritten".to_string())
}

/// The other copy of `credential_id` (the vault key for `None`, else that
/// account's row) rewritten once, with the same chain in other bytes, right
/// before it follows a write (test only).
#[cfg(test)]
pub(crate) static REWRITTEN_BEFORE_FOLLOW: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
fn rewrite_before_follow(
    store: &CredentialStore,
    row_of: Option<i64>,
    credential_id: &str,
    copy: &str,
) {
    if !REWRITTEN_BEFORE_FOLLOW.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let mut chain: Value = serde_json::from_str(copy).expect("a stored chain");
    chain["rewritten"] = Value::Bool(true);
    let rewritten = chain.to_string();
    match row_of {
        None => store
            .store(credential_id, &rewritten)
            .expect("rewrite the vault copy"),
        // The row alone: an imported write would rewrite the vault copy too.
        Some(user) => {
            let conn = open_or_init_cli().expect("partition DB");
            let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
            set_user_credential_for(
                &conn,
                &root_key,
                user,
                credential_id,
                "jottacloud_refresh",
                &rewritten,
            )
            .expect("rewrite the row")
        }
    }
}

/// The `global_state` key that counts the deletions of a refresh chain's
/// key for one owner: tagged, so the shared database names neither.
fn chain_deletion_key(
    store: &CredentialStore,
    owner: Option<i64>,
    account: &str,
) -> Result<String, String> {
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let owner = owner.map_or_else(|| "none".to_string(), |o| o.to_string());
    let tag = user_crypto::metadata_tag(
        &root_secret,
        b"chain-deleted",
        &format!("{owner}|{account}"),
    )?;
    Ok(format!("chain_deleted:{tag}"))
}

/// The deletions of a refresh chain's key, by any process: how many
/// completed, and how many are running now. A rotation filed before the last
/// completed one is not handed out again, and none is handed out while one
/// runs (`load_refresh_chain` in providers/jottacloud.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChainDeletions {
    /// Moved by every deletion that completed, and by every one its process
    /// left unfinished: either may have removed the key.
    pub(crate) generation: u64,
    /// The deletions running now, each in a live process.
    pub(crate) in_progress: usize,
}

impl ChainDeletions {
    /// No deletion in progress.
    pub(crate) fn settled(&self) -> bool {
        self.in_progress == 0
    }
}

/// Why a conditional write reports its key as not fully written: the copy it
/// read was replaced, but the legacy owner's other copy, which held the same
/// chain, did not follow, and its reader still reads the old one.
pub(crate) const COPY_NOT_FOLLOWED: &str =
    "COPY_NOT_FOLLOWED: the other copy of the chain did not follow";

/// Why a conditional write of a refresh chain stopped: a deletion of a key
/// it depends on began or completed since its connection began.
pub(crate) const CHAIN_DELETED: &str =
    "CHAIN_DELETED: the chain was deleted since its connection began";

/// How long a deletion recorded by a process this one cannot look at (on
/// another machine sharing the data folder, in another pid namespace, or on
/// a system whose processes cannot be read) counts as running. A deletion
/// takes milliseconds; this only bounds how long one left behind blocks.
pub(crate) const CHAIN_DELETION_LEASE_MS: i64 = 10 * 60 * 1000;

/// What the `global_state` row of one key's deletions holds.
#[derive(Default, Serialize, Deserialize)]
struct DeletionRecord {
    generation: u64,
    #[serde(default)]
    running: Vec<RunningDeletion>,
}

/// One deletion in progress, and the process that runs it.
#[derive(Serialize, Deserialize)]
struct RunningDeletion {
    id: String,
    /// The process, by a token drawn when it started.
    process: String,
    /// Where `pid` and `start` can be looked up: the boot and the pid
    /// namespace on Linux, hashed. `None` where processes are not read.
    space: Option<String>,
    pid: u32,
    /// When the process started, in clock ticks since boot.
    start: Option<u64>,
    /// When the deletion began (`now_ms`), for the lease.
    at: i64,
}

/// A deletion this process began: running until it is ended, or dropped.
/// One dropped without its end recorded is over as far as this process
/// knows, and the next read of its key's deletions here counts it as
/// completed.
#[derive(Debug)]
pub(crate) struct ChainDeletion {
    id: String,
}

impl Drop for ChainDeletion {
    fn drop(&mut self) {
        deletions_running_here().remove(&self.id);
    }
}

fn deletions_running_here() -> MutexGuard<'static, HashSet<String>> {
    static RUNNING: std::sync::LazyLock<Mutex<HashSet<String>>> =
        std::sync::LazyLock::new(Default::default);
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

/// This process as a deletion records it.
struct DeletingProcess {
    token: String,
    space: Option<String>,
    pid: u32,
    start: Option<u64>,
}

fn this_process() -> &'static DeletingProcess {
    static THIS: std::sync::LazyLock<DeletingProcess> = std::sync::LazyLock::new(|| {
        let pid = std::process::id();
        DeletingProcess {
            token: hex::encode(crate::crypto::random_bytes(16)),
            space: pid_space(),
            pid,
            start: process_start(pid).ok().flatten(),
        }
    });
    &THIS
}

/// Whether this system lets a process look another one up: then a deletion
/// whose process exited or was killed settles at once, else only once its
/// lease ran out ([`deletion_left_over`]). Tests choose their cases by it.
#[cfg(test)]
pub(crate) fn reads_process_identity() -> bool {
    let this = this_process();
    this.space.is_some() && this.start.is_some()
}

/// The boot and pid namespace this process's pids belong to, hashed: a pid
/// recorded in another one names some other process here.
#[cfg(target_os = "linux")]
pub(crate) fn pid_space() -> Option<String> {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let namespace = std::fs::read_link("/proc/self/ns/pid").ok()?;
    let mut hash = Sha256::new();
    hash.update(boot.trim().as_bytes());
    hash.update(b"|");
    hash.update(namespace.to_string_lossy().as_bytes());
    Some(hex::encode(&hash.finalize()[..16]))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn pid_space() -> Option<String> {
    None
}

/// When process `pid` started, in clock ticks since boot: `Ok(None)` when
/// there is no such process, or only its zombie; `Err` when that cannot be
/// read. Measured on Linux 7.0 (2026-11-20): about 10 microseconds for a
/// live process, 2 for a gone one, and the same value at every read.
#[cfg(target_os = "linux")]
pub(crate) fn process_start(pid: u32) -> Result<Option<u64>, ()> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    // `comm` may hold spaces and parentheses: the fields follow the last ')'.
    let fields: Vec<&str> = stat
        .get(stat.rfind(')').ok_or(())? + 1..)
        .ok_or(())?
        .split_whitespace()
        .collect();
    // Field 3 (the state) is at index 0, field 22 (the start time) at 19.
    if matches!(fields.first(), Some(&"Z") | Some(&"X")) {
        return Ok(None);
    }
    fields
        .get(19)
        .and_then(|start| start.parse().ok())
        .map(Some)
        .ok_or(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn process_start(_pid: u32) -> Result<Option<u64>, ()> {
    Err(())
}

/// Whether `deletion` will never end: this process's own when it no longer
/// runs here; one of a process on this machine when that process is gone
/// (its pid is free, a zombie, or another process's); any other once its
/// lease ran out.
fn deletion_left_over(deletion: &RunningDeletion, now: i64) -> bool {
    let this = this_process();
    if deletion.process == this.token {
        return !deletions_running_here().contains(&deletion.id);
    }
    if let (Some(space), Some(start)) = (&deletion.space, deletion.start) {
        if this.space.as_ref() == Some(space) {
            match process_start(deletion.pid) {
                Ok(None) => return true,
                Ok(Some(now_start)) => return now_start != start,
                Err(()) => {}
            }
        }
    }
    now.saturating_sub(deletion.at) > CHAIN_DELETION_LEASE_MS
}

impl DeletionRecord {
    fn read(conn: &Connection, key: &str) -> Result<Self, String> {
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM global_state WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Read chain deletions: {e}"))?;
        match value {
            None => Ok(Self::default()),
            Some(value) => serde_json::from_str(&value)
                .map_err(|_| "Read chain deletions: unreadable value".to_string()),
        }
    }

    fn view(&self) -> ChainDeletions {
        ChainDeletions {
            generation: self.generation,
            in_progress: self.running.len(),
        }
    }

    /// Count every deletion left over as completed: it may have removed the
    /// key, so a rotation filed before it is not handed out again, and it
    /// never ends, so the key is not held for ever. Whether any was.
    fn settle(&mut self, now: i64) -> bool {
        let before = self.running.len();
        self.running
            .retain(|deletion| !deletion_left_over(deletion, now));
        let settled = (before - self.running.len()) as u64;
        self.generation += settled;
        settled > 0
    }
}

/// Settle the deletions of `key` and apply `update`, in one IMMEDIATE
/// transaction.
fn update_chain_deletions(
    conn: &Connection,
    key: &str,
    update: impl FnOnce(&mut DeletionRecord),
) -> Result<ChainDeletions, String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Begin chain deletion: {e}"))?;
    let mut record = DeletionRecord::read(&tx, key)?;
    record.settle(now_ms());
    update(&mut record);
    let value = serde_json::to_string(&record).map_err(|e| format!("Chain deletions: {e}"))?;
    upsert_global_state(&tx, key, &value, now_ms())?;
    tx.commit()
        .map_err(|e| format!("Commit chain deletion: {e}"))?;
    Ok(record.view())
}

/// The next partition row written is refused, as an account store that
/// fails does, on any thread (test only).
#[cfg(test)]
pub(crate) static PARTITION_WRITE_REFUSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Every read of a key's chain deletions fails (test only).
#[cfg(test)]
pub(crate) static CHAIN_DELETIONS_REFUSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Every follow of a chain's other copy fails (test only).
#[cfg(test)]
pub(crate) static FOLLOW_REFUSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Every begin of a chain deletion fails (test only).
#[cfg(test)]
pub(crate) static CHAIN_DELETION_BEGIN_REFUSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Every end of a chain deletion fails (test only).
#[cfg(test)]
pub(crate) static CHAIN_DELETION_END_REFUSED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Write the raw deletion record of `account` for `owner` (test only).
#[cfg(test)]
pub(crate) fn set_chain_deletions_raw(
    store: &CredentialStore,
    owner: Option<i64>,
    account: &str,
    value: &str,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    upsert_global_state(
        &tx,
        &chain_deletion_key(store, owner, account)?,
        value,
        now_ms(),
    )?;
    tx.commit().map_err(|e| e.to_string())
}

/// The deletions of `account` for `owner` ([`ChainDeletions`]); those left
/// over are settled first.
pub(crate) fn chain_deletions(
    store: &CredentialStore,
    owner: Option<i64>,
    account: &str,
) -> Result<ChainDeletions, String> {
    #[cfg(test)]
    if CHAIN_DELETIONS_REFUSED.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("Read chain deletions: refused (test)".to_string());
    }
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let key = chain_deletion_key(store, owner, account)?;
    let record = DeletionRecord::read(&conn, &key)?;
    let now = now_ms();
    if !record
        .running
        .iter()
        .any(|deletion| deletion_left_over(deletion, now))
    {
        return Ok(record.view());
    }
    update_chain_deletions(&conn, &key, |_| {})
}

/// Record a deletion of `account` for `owner` as running, before anything
/// is deleted; [`end_chain_deletion`] ends it.
pub(crate) fn begin_chain_deletion(
    store: &CredentialStore,
    owner: Option<i64>,
    account: &str,
) -> Result<ChainDeletion, String> {
    #[cfg(test)]
    if CHAIN_DELETION_BEGIN_REFUSED.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("Begin chain deletion: refused (test)".to_string());
    }
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let key = chain_deletion_key(store, owner, account)?;
    let this = this_process();
    let deletion = ChainDeletion {
        id: hex::encode(crate::crypto::random_bytes(16)),
    };
    // Running here before it is recorded: no reader of this process takes
    // it for one left over.
    deletions_running_here().insert(deletion.id.clone());
    update_chain_deletions(&conn, &key, |record| {
        record.running.push(RunningDeletion {
            id: deletion.id.clone(),
            process: this.token.clone(),
            space: this.space.clone(),
            pid: this.pid,
            start: this.start,
            at: now_ms(),
        })
    })?;
    Ok(deletion)
}

/// End `deletion` of `account` for `owner`: completed when `done`, else
/// taken back, so a delete that failed leaves a rotation kept for the key as
/// usable as before it. One a reader already settled as left over counts as
/// completed either way. Tried three times; one whose end is never recorded
/// is settled as completed by the next read of the key in this process,
/// which [`settle_unended_chain_deletions`] makes.
pub(crate) fn end_chain_deletion(
    store: &CredentialStore,
    owner: Option<i64>,
    account: &str,
    deletion: ChainDeletion,
    done: bool,
) -> Result<(), String> {
    let end = || -> Result<(), String> {
        #[cfg(test)]
        if CHAIN_DELETION_END_REFUSED.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("End chain deletion: refused (test)".to_string());
        }
        init_or_migrate_cli(store)?;
        let conn = open_or_init_cli()?;
        let key = chain_deletion_key(store, owner, account)?;
        update_chain_deletions(&conn, &key, |record| {
            let before = record.running.len();
            record.running.retain(|running| running.id != deletion.id);
            if done || record.running.len() == before {
                record.generation += 1;
            }
        })
        .map(|_| ())
    };
    let mut result = end();
    for attempt in 1..3 {
        if result.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50 * attempt));
        result = end();
    }
    if result.is_err() {
        // Over here first: a sweep that reads the record from now on finds
        // it left over and settles it. Published while it still ran here, a
        // sweep would read it as running, leave it, and drop the entry.
        let id = deletion.id.clone();
        drop(deletion);
        unended_chain_deletions().insert((owner, account.to_string(), id));
        #[cfg(test)]
        if SWEEP_AFTER_UNENDED.load(std::sync::atomic::Ordering::SeqCst) {
            settle_unended_chain_deletions(store);
        }
    }
    result
}

/// A sweep of the unended deletions runs right after one is recorded (test
/// only).
#[cfg(test)]
pub(crate) static SWEEP_AFTER_UNENDED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// A deletion [`unended_chain_deletions`] keeps: (owner, key, its id).
type UnendedDeletion = (Option<i64>, String, String);

/// The deletions this process began and could not record as ended, by
/// (owner, key, id). This process counts each as completed at its next read of
/// that key, but every other process takes it for one still running for as
/// long as this one lives, and would refuse that key's chain meanwhile: so
/// this process settles them again ([`settle_unended_chain_deletions`]) until
/// that is recorded.
fn unended_chain_deletions() -> MutexGuard<'static, HashSet<UnendedDeletion>> {
    static UNENDED: std::sync::LazyLock<Mutex<HashSet<UnendedDeletion>>> =
        std::sync::LazyLock::new(Default::default);
    UNENDED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Settle every deletion this process could not record as ended
/// ([`unended_chain_deletions`]): a read of the key's deletions here counts
/// it as left over and commits that, for every process. One whose read fails
/// is tried again next time. Called when a connection begins and before a
/// deletion, wherever that connection's key is.
/// An entry goes only once the record no longer lists its deletion as
/// running: a read that left it (it still ran here) or failed keeps it.
pub(crate) fn settle_unended_chain_deletions(store: &CredentialStore) {
    let unended: Vec<UnendedDeletion> = unended_chain_deletions().iter().cloned().collect();
    for (owner, account, id) in unended {
        if chain_deletions(store, owner, &account).is_err() {
            continue;
        }
        if running_chain_deletions(store, owner, &account)
            .is_ok_and(|running| !running.contains(&id))
        {
            unended_chain_deletions().remove(&(owner, account, id));
        }
    }
}

/// The ids of the deletions of `account` for `owner` the record lists as
/// running, as stored.
fn running_chain_deletions(
    store: &CredentialStore,
    owner: Option<i64>,
    account: &str,
) -> Result<Vec<String>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let record = DeletionRecord::read(&conn, &chain_deletion_key(store, owner, account)?)?;
    Ok(record
        .running
        .into_iter()
        .map(|running| running.id)
        .collect())
}

/// How many deletions of `account` for `owner` the record lists as running,
/// read without settling any (test only).
#[cfg(test)]
pub(crate) fn chain_deletions_running_raw(
    store: &CredentialStore,
    owner: Option<i64>,
    account: &str,
) -> usize {
    running_chain_deletions(store, owner, account)
        .expect("read the record")
        .len()
}

/// Whether this process still keeps an unended deletion of `account` for
/// `owner` to settle (test only).
#[cfg(test)]
pub(crate) fn keeps_unended_deletion(owner: Option<i64>, account: &str) -> bool {
    unended_chain_deletions()
        .iter()
        .any(|(o, a, _)| *o == owner && a == account)
}

/// Guard an implicit GUI account during a global/memory token update. Resolve
/// the session before SQLite, and never perform network I/O inside `refresh`.
pub(crate) fn with_unchanged_active_credential_user<R>(
    store: Option<&CredentialStore>,
    expected: Option<i64>,
    refresh: impl FnOnce() -> Result<R, String>,
) -> Result<R, String> {
    let conn = open_or_init_cli()?;
    let apply = || {
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Begin active authorization refresh: {e}"))?;
        if active_user_id(&tx)? != expected {
            return Err(ACCOUNT_CHANGED.to_owned());
        }
        let result = refresh()?;
        tx.commit()
            .map_err(|e| format!("Commit active authorization refresh: {e}"))?;
        Ok(result)
    };
    match expected {
        Some(user) if store.is_some() => {
            let root = Zeroizing::new(
                store
                    .expect("checked store")
                    .derive_user_partition_wrapping_key(),
            );
            let key = user_crypto::secret_key_from_bytes(&root);
            with_user_dek(&conn, &key, user, |_, _| apply())
        }
        _ => {
            let _session = hold_session()?;
            if let Some(user) = expected {
                ensure_credential_user_unlocked(user)?;
            }
            apply()
        }
    }
}

/// Compute a domain-separated journal owner tag with the active partition's
/// real DEK. No derived key or credential fingerprint leaves this boundary.
pub(crate) fn sync_owner_tag_for_user(
    store: &CredentialStore,
    user: i64,
    material: &str,
) -> Result<String, String> {
    let conn = open_or_init_cli()?;
    let root = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let root = user_crypto::secret_key_from_bytes(&root);
    with_user_dek(&conn, &root, user, |user, dek| {
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Deferred)
                .map_err(|e| format!("Begin journal owner check: {e}"))?;
        if active_user_id(&tx)? != Some(user) {
            return Err(ACCOUNT_CHANGED.to_owned());
        }
        let tag = user_crypto::metadata_tag(dek, b"sync-journal-owner-v1", material)?;
        tx.commit()
            .map_err(|e| format!("Commit journal owner check: {e}"))?;
        Ok(tag)
    })
}

/// Delete one secret from a user's partition. No DEK is required: a row is
/// removed by its `(user_id, credential_id)` key without decrypting anything.
pub fn delete_user_credential_for(
    conn: &Connection,
    user_id: i64,
    credential_id: &str,
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM user_credentials WHERE user_id = ?1 AND credential_id = ?2",
        params![user_id, credential_id],
    )
    .map_err(|e| format!("Delete user credential: {e}"))?;
    Ok(())
}

/// List the `(credential_id, credential_type)` pairs stored for a user, without
/// decrypting any secret. Used by MUV-2 (migration bookkeeping) and the tests.
pub fn list_user_credential_ids_for(
    conn: &Connection,
    user_id: i64,
) -> Result<Vec<(String, String)>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT credential_id, credential_type FROM user_credentials
             WHERE user_id = ?1 ORDER BY credential_id ASC",
        )
        .map_err(|e| format!("Prepare list user credentials: {e}"))?;
    let rows = stmt
        .query_map(params![user_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("Query list user credentials: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("Read user credential ids: {e}"))
}

/// Active-user wrapper for [`set_user_credential_for`].
pub fn set_active_user_credential(
    conn: &Connection,
    root_key: &[u8; 32],
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    set_user_credential_for(
        conn,
        root_key,
        user_id,
        credential_id,
        credential_type,
        secret,
    )
}

/// Active-user wrapper for [`delete_user_credential_for`].
pub fn delete_active_user_credential(conn: &Connection, credential_id: &str) -> Result<(), String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    delete_user_credential_for(conn, user_id, credential_id)
}

// --- MUV-2: copy-only migration of legacy vault secrets into user_credentials -
//
// The bulk copy moves each user's raw secrets from the global `vault.db` into
// `user_credentials`, re-encrypted under that user's DEK. It is copy-only
// (never deletes from the vault; cleanup is MUV-6), idempotent (per-user marker
// in global_state), eager for device-wrapped users (DEK reachable at boot) and
// lazy for passphrase users (migrated at first unlock when the session DEK is
// primed). The engine is closure-backed over the vault so it is unit-testable
// without a real CredentialStore; production wires the closure to the store.

const CREDS_MIGRATED_PREFIX: &str = "creds_migrated_";

/// Closure that reads one secret from the legacy vault, mapping "absent" to
/// `None`. Lets the migration engine run against a fake vault in tests.
type SecretReader<'a> = dyn Fn(&str) -> Result<Option<Zeroizing<String>>, String> + 'a;

/// Deletes one vault key after the alignment committed. `expected` is the
/// value the pass read: the delete lands only while the vault still holds
/// it, and `Ok(false)` reports a value another writer stored since, which
/// keeps the alignment pending for the next pass.
type SecretDeleter<'a> = dyn Fn(&str, Option<&str>) -> Result<bool, String> + 'a;

/// Closure that lists the legacy vault's keys now.
type SecretLister<'a> = dyn Fn() -> Result<Vec<String>, String> + 'a;

/// Marks an account whose private tokens left the vault ([`align_private_tokens_inner`]).
const PRIVATE_TOKENS_ALIGNED_PREFIX: &str = "private_tokens_aligned_";

fn creds_migrated_key(user_id: i64) -> String {
    format!("{CREDS_MIGRATED_PREFIX}{user_id}")
}

fn private_tokens_aligned(conn: &Connection, user_id: i64) -> Result<bool, String> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM global_state WHERE key = ?1",
            params![format!("{PRIVATE_TOKENS_ALIGNED_PREFIX}{user_id}")],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Read private token marker: {e}"))?;
    Ok(value.as_deref() == Some("1"))
}

fn mark_private_tokens_aligned(conn: &Connection, user_id: i64) -> Result<(), String> {
    conn.execute(
        "INSERT INTO global_state(key, value, updated_at)
         VALUES (?1, '1', ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![
            format!("{PRIVATE_TOKENS_ALIGNED_PREFIX}{user_id}"),
            now_ms()
        ],
    )
    .map_err(|e| format!("Write private token marker: {e}"))?;
    Ok(())
}

/// Both one-time passes over an account's legacy secrets are done, and no
/// targeted pass after a restore waits ([`defer_realign`]).
fn credentials_settled(conn: &Connection, user_id: i64) -> Result<bool, String> {
    Ok(is_creds_migrated(conn, user_id)?
        && private_tokens_aligned(conn, user_id)?
        && deferred_realign_tags(conn, user_id)?.is_none())
}

fn is_creds_migrated(conn: &Connection, user_id: i64) -> Result<bool, String> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM global_state WHERE key = ?1",
            params![creds_migrated_key(user_id)],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Read creds migration marker: {e}"))?;
    Ok(value.as_deref() == Some("1"))
}

fn mark_creds_migrated(conn: &Connection, user_id: i64) -> Result<(), String> {
    conn.execute(
        "INSERT INTO global_state(key, value, updated_at)
         VALUES (?1, '1', ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![creds_migrated_key(user_id), now_ms()],
    )
    .map_err(|e| format!("Write creds migration marker: {e}"))?;
    Ok(())
}

fn session_user_id() -> Option<i64> {
    USER_SESSION
        .lock()
        .ok()
        .and_then(|session| session.as_ref().map(|s| s.user_id))
}

/// Owner of the pre-partition globals (the legacy vault fallback, `ai_apikey_*`,
/// github tokens). Routing them to a single account avoids duplicating them
/// across every admin (MUV-0 / sec 2.3). The owner is established once, as the
/// lowest-id admin (the migration's `default`, and what every earlier release
/// computed), then persisted: a later demotion, deletion or new admin never
/// moves it, and a deleted owner leaves no owner instead of handing its legacy
/// secrets to the next admin. User ids are AUTOINCREMENT, so never reused.
fn nonprofile_secret_owner(conn: &Connection) -> Result<Option<i64>, String> {
    nonprofile_secret_owner_in(conn, true)
}

/// [`nonprofile_secret_owner`] on a connection that may be read-only: the
/// owner not yet persisted is the one the first writer would persist, and is
/// returned without storing it.
pub(crate) fn nonprofile_secret_owner_unpersisted(
    conn: &Connection,
) -> Result<Option<i64>, String> {
    nonprofile_secret_owner_in(conn, false)
}

fn nonprofile_secret_owner_in(conn: &Connection, persist: bool) -> Result<Option<i64>, String> {
    let read_stored = || {
        conn.query_row(
            "SELECT value FROM global_state WHERE key = ?1",
            params![LEGACY_OWNER_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| format!("Read legacy credential owner: {e}"))
    };
    let stored = match read_stored()? {
        Some(value) => value,
        None => {
            let first_admin = conn
                .query_row(
                    "SELECT id FROM users WHERE is_admin = 1 ORDER BY id ASC LIMIT 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(|e| format!("Read nonprofile secret owner: {e}"))?;
            let Some(first_admin) = first_admin else {
                return Ok(None);
            };
            if !persist {
                return Ok(Some(first_admin));
            }
            conn.execute(
                "INSERT OR IGNORE INTO global_state(key, value, updated_at) VALUES (?1, ?2, ?3)",
                params![LEGACY_OWNER_KEY, first_admin.to_string(), now_ms()],
            )
            .map_err(|e| format!("Persist legacy credential owner: {e}"))?;
            // Re-read: concurrent first writers settle on the one row stored.
            read_stored()?.ok_or_else(|| "Legacy credential owner not stored".to_string())?
        }
    };
    let owner = stored
        .parse::<i64>()
        .map_err(|e| format!("Read legacy credential owner: {e}"))?;
    let exists = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id = ?1)",
            params![owner],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|e| format!("Read legacy credential owner: {e}"))?;
    Ok(exists.then_some(owner))
}

/// Remove one secret from the live store; one already absent is not an error.
#[cfg(any(test, not(feature = "proton-native")))]
fn delete_optional_secret(store: &CredentialStore, account: &str) -> Result<(), String> {
    match store.delete(account) {
        Ok(()) | Err(CredentialError::NotFound(_)) => Ok(()),
        Err(e) => Err(format!("Delete legacy secret: {e}")),
    }
}

/// Read one optional secret from the live store, mapping NotFound to None.
/// The vault's keys as it lists them now.
fn list_vault_keys(store: &CredentialStore) -> Result<Vec<String>, String> {
    store
        .list_accounts()
        .map_err(|e| format!("List vault accounts: {e}"))
}

fn get_optional_secret(
    store: &CredentialStore,
    account: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    match store.get_secret(account) {
        Ok(value) => Ok(Some(value)),
        Err(CredentialError::NotFound(_)) => Ok(None),
        Err(e) => Err(format!("Read legacy secret: {e}")),
    }
}

/// Copy one legacy secret into `user_credentials` when it is not already there
/// and the vault has it. Returns 1 on insert, 0 otherwise. Copy-only: the
/// legacy vault entry is never touched.
fn copy_one_credential(
    tx: &Transaction<'_>,
    user_id: i64,
    dek: &SecretKey,
    credential_id: &str,
    credential_type: &str,
    read_secret: &SecretReader<'_>,
) -> Result<usize, String> {
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM user_credentials
             WHERE user_id = ?1 AND credential_id = ?2)",
            params![user_id, credential_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Check existing credential: {e}"))?;
    if exists {
        return Ok(0);
    }
    let Some(secret) = read_secret(credential_id)? else {
        return Ok(0);
    };
    let (encrypted_blob, nonce) = user_crypto::encrypt_blob(dek, secret.as_bytes())?;
    tx.execute(
        "INSERT INTO user_credentials(
             user_id, credential_id, credential_type, encrypted_blob, nonce,
             aead_alg, updated_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, 'aes-256-gcm', ?6)",
        params![
            user_id,
            credential_id,
            credential_type,
            encrypted_blob,
            nonce.to_vec(),
            now_ms()
        ],
    )
    .map_err(|e| format!("Insert migrated credential: {e}"))?;
    Ok(1)
}

/// Copy every legacy secret belonging to `user_id` under the resolved `dek`.
/// Profile-bound secrets (`server_<id>`, `oauth_<slug>_<id>`,
/// `jottacloud_refresh_<id>`) are matched against the user's own profile ids.
/// When `owns_global_secrets` is true the user also receives the non-profile
/// per-user globals (`ai_apikey_*`, `github_oauth_token`, `github_pat`).
/// `github_pem_*` / `github_app_credentials` are machine-global (MUV-0) and are
/// deliberately NOT migrated. A `server_<id>` of a profile id another account
/// also holds is taken by the legacy owner only: the vault entry may be that
/// account's, and a copy would keep it in this partition after its owner
/// changes or removes it. Reading is unchanged: the fallback reader still
/// gives the vault entry to an account that has no row of its own.
fn copy_user_secrets_with_dek(
    conn: &Connection,
    root_secret: &SecretKey,
    user_id: i64,
    dek: &SecretKey,
    owns_global_secrets: bool,
    vault_keys: &[String],
    read_secret: &SecretReader<'_>,
) -> Result<usize, String> {
    let profiles = read_profiles_with_dek(conn, user_id, dek)?;
    let profile_ids: Vec<String> = profiles
        .iter()
        .filter_map(|p| value_str(p, &["id", "uid", "profileUid"]).map(str::to_string))
        .collect();

    // IMMEDIATE, as `write_profiles_with_dek`: each copy reads before it
    // inserts, which a deferred transaction on this WAL database turns into
    // SQLITE_BUSY_SNAPSHOT when another process commits in between. Called
    // under the session lock (`with_user_dek`), the order every writer takes.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Start credential migration: {e}"))?;
    let mut migrated = 0usize;

    for id in &profile_ids {
        let shared = || -> Result<bool, String> {
            let tag = user_crypto::metadata_tag(root_secret, b"profile-uid", id)?;
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM server_profiles
                 WHERE profile_uid = ?1 AND user_id <> ?2)",
                params![tag, user_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Read profile holders: {e}"))
        };
        if owns_global_secrets || !shared()? {
            migrated += copy_one_credential(
                &tx,
                user_id,
                dek,
                &format!("server_{id}"),
                "server",
                read_secret,
            )?;
        }
        // Another account's private tokens are not taken from the vault here:
        // the vault key is the legacy owner's, also of a profile id both hold.
        // [`align_private_tokens_inner`] moves the ones of its own profiles.
        let taken = |key: &str| owns_global_secrets || private_token_type(key).is_none();
        let jottacloud = format!("jottacloud_refresh_{id}");
        if taken(&jottacloud) {
            migrated += copy_one_credential(
                &tx,
                user_id,
                dek,
                &jottacloud,
                "jottacloud_refresh",
                read_secret,
            )?;
        }
        // An OAuth token's key as its runtime composes it, which may lowercase
        // the id ([`crate::providers::oauth2::oauth_token_key`]).
        let suffix = format!("_{id}");
        let composed = private_token_keys_of_profile(id);
        for key in vault_keys.iter().filter(|k| {
            k.starts_with("oauth_") && (k.ends_with(&suffix) || composed.contains(k)) && taken(k)
        }) {
            migrated += copy_one_credential(&tx, user_id, dek, key, "oauth", read_secret)?;
        }
    }

    if owns_global_secrets {
        for key in vault_keys.iter().filter(|k| k.starts_with("ai_apikey_")) {
            migrated += copy_one_credential(&tx, user_id, dek, key, "ai_apikey", read_secret)?;
        }
        for key in ["github_oauth_token", "github_pat"] {
            migrated += copy_one_credential(&tx, user_id, dek, key, "github", read_secret)?;
        }
    }

    tx.commit()
        .map_err(|e| format!("Commit credential migration: {e}"))?;
    Ok(migrated)
}

/// Migrate one user's legacy secrets (engine, closure-backed vault). Idempotent
/// via the `creds_migrated_<id>` marker. A passphrase account without a primed
/// session is skipped (`Ok(0)`, left unmarked) so it migrates lazily at first
/// unlock. Copy-only.
fn migrate_user_credentials_inner(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    vault_keys: &[String],
    read_secret: &SecretReader<'_>,
) -> Result<usize, String> {
    if is_creds_migrated(conn, user_id)? {
        return Ok(0);
    }
    let owns_global_secrets = nonprofile_secret_owner(conn)? == Some(user_id);
    // Which profile ids another account holds is read from the tags.
    if !owns_global_secrets && profile_tags_pending(conn)? {
        return Ok(0);
    }
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let outcome = with_user_dek(conn, &root_secret, user_id, |uid, dek| {
        copy_user_secrets_with_dek(
            conn,
            &root_secret,
            uid,
            dek,
            owns_global_secrets,
            vault_keys,
            read_secret,
        )
    });
    match outcome {
        Ok(migrated) => {
            mark_creds_migrated(conn, user_id)?;
            Ok(migrated)
        }
        // A locked passphrase account has no DEK yet: defer to first unlock.
        Err(e) if e == "USER_LOCKED" => Ok(0),
        Err(e) => Err(e),
    }
}

/// The private token keys ([`private_token_type`]) a profile's id gives.
fn private_token_keys_of_profile(profile_id: &str) -> Vec<String> {
    let mut keys = vec![
        format!("jottacloud_refresh_{profile_id}"),
        format!("{PROTON_SESSION_PREFIX}{profile_id}"),
    ];
    keys.extend(
        OAUTH_TOKEN_SLUGS
            .iter()
            .map(|slug| crate::providers::oauth2::oauth_token_key(slug, profile_id)),
    );
    keys.extend(
        crate::profile_auth::DURABLE_KEY_PREFIXES
            .iter()
            .chain(crate::profile_auth::TRANSIENT_KEY_PREFIXES.iter())
            .map(|prefix| format!("{prefix}_{profile_id}")),
    );
    keys.retain(|key| private_token_type(key).is_some());
    keys
}

/// Whether `key`, a private token key of a profile, lowercases the profile id
/// ([`crate::providers::oauth2::oauth_token_key_folds_case`]): two profile ids
/// that differ only in case then name it both.
fn token_key_folds_case(key: &str) -> bool {
    OAUTH_TOKEN_SLUGS.iter().any(|slug| {
        key.strip_prefix("oauth_")
            .and_then(|rest| rest.strip_prefix(slug))
            .is_some_and(|tail| tail.starts_with('_'))
            && crate::providers::oauth2::oauth_token_key_folds_case(slug)
    })
}

/// Whether the alignment of `user_id` has `key` to move or to drop: a vault
/// key of that name, or a row of it (read without decrypting).
fn token_key_present(
    conn: &Connection,
    user_id: i64,
    vault_keys: &[String],
    key: &str,
) -> Result<bool, String> {
    if vault_keys.iter().any(|vault_key| vault_key == key) {
        return Ok(true);
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM user_credentials WHERE user_id = ?1 AND credential_id = ?2)",
        params![user_id, key],
        |row| row.get(0),
    )
    .map_err(|e| format!("Read credential rows: {e}"))
}

/// MUV-6 for private tokens: the tokens an account other than the legacy owner
/// saved before they were kept to its partition still sit in the vault, where
/// the vault key reads as the owner's (a backup of the vault carries it, the
/// owner's fallback answers it). Each one of this account's profiles is copied
/// into its partition when no row is there yet, then removed from the vault.
/// A profile id another account also holds is left: the vault copy cannot be
/// told apart, and the legacy owner reads it as its own; a row of this account
/// that its own sign-in did not stamp ([`signed_in_row`]), may be a
/// copy an earlier migration made of it, and is dropped. Idempotent via the
/// `private_tokens_aligned_<id>` marker, set once the vault has let go of the
/// keys moved; a locked passphrase account is left unmarked until its first
/// unlock, as for [`migrate_user_credentials_inner`].
///
/// A native Proton Drive session belongs to one account: a copy of a session
/// (same UID) that another account still holds in the vault is dropped here,
/// row and vault copy, and the other account keeps it. The drop is local
/// only, never a sign-out on Proton, which would end the session for the
/// account that keeps it. This account signs in again.
fn align_private_tokens_inner(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    vault_keys: &[String],
    list_secrets: &SecretLister<'_>,
    read_secret: &SecretReader<'_>,
    delete_secret: &SecretDeleter<'_>,
) -> Result<usize, String> {
    let realigned = realign_deferred_tokens(
        conn,
        root_key,
        user_id,
        vault_keys,
        list_secrets,
        read_secret,
        delete_secret,
    )?;
    let aligned = align_private_tokens_of(
        conn,
        root_key,
        user_id,
        vault_keys,
        None,
        read_secret,
        delete_secret,
    )?;
    Ok(realigned + aligned.unwrap_or(0))
}

/// Marks the private token keys a targeted pass after a restore could not
/// align yet ([`realign_private_tokens_after_restore`]), by their tags with
/// this machine's root (a key names its profile id, which the partition
/// keeps as a tag only), as a JSON array.
const PRIVATE_TOKENS_REALIGN_PREFIX: &str = "private_tokens_realign_";

fn realign_marker_key(user_id: i64) -> String {
    format!("{PRIVATE_TOKENS_REALIGN_PREFIX}{user_id}")
}

/// Counts every deferral of `user_id`'s keys ([`defer_realign`]) and is never
/// reset: a pass that finds it moved since it read the marker cannot tell a
/// key deferred again, restored after the pass ran it, from the deferral it
/// ran, so it keeps that key's tag for the next pass.
fn realign_generation_key(user_id: i64) -> String {
    format!("{PRIVATE_TOKENS_REALIGN_PREFIX}generation_{user_id}")
}

/// The deferrals of `user_id`'s keys so far ([`realign_generation_key`]).
fn realign_generation(conn: &Connection, user_id: i64) -> Result<u64, String> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM global_state WHERE key = ?1",
            params![realign_generation_key(user_id)],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Read deferred private token alignment: {e}"))?;
    value.map_or(Ok(0), |raw| {
        raw.parse()
            .map_err(|e| format!("Read deferred private token alignment: {e}"))
    })
}

/// The digest of the value the private-token alignment last wrote into each
/// of `user_id`'s rows, by credential id, kept in `global_state` until the
/// vault copy's fate is known: a later pass replaces such a row with a
/// rotated vault chain only while the row still holds that value
/// ([`align_private_tokens_of`]).
const PRIVATE_TOKENS_INSERTED_PREFIX: &str = "private_tokens_inserted_";

fn inserted_row_digests_key(user_id: i64) -> String {
    format!("{PRIVATE_TOKENS_INSERTED_PREFIX}{user_id}")
}

/// SHA-256 over the row's plaintext: enough to tell the value the alignment
/// wrote is still there without keeping the token itself in `global_state`.
fn private_token_value_digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn read_inserted_row_digests(
    conn: &Connection,
    user_id: i64,
) -> Result<HashMap<String, String>, String> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM global_state WHERE key = ?1",
            params![inserted_row_digests_key(user_id)],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Read inserted private token rows: {e}"))?;
    value
        .map(|raw| {
            serde_json::from_str(&raw).map_err(|e| format!("Read inserted private token rows: {e}"))
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn write_inserted_row_digests(
    conn: &Connection,
    user_id: i64,
    digests: &HashMap<String, String>,
) -> Result<(), String> {
    if digests.is_empty() {
        conn.execute(
            "DELETE FROM global_state WHERE key = ?1",
            params![inserted_row_digests_key(user_id)],
        )
        .map_err(|e| format!("Clear inserted private token rows: {e}"))?;
        return Ok(());
    }
    let value = serde_json::to_string(digests)
        .map_err(|e| format!("Record inserted private token rows: {e}"))?;
    conn.execute(
        "INSERT INTO global_state(key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![inserted_row_digests_key(user_id), value, now_ms()],
    )
    .map_err(|e| format!("Record inserted private token rows: {e}"))?;
    Ok(())
}

/// What [`align_private_tokens_of`] did with one sole key's row.
enum SoleRow {
    /// The row now holds the vault chain, written by the alignment or already
    /// equal to it: the vault copy may go.
    Moved,
    /// The row holds a value the alignment wrote before but no longer does,
    /// while the vault chain differs: another writer's row, left alone with
    /// its vault copy.
    Skipped,
}

/// The row side of one [`align_private_tokens_of`] sole key, inside its
/// transaction. A missing row takes the vault chain; a row already equal to
/// it needs nothing; a row still holding the value an earlier pass of this
/// alignment wrote ([`inserted`]) takes the vault chain too, in the same
/// transaction; a row that moved away from that value is another writer's
/// and keeps its vault copy; any other row keeps the old rule, the vault
/// copy still leaves.
fn sole_row_state(
    tx: &Connection,
    user_id: i64,
    dek: &SecretKey,
    key: &str,
    kind: &str,
    vault: Option<&str>,
    inserted: &mut HashMap<String, String>,
) -> Result<SoleRow, String> {
    let Some(vault) = vault else {
        return Ok(SoleRow::Moved);
    };
    let digest = private_token_value_digest(vault);
    match read_credential_with_dek(tx, user_id, dek, key)? {
        None => {
            set_user_credential_with_dek(tx, user_id, dek, key, kind, vault)?;
            inserted.insert(key.to_string(), digest);
            Ok(SoleRow::Moved)
        }
        Some(row) if row.as_str() == vault => {
            if inserted.get(key) != Some(&digest) {
                inserted.remove(key);
            }
            Ok(SoleRow::Moved)
        }
        Some(row) => {
            // A row the alignment never wrote keeps the old rule: it wins
            // over the vault copy, which still leaves. Only a row that moved
            // away from a value this alignment wrote is another writer's,
            // and it keeps its vault copy.
            let Some(recorded) = inserted.get(key).cloned() else {
                return Ok(SoleRow::Moved);
            };
            if recorded != private_token_value_digest(row.as_str()) {
                inserted.remove(key);
                return Ok(SoleRow::Skipped);
            }
            set_user_credential_with_dek(tx, user_id, dek, key, kind, vault)?;
            inserted.insert(key.to_string(), digest);
            Ok(SoleRow::Moved)
        }
    }
}

/// The tags of the keys a targeted pass left to the next one, `None` when
/// none waits.
fn deferred_realign_tags(conn: &Connection, user_id: i64) -> Result<Option<Vec<String>>, String> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM global_state WHERE key = ?1",
            params![realign_marker_key(user_id)],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Read deferred private token alignment: {e}"))?;
    value
        .map(|raw| {
            serde_json::from_str(&raw)
                .map_err(|e| format!("Read deferred private token alignment: {e}"))
        })
        .transpose()
}

/// Writes `tags` as the keys `user_id`'s next pass realigns, inside the
/// caller's transaction; no tag left clears the marker.
fn write_realign_tags(conn: &Connection, user_id: i64, tags: &[String]) -> Result<(), String> {
    if tags.is_empty() {
        conn.execute(
            "DELETE FROM global_state WHERE key = ?1",
            params![realign_marker_key(user_id)],
        )
        .map_err(|e| format!("Clear deferred private token alignment: {e}"))?;
        return Ok(());
    }
    let value =
        serde_json::to_string(tags).map_err(|e| format!("Defer private token alignment: {e}"))?;
    conn.execute(
        "INSERT INTO global_state(key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![realign_marker_key(user_id), value, now_ms()],
    )
    .map_err(|e| format!("Defer private token alignment: {e}"))?;
    Ok(())
}

/// Leaves `keys` to the next pass of `user_id` ([`realign_deferred_tokens`]),
/// with any it left before.
fn defer_realign(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    keys: &[String],
) -> Result<(), String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    // Read and written in one transaction: two restores deferring at once
    // both keep their keys.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Defer private token alignment: {e}"))?;
    let mut tags = deferred_realign_tags(&tx, user_id)?.unwrap_or_default();
    for key in keys {
        tags.push(user_crypto::metadata_tag(
            &root_secret,
            b"realign-key",
            key,
        )?);
    }
    tags.sort();
    tags.dedup();
    write_realign_tags(&tx, user_id, &tags)?;
    let generation = realign_generation(&tx, user_id)? + 1;
    upsert_global_state(
        &tx,
        &realign_generation_key(user_id),
        &generation.to_string(),
        now_ms(),
    )?;
    tx.commit()
        .map_err(|e| format!("Defer private token alignment: {e}"))
}

#[cfg(test)]
thread_local! {
    /// Runs once on this thread in the next [`realign_deferred_tokens`],
    /// after its pass and before it clears the marker: a restore deferring
    /// keys meanwhile.
    static BEFORE_REALIGN_CLEAR: DualWriteHook = const { RefCell::new(None) };
}

/// The targeted pass a restore could not run ([`defer_realign`]), run by the
/// next ordinary pass of the account over the same keys alone: the tags of
/// the keys it ran go once it ran, and every other tag stays, a restore's
/// deferred since included, for the next pass, whose vault list holds its
/// key. A tag whose key the vault no longer lists names nothing left to
/// align, and goes too. A restore that deferred any key since this pass read
/// the marker ([`realign_generation_key`]) may have deferred one this pass
/// ran again, restored after the pass let the list lock go: then the tags of
/// the keys it ran stay too, and the next pass runs them again. Its aligned
/// marker stays as the restore left it, so the profiles the restore brought
/// get no pass of their own.
fn realign_deferred_tokens(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    vault_keys: &[String],
    list_secrets: &SecretLister<'_>,
    read_secret: &SecretReader<'_>,
    delete_secret: &SecretDeleter<'_>,
) -> Result<usize, String> {
    // Before the marker: a deferral made after this read moves it.
    let generation = realign_generation(conn, user_id)?;
    let Some(tags) = deferred_realign_tags(conn, user_id)? else {
        return Ok(0);
    };
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let realign_tag = |key: &str| user_crypto::metadata_tag(&root_secret, b"realign-key", key);
    let (mut only, mut covered) = (Vec::new(), Vec::new());
    for key in vault_keys
        .iter()
        .filter(|key| private_token_type(key).is_some())
    {
        let tag = realign_tag(key)?;
        if tags.contains(&tag) {
            only.push(key.clone());
            covered.push(tag);
        }
    }
    let aligned = if only.is_empty() {
        Some(0)
    } else {
        align_private_tokens_of(
            conn,
            root_key,
            user_id,
            vault_keys,
            Some(&only),
            read_secret,
            delete_secret,
        )?
    };
    let Some(aligned) = aligned else {
        return Ok(0);
    };
    #[cfg(test)]
    run_dual_write_hook(&BEFORE_REALIGN_CLEAR);
    // Read, subtracted and written in one transaction, as a deferral writes
    // it: a tag deferred since this pass read the marker stays. The vault is
    // listed inside it (the database, then the vault), so a key restored after
    // that list defers its tag after this commit, and a tag kept for lack of
    // a listed key cannot outlive its key.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Clear deferred private token alignment: {e}"))?;
    let Some(current) = deferred_realign_tags(&tx, user_id)? else {
        return Ok(aligned);
    };
    let mut listed = HashSet::new();
    for key in list_secrets()?
        .iter()
        .filter(|key| private_token_type(key).is_some())
    {
        listed.insert(realign_tag(key)?);
    }
    let deferred_since = realign_generation(&tx, user_id)? != generation;
    let left: Vec<String> = current
        .into_iter()
        .filter(|tag| (deferred_since || !covered.contains(tag)) && listed.contains(tag))
        .collect();
    write_realign_tags(&tx, user_id, &left)?;
    tx.commit()
        .map_err(|e| format!("Clear deferred private token alignment: {e}"))?;
    Ok(aligned)
}

/// [`align_private_tokens_inner`], over the keys in `only` alone when given:
/// one pass of the same rule for an account already aligned, which neither
/// reads nor sets its marker ([`realign_private_tokens_after_restore`]).
/// `None`: the pass could not run now (the list lock busy, profile tags
/// pending, the account locked) and changed nothing, or a targeted pass
/// copied keys the vault could not let go of (no file locks), which a later
/// pass must still remove from the vault, or aligned every key but one that
/// waits for another account's token tags ([`fill_profile_token_tags`]).
/// A full pass that leaves such a key aligns the rest and leaves the account
/// unmarked for the pass that runs it.
fn align_private_tokens_of(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    vault_keys: &[String],
    only: Option<&[String]>,
    read_secret: &SecretReader<'_>,
    delete_secret: &SecretDeleter<'_>,
) -> Result<Option<usize>, String> {
    if only.is_none() && private_tokens_aligned(conn, user_id)? {
        return Ok(Some(0));
    }
    if nonprofile_secret_owner(conn)? == Some(user_id) {
        if only.is_none() {
            write_inserted_row_digests(conn, user_id, &HashMap::new())?;
            mark_private_tokens_aligned(conn, user_id)?;
        }
        return Ok(Some(0));
    }
    // Which profiles another account holds must not change until the vault has
    // let go: a profile writer, or the save of a token for a profile, holds the
    // list lock. It is not re-entrant and a caller may hold it (an import's
    // secret writes reach this through `init_or_migrate_cli`), so a busy lock
    // leaves the account unmarked for the next pass, as `mirror_after_load` does.
    let lock = match conn.path() {
        Some(path) if !path.is_empty() => {
            match ProfileListLock::acquire_within(
                std::path::Path::new(path),
                std::time::Duration::ZERO,
            ) {
                Ok(lock) => Some(lock),
                Err(e) if e == PROFILE_LIST_BUSY => {
                    tracing::debug!("Private tokens not aligned by this pass: {e}");
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
        }
        _ => None,
    };
    // On a filesystem without file locks the list lock excludes nobody: the
    // copies are still made, the holders read in the same transaction as every
    // profile writer's, but the vault keeps its keys, since a holder added after
    // the commit would lose the one it reads.
    let vault_lets_go = lock.as_ref().is_none_or(ProfileListLock::is_exclusive);
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let outcome = with_user_dek(conn, &root_secret, user_id, |uid, dek| {
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| format!("Start private token alignment: {e}"))?;
        // The holders below are read from tags: one made with another
        // machine's key would hide its account, so the pass waits for them.
        if profile_tags_pending(&tx)? {
            return Ok(None);
        }
        let profiles = read_profiles_with_dek(&tx, uid, dek)?;
        // A list written before profiles had a token tag hides which token
        // keys its account's profiles name: a token key that folds case may
        // be one of its profiles' too.
        let token_tags_pending: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM server_profiles
                 WHERE user_id <> ?1 AND token_uid IS NULL)",
                params![uid],
                |row| row.get(0),
            )
            .map_err(|e| format!("Read profile token tags: {e}"))?;
        let held_elsewhere_by = |column: &str, tag: &str| -> Result<bool, String> {
            tx.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM server_profiles
                     WHERE {column} = ?1 AND user_id <> ?2)"
                ),
                params![tag, uid],
                |row| row.get(0),
            )
            .map_err(|e| format!("Read profile holders: {e}"))
        };
        let mut sole = Vec::new();
        let mut waits = false;
        // The vault value each moved key held when the pass read it: the
        // post-commit delete lands only while the vault still holds it.
        let mut expected: HashMap<String, Option<Zeroizing<String>>> = HashMap::new();
        for id in profiles
            .iter()
            .filter_map(|p| value_str(p, &["id", "uid", "profileUid"]))
        {
            let tag = user_crypto::metadata_tag(&root_secret, b"profile-uid", id)?;
            let id_shared = held_elsewhere_by("profile_uid", &tag)?;
            let key_shared = held_elsewhere_by("token_uid", &profile_token_tag(&root_secret, id)?)?;
            let keys = private_token_keys_of_profile(id)
                .into_iter()
                .filter(|key| only.is_none_or(|only| only.contains(key)));
            // A token key is another account's too when one of its profiles
            // names it: the same id, or, for a key that lowercases the id
            // ([`crate::providers::oauth2::oauth_token_key`]), an id that
            // differs only in case (`team` of the legacy owner, `Team` here).
            let mut shared_keys = Vec::new();
            for key in keys {
                let folds = token_key_folds_case(&key);
                if id_shared || (folds && key_shared) {
                    shared_keys.push(key);
                    continue;
                }
                // Such a key waits alone, vault key and row as they are, and
                // the account stays unmarked, so a later pass runs it once the
                // tags are there: an account locked behind its passphrase
                // keeps its tags unfilled, and holds up no other key.
                if folds && token_tags_pending && token_key_present(&tx, uid, vault_keys, &key)? {
                    waits = true;
                    continue;
                }
                if vault_keys.contains(&key) {
                    sole.push(key);
                }
            }
            // A token key another account's profile also names: the vault key
            // is the legacy owner's, and a row of this account is a copy of it,
            // which an earlier migration made and a refresh may have rewritten
            // since, unless its own sign-in stamped it. A native Proton session
            // is the same session by its UID, whatever pair a refresh has
            // rotated it to since. A copy outlives its vault key when the owner
            // signed out first: an unstamped row of such a key is dropped all
            // the same, but a Proton session, whose owner's sign-out ended it.
            for key in shared_keys {
                let Some((row, digest)) = read_credential_and_digest_with_dek(&tx, uid, dek, &key)?
                else {
                    continue;
                };
                let vault = read_secret(&key)?;
                if row_may_be_a_copy(
                    &tx,
                    uid,
                    &key,
                    &row,
                    &digest,
                    vault.as_deref().map(|v| v.as_str()),
                )? {
                    delete_user_credential_for(&tx, uid, &key)?;
                }
            }
        }
        let unbound = format!("{PROTON_UNBOUND_USER_PREFIX}{uid}");
        if only.is_none() && vault_keys.contains(&unbound) {
            sole.push(unbound);
        }
        // The sessions other accounts hold in the vault, by UID.
        let mut held_elsewhere = HashSet::new();
        for key in vault_keys
            .iter()
            .filter(|key| is_proton_session_key(key) && !sole.contains(key))
        {
            if let Some(held) = read_secret(key)?.and_then(|raw| proton_session_uid(&raw)) {
                held_elsewhere.insert(held);
            }
        }
        let mut inserted = read_inserted_row_digests(&tx, uid)?;
        let mut skipped = Vec::new();
        for key in &sole {
            let vault_now = read_secret(key)?;
            expected.insert(
                key.clone(),
                vault_now
                    .as_ref()
                    .map(|value| Zeroizing::new((**value).clone())),
            );
            let kind = private_token_type(key).unwrap_or("oauth");
            if is_proton_session_key(key) {
                let copy_uid = match read_credential_with_dek(&tx, uid, dek, key)? {
                    Some(row) => proton_session_uid(&row),
                    None => read_secret(key)?.and_then(|raw| proton_session_uid(&raw)),
                };
                if copy_uid.is_some_and(|copy| held_elsewhere.contains(&copy)) {
                    delete_user_credential_for(&tx, uid, key)?;
                    inserted.remove(key);
                    continue;
                }
            }
            let vault_str = vault_now.as_ref().map(|value| value.as_str());
            match sole_row_state(&tx, uid, dek, key, kind, vault_str, &mut inserted)? {
                SoleRow::Moved => {}
                SoleRow::Skipped => skipped.push(key.clone()),
            }
        }
        if !skipped.is_empty() {
            sole.retain(|key| !skipped.contains(key));
            for key in &skipped {
                expected.remove(key);
            }
        }
        if only.is_none() {
            let held: HashSet<&str> = sole.iter().map(String::as_str).collect();
            inserted.retain(|key, _| held.contains(key.as_str()));
        }
        write_inserted_row_digests(&tx, uid, &inserted)?;
        tx.commit()
            .map_err(|e| format!("Commit private token alignment: {e}"))?;
        Ok(Some((sole, waits, expected)))
    });
    let (moved, waits, expected) = match outcome {
        Ok(Some(done)) => done,
        Ok(None) => return Ok(None),
        Err(e) if e == "USER_LOCKED" => return Ok(None),
        Err(e) => return Err(e),
    };
    // The copies are committed: only now does the vault let go of them. A
    // value another writer stored since the pass read it stays, and the
    // account stays unmarked so the next pass reconciles that newer chain.
    let mut changed = false;
    if vault_lets_go {
        for key in &moved {
            let held = expected
                .get(key)
                .and_then(|value| value.as_ref().map(|raw| raw.as_str()));
            if !delete_secret(key, held)? {
                changed = true;
            }
        }
    }
    // A pass that left them in the vault is not done: the next one that can
    // exclude the profile writers removes them. Nor is one with a key that
    // waits for token tags: the next pass runs it. A targeted pass marks
    // nothing, so it reports itself incomplete, and its caller keeps the keys
    // for that pass ([`realign_deferred_tokens`], [`defer_realign`]).
    let incomplete = waits || changed || (!vault_lets_go && !moved.is_empty());
    if incomplete && only.is_some() {
        return Ok(None);
    }
    if only.is_none() && !incomplete {
        write_inserted_row_digests(conn, user_id, &HashMap::new())?;
        mark_private_tokens_aligned(conn, user_id)?;
    }
    Ok(Some(moved.len()))
}

/// Whether `row`, the row `key` of `user_id`, may be a copy of `vault`, the
/// vault's value under that key: the one rule by which the alignment drops a
/// copy on a profile id another account holds, and by which the readers hide
/// a row while the alignment waits ([`unverified_while_tags_pending`]). A
/// native Proton session is the same session by its UID, whatever pair a
/// refresh has rotated it to since; any other row is a copy unless its own
/// sign-in stamped it ([`stamp_matches`], against `digest`, the digest of
/// the ciphertext `row` was decrypted from). With no vault value the
/// owner signed out first, and a copy outlives its key: any unstamped row is
/// a copy all the same, but a Proton session, whose owner's sign-out ended
/// it.
fn row_may_be_a_copy(
    conn: &Connection,
    user_id: i64,
    key: &str,
    row: &str,
    digest: &str,
    vault: Option<&str>,
) -> Result<bool, String> {
    let Some(vault) = vault else {
        return Ok(!is_proton_session_key(key) && !stamp_matches(conn, user_id, key, digest)?);
    };
    if is_proton_session_key(key) {
        return Ok(proton_session_uid(row)
            .is_some_and(|copy| proton_session_uid(vault).is_some_and(|held| held == copy)));
    }
    // Only the sign-in stamp proves a row is this account's own: a changed
    // Jottacloud username or refresh token cannot, since a copied chain an
    // earlier migration made drifts in both once it is refreshed, and an
    // ambiguous legacy row signs in again instead.
    Ok(!stamp_matches(conn, user_id, key, digest)?)
}

/// Whether a profile of another account still misses its token tag: which
/// token keys its profiles name cannot be read until they are filled, which
/// matters for a key that folds case ([`token_key_folds_case`]), where two
/// profile ids that differ only in case name the same vault key.
fn token_tags_pending_elsewhere(conn: &Connection, user_id: i64) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM server_profiles
         WHERE user_id <> ?1 AND token_uid IS NULL)",
        params![user_id],
        |row| row.get(0),
    )
    .map_err(|e| format!("Read profile token tags: {e}"))
}

thread_local! {
    /// This thread holds the profile-list lock and has not written the
    /// restore yet. The plan read must see the rows a preview sees.
    /// Off again before the partition is replaced: later reads on this
    /// thread still withhold unstamped copies until realign.
    static LIST_LOCK_HELD_BEFORE_RESTORE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Marks this thread's profile-list lock as held for a plan read that runs
/// before the restore writes ([`profile_list_lock_busy`] then answers
/// false): the import takes the lock and only then recomputes the plan the
/// preview fingerprinted, and the rows must read the same. The guard's
/// drop ends the mark before the partition is replaced.
pub(crate) struct PlanReadsBeforeRestore(());

impl Drop for PlanReadsBeforeRestore {
    fn drop(&mut self) {
        LIST_LOCK_HELD_BEFORE_RESTORE.with(|held| held.set(false));
    }
}

pub(crate) fn plan_reads_before_restore() -> PlanReadsBeforeRestore {
    LIST_LOCK_HELD_BEFORE_RESTORE.with(|held| held.set(true));
    PlanReadsBeforeRestore(())
}

/// Whether a writer holds the [`ProfileListLock`] of `conn`'s database right
/// now, asked without waiting: a restore or import keeps it for the whole
/// write, during which the rows it lands may still be copies the alignment
/// has not reached. An in-memory database, a missing lock file or a
/// filesystem without file locks answers false.
fn profile_list_lock_busy(conn: &Connection) -> bool {
    if LIST_LOCK_HELD_BEFORE_RESTORE.with(|held| held.get()) {
        return false;
    }
    let Some(path) = conn.path() else {
        return false;
    };
    if path.is_empty() {
        return false;
    }
    let db_path = std::path::Path::new(path);
    let parent = db_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let resolved = std::fs::canonicalize(db_path).or_else(|_| {
        std::fs::canonicalize(parent).map(|dir| dir.join(db_path.file_name().unwrap_or_default()))
    });
    let Ok(resolved) = resolved else {
        return false;
    };
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(resolved.with_extension("write-lock"))
    else {
        return false;
    };
    #[cfg(test)]
    if NO_FILE_LOCKS_IN_TEST.get() {
        return false;
    }
    matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

/// Whether the private-token row `key` of `user_id`, read as `row`, must read
/// as absent: the account is not the legacy owner and not yet aligned, and
/// the alignment waits for profile tags ([`waits_for_profile_tags`], or, for
/// a key that folds case, [`token_tags_pending_elsewhere`]), so nothing yet
/// tells whether another account holds the profile, while the row may be a
/// copy of the vault's value ([`row_may_be_a_copy`]). A copy an earlier
/// migration made of the legacy owner's token would otherwise sign this
/// account in to the owner's cloud until an unrelated account is unlocked.
/// A row the account's own sign-in stamped stays readable, and so does every
/// row of an aligned account. The same holds while a writer keeps the
/// profile list lock: a restore lands rows before its realignment runs, with
/// the tags already current, so an unaligned non-owner row is just as
/// unverified then.
fn unverified_while_tags_pending(
    conn: &Connection,
    user_id: i64,
    key: &str,
    row: &str,
    digest: &str,
    read_secret: &SecretReader<'_>,
) -> Result<bool, String> {
    if private_token_type(key).is_none() || private_tokens_aligned(conn, user_id)? {
        return Ok(false);
    }
    // A key that folds case is ambiguous while another account's token tags
    // are pending even without a restore's wait: `Team` here and `team`
    // there are one vault key, and nothing yet says which profile's token
    // the vault holds.
    let waits = waits_for_profile_tags(conn, user_id)?
        || (token_key_folds_case(key) && token_tags_pending_elsewhere(conn, user_id)?)
        || (profile_list_lock_busy(conn)
            && nonprofile_secret_owner_unpersisted(conn)? != Some(user_id));
    if !waits {
        return Ok(false);
    }
    let vault = read_secret(key)?;
    row_may_be_a_copy(
        conn,
        user_id,
        key,
        row,
        digest,
        vault.as_deref().map(|v| v.as_str()),
    )
}

/// `row`, just read as the row `key` of `user_id` with the digest of its
/// ciphertext, as a reader may use it: `None` while it is unverified
/// ([`unverified_while_tags_pending`]). Every reader of a row for use goes
/// through this, so a copy cannot reach a connection, a backup, a duplicate
/// or a relocation either.
fn checked_row(
    conn: &Connection,
    user_id: i64,
    key: &str,
    row: Option<RowRead>,
    read_secret: &SecretReader<'_>,
) -> Result<Option<Zeroizing<String>>, String> {
    match row {
        Some((value, digest))
            if unverified_while_tags_pending(conn, user_id, key, &value, &digest, read_secret)? =>
        {
            Ok(None)
        }
        other => Ok(other.map(|(value, _)| value)),
    }
}

/// The row `key` of `user_id` as a reader may use it ([`read_checked_row`]).
pub(crate) enum CheckedRow {
    /// No row.
    Absent,
    /// A row that may be another account's copy, which reads as absent and
    /// whose vault key is that account's too ([`checked_row`]).
    Withheld,
    Value(Zeroizing<String>),
}

impl CheckedRow {
    pub(crate) fn value(self) -> Option<Zeroizing<String>> {
        match self {
            CheckedRow::Value(value) => Some(value),
            CheckedRow::Absent | CheckedRow::Withheld => None,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Runs once on this thread in the next [`read_checked_row_with`], after
    /// its row is decrypted and before the row is checked: another writer
    /// rewriting the row in between.
    pub(crate) static AFTER_ROW_READ: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

/// The row `key` of `user_id`, decrypted and checked ([`checked_row`]) on the
/// bytes one read returned, against the alignment state of the same snapshot:
/// one read transaction holds the row, the pending tags and the aligned
/// marker, so an alignment that drops the row and marks the account after
/// the read cannot vouch for the bytes it dropped. A caller already inside a
/// transaction reads in that one.
fn read_checked_row_with(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    key: &str,
    read_secret: &SecretReader<'_>,
) -> Result<CheckedRow, String> {
    let snapshot = conn
        .is_autocommit()
        .then(|| {
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Deferred)
        })
        .transpose()
        .map_err(|e| format!("Begin credential read: {e}"))?;
    let row = get_user_credential_and_digest_for(conn, root_key, user_id, key)?;
    #[cfg(test)]
    run_dual_write_hook(&AFTER_ROW_READ);
    let checked = if row.is_none() {
        CheckedRow::Absent
    } else {
        match checked_row(conn, user_id, key, row, read_secret)? {
            Some(value) => CheckedRow::Value(value),
            None => CheckedRow::Withheld,
        }
    };
    if let Some(snapshot) = snapshot {
        snapshot
            .commit()
            .map_err(|e| format!("End credential read: {e}"))?;
    }
    Ok(checked)
}

/// [`read_checked_row_with`] for a reader outside this module, with the vault
/// of `store`. A locked passphrase account is `Err("USER_LOCKED")`, as
/// [`get_user_credential_for`] gives it.
pub(crate) fn read_checked_row(
    conn: &Connection,
    store: &CredentialStore,
    root_key: &[u8; 32],
    user_id: i64,
    key: &str,
) -> Result<CheckedRow, String> {
    read_checked_row_with(conn, root_key, user_id, key, &|key: &str| {
        get_optional_secret(store, key)
    })
}

/// The row `key` of `user_id` in a backup's partition `conn`, when it is that
/// account's own to restore or copy elsewhere: any key that is not a private
/// token, the backup's legacy owner, an account the backup had aligned, or a
/// row that is no copy of `vault_after_import`, the value the vault holds
/// under `key` once the import has run ([`row_may_be_a_copy`]). A backup made
/// before tokens were kept to their account can carry, in another account's
/// row, the copy a migration made of the owner's token; such a row is
/// `None`. The restore readers and the alignment still see it once the
/// partition is restored, but a decision never copies it under another key.
pub(crate) fn backup_row_if_own(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    key: &str,
    vault_after_import: Option<&str>,
) -> Result<Option<Zeroizing<String>>, String> {
    let Some((row, digest)) = get_user_credential_and_digest_for(conn, root_key, user_id, key)?
    else {
        return Ok(None);
    };
    let own = private_token_type(key).is_none()
        || is_legacy_owner(conn, user_id)?
        || private_tokens_aligned(conn, user_id)?
        || !row_may_be_a_copy(conn, user_id, key, &row, &digest, vault_after_import)?;
    Ok(own.then_some(row))
}

/// The account active before a keystore restore, and what it held then, for
/// [`realign_private_tokens_after_restore`].
pub struct ActiveBeforeRestore<'a> {
    pub store: &'a CredentialStore,
    pub user_id: i64,
    /// The ids of the profiles it listed before the import wrote anything.
    pub profile_ids: &'a [String],
    /// The private token keys the import wrote into the vault.
    pub restored_keys: &'a [String],
}

/// After a keystore restore wrote private token keys into the vault: a backup
/// made before tokens were kept to their account carries another account's
/// under the key the legacy owner reads, so the alignment runs again for every
/// account but the one active after the import. That one is not reset: the
/// restore may have filled its list with the backup's profiles, whose vault
/// keys are the backup's, and the import gives it those only where they are
/// its own ([`vault_copy_is_another_accounts`]). Its own tokens are not left
/// behind either: when it is the account that was active before the import
/// (`active`), the alignment's rule runs once over the restored keys of the
/// profiles it listed then, the ones a vault key can be its own for, so its
/// token comes back to its row where it alone holds the profile.
pub fn realign_private_tokens_after_restore(
    db_path: &std::path::Path,
    active_before: Option<ActiveBeforeRestore<'_>>,
) -> Result<usize, String> {
    let conn = open_restored_db(db_path)?;
    let active = active_user_id(&conn)?;
    for user in list_users(&conn)? {
        if Some(user.id) == active {
            continue;
        }
        conn.execute(
            "DELETE FROM global_state WHERE key = ?1",
            params![format!("{PRIVATE_TOKENS_ALIGNED_PREFIX}{}", user.id)],
        )
        .map_err(|e| format!("Reset private token marker: {e}"))?;
    }
    let Some(before) = active_before.filter(|before| Some(before.user_id) == active) else {
        return Ok(0);
    };
    let held: Vec<String> = before
        .profile_ids
        .iter()
        .flat_map(|id| private_token_keys_of_profile(id))
        .filter(|key| before.restored_keys.contains(key))
        .collect();
    if held.is_empty() {
        return Ok(0);
    }
    let vault_keys = before
        .store
        .list_accounts()
        .map_err(|e| format!("List vault keys: {e}"))?;
    let read_secret = |key: &str| get_optional_secret(before.store, key);
    let delete_secret = |key: &str, expected: Option<&str>| -> Result<bool, String> {
        match expected {
            Some(expected) => before
                .store
                .delete_secret_if_unchanged(key, expected)
                .map_err(|e| format!("Delete legacy secret: {e}")),
            None => match get_optional_secret(before.store, key)? {
                Some(_) => Ok(false),
                None => Ok(true),
            },
        }
    };
    let root_key = Zeroizing::new(before.store.derive_user_partition_wrapping_key());
    match align_private_tokens_of(
        &conn,
        &root_key,
        before.user_id,
        &vault_keys,
        Some(&held),
        &read_secret,
        &delete_secret,
    )? {
        Some(aligned) => Ok(aligned),
        // Not run now: the account's next ordinary pass runs it over the
        // same keys, which its aligned marker would otherwise skip.
        None => {
            defer_realign(&conn, &root_key, before.user_id, &held)?;
            Ok(0)
        }
    }
}

/// Reader-fallback net (sec 3.3), engine form: prefer the per-user encrypted
/// store, falling back to the legacy vault only when its per-user row is absent.
/// A locked partition refuses access; MUV-6 drops the fallback once purged.
fn read_credential_with_fallback_inner(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
    read_secret: &SecretReader<'_>,
) -> Result<Option<Zeroizing<String>>, String> {
    if let Some(value) =
        read_checked_row_with(conn, root_key, user_id, credential_id, read_secret)?.value()
    {
        return Ok(Some(value));
    }
    // Another account's private token lives in its partition alone: the vault
    // key of that name is the legacy owner's ([`private_token_user`]).
    if vault_copy_is_another_accounts(conn, user_id, credential_id)? {
        return Ok(None);
    }
    read_secret(credential_id)
}

/// True if any user can be migrated eagerly right now (device-wrapped, or a
/// passphrase user already in session) and is not yet migrated. Cheap pre-check
/// so the per-command boot path skips the vault read once everything reachable
/// is done; a locked passphrase user is intentionally NOT "eager-pending".
fn has_eager_pending_users(conn: &Connection) -> Result<bool, String> {
    for user in list_users(conn)? {
        if credentials_settled(conn, user.id)? || waits_for_profile_tags(conn, user.id)? {
            continue;
        }
        if !user.has_passphrase || session_user_id() == Some(user.id) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Eager migration pass over every user whose DEK is reachable now. Best-effort
/// per user: one account's failure leaves it unmarked (retried next boot) and
/// never aborts the others. Reads the vault only when work remains.
pub fn migrate_credentials_eager_all(
    conn: &Connection,
    store: &CredentialStore,
    root_key: &[u8; 32],
) -> Result<usize, String> {
    let users = list_users(conn)?;
    // The token tags the alignment reads ([`profile_token_tag`]), for every
    // list this key opens, before any account is aligned.
    for user in &users {
        if !user.has_passphrase || session_user_id() == Some(user.id) {
            if let Err(e) = fill_profile_token_tags(conn, root_key, user.id) {
                tracing::warn!("Profile token tags of account {} not written: {e}", user.id);
            }
        }
    }
    let mut pending = Vec::new();
    for user in &users {
        if credentials_settled(conn, user.id)? || waits_for_profile_tags(conn, user.id)? {
            continue;
        }
        if !user.has_passphrase || session_user_id() == Some(user.id) {
            pending.push(user.id);
        }
    }
    if pending.is_empty() {
        return Ok(0);
    }
    let vault_keys = list_vault_keys(store)?;
    let read_secret = |key: &str| get_optional_secret(store, key);
    let delete_secret = |key: &str, expected: Option<&str>| -> Result<bool, String> {
        match expected {
            Some(expected) => store
                .delete_secret_if_unchanged(key, expected)
                .map_err(|e| format!("Delete legacy secret: {e}")),
            None => match get_optional_secret(store, key)? {
                Some(_) => Ok(false),
                None => Ok(true),
            },
        }
    };
    let list_secrets = || list_vault_keys(store);
    let mut total = 0usize;
    for user_id in pending {
        match migrate_user_credentials_inner(conn, root_key, user_id, &vault_keys, &read_secret) {
            Ok(migrated) => total += migrated,
            // Leave unmarked; a transient failure is retried on the next boot.
            Err(_) => continue,
        }
        // After the copy, which reads the same vault keys.
        let _ = align_private_tokens_inner(
            conn,
            root_key,
            user_id,
            &vault_keys,
            &list_secrets,
            &read_secret,
            &delete_secret,
        );
    }
    Ok(total)
}

/// Lazy migration for one just-unlocked user. Called from the unlock bridges
/// where the session DEK is primed and the store is available.
pub fn migrate_credentials_for_user(
    conn: &Connection,
    store: &CredentialStore,
    root_key: &[u8; 32],
    user_id: i64,
) -> Result<usize, String> {
    retag_own_profiles_if_pending(conn, root_key, user_id)?;
    if let Err(e) = fill_profile_token_tags(conn, root_key, user_id) {
        tracing::warn!("Profile token tags of account {user_id} not written: {e}");
    }
    if credentials_settled(conn, user_id)? {
        return Ok(0);
    }
    let vault_keys = list_vault_keys(store)?;
    let read_secret = |key: &str| get_optional_secret(store, key);
    let delete_secret = |key: &str, expected: Option<&str>| -> Result<bool, String> {
        match expected {
            Some(expected) => store
                .delete_secret_if_unchanged(key, expected)
                .map_err(|e| format!("Delete legacy secret: {e}")),
            None => match get_optional_secret(store, key)? {
                Some(_) => Ok(false),
                None => Ok(true),
            },
        }
    };
    let list_secrets = || list_vault_keys(store);
    let migrated =
        migrate_user_credentials_inner(conn, root_key, user_id, &vault_keys, &read_secret)?;
    align_private_tokens_inner(
        conn,
        root_key,
        user_id,
        &vault_keys,
        &list_secrets,
        &read_secret,
        &delete_secret,
    )?;
    Ok(migrated)
}

/// Reader-fallback net (sec 3.3): store-backed wrapper of
/// [`read_credential_with_fallback_inner`].
pub fn read_credential_with_fallback(
    conn: &Connection,
    store: &CredentialStore,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    let read_secret = |key: &str| get_optional_secret(store, key);
    read_credential_with_fallback_inner(conn, root_key, user_id, credential_id, &read_secret)
}

// --- MUV-3: live cutover of `server_*` reader/writer call-sites -------------
//
// MUV-3 points the real `server_*` credential resolution at the per-user store
// while keeping the legacy vault as source of truth + fallback during the
// rollout (dual-write). Readers prefer the partition row and fall back to the
// vault only for a not-yet-migrated key; a locked partition refuses access.
// writers always write the vault first, then best-effort mirror the secret into
// the active (GUI) or scoped (CLI `--user`) user's partition. No vault entry is
// ever removed here (that is MUV-6). The mirror is confined to in-scope keys by
// `muv3_credential_type` (`server_*`, and `ai_apikey_*` once MUV-5 lands), so the
// generic `store_credential` / `delete_credential` Tauri commands mirror those by
// prefix while OAuth and GitHub tokens are mirrored by their own type-explicit
// call-sites (MUV-4/5) and never by prefix. The
// partition DB is opened without an AppHandle (`open_or_init_cli`), which
// resolves to the same file as the GUI (see `cli_db_path` / `db_path`), so a
// shared reader in factory/agent/ai_tools sees exactly what the GUI wrote.

/// Prefix scope classifier: `Some(credential_type)` for the keys whose namespace
/// is unambiguous enough that the generic `store_credential`/`delete_credential`
/// Tauri commands can mirror them by prefix alone. `server_*` (MUV-3) and
/// `ai_apikey_*` (MUV-5) qualify: an AI key is always `ai_apikey_<provider>` with
/// no `_client_id`-style sibling, so prefix matching never misfires. The four
/// per-profile overlay secret prefixes also mirror by their unambiguous keys,
/// so saving a password/keyfile in the GUI repairs a missing owned row instead
/// of leaving an ambiguous global copy. Everything else is `None`: OAuth tokens
/// (`oauth_<p>_<id>` vs `oauth_<p>_client_id`) and
/// the GitHub tokens are mirrored by call-sites that pass the type explicitly
/// (MUV-4/5), and the machine-global `github_pem_*` / `github_app_credentials`
/// stay vault-only on purpose (MUV-0).
fn muv3_credential_type(credential_id: &str) -> Option<&'static str> {
    if credential_id.starts_with("server_") {
        Some("server")
    } else if credential_id.starts_with("ai_apikey_") {
        Some("ai_apikey")
    } else if [
        "aerocrypt_overlay_pw_",
        "aerocrypt_overlay_salt_",
        "aerocrypt_overlay_config_",
        "aerocrypt_overlay_keyfile_path_",
    ]
    .iter()
    .any(|prefix| credential_id.starts_with(prefix))
    {
        Some("crypt")
    } else {
        None
    }
}

/// Remove `credential_id` from `user_id`'s partition, reporting a failure:
/// a sign-out must not go on while a partition copy it could not remove stays
/// readable (`unmirror_credential_for_user_best_effort_any` drops the error).
pub(crate) fn try_unmirror_active_credential(
    user_id: i64,
    credential_id: &str,
) -> Result<(), String> {
    delete_user_credential_for(&open_or_init_cli()?, user_id, credential_id)
}

/// The active account for a write or a delete that readers must see: a lookup
/// that fails is an error, since a partition row may be holding the old value.
/// So is a store that cannot be opened or read, an active value that is not
/// an id, and no active account while accounts exist (no writer leaves that
/// state: the one that creates the first account activates it). `Ok(None)`
/// is a confirmed absence: no account at all.
pub(crate) fn active_user_id_for_write() -> Result<Option<i64>, String> {
    #[cfg(test)]
    if ACCOUNT_LOOKUP_FAILS.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("Active account lookup: refused".to_string());
    }
    let conn = open_or_init_cli()?;
    if let Some(user) = active_user_id(&conn)? {
        return Ok(Some(user));
    }
    let accounts: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
        .map_err(|e| format!("Read accounts: {e}"))?;
    if accounts == 0 {
        Ok(None)
    } else {
        Err("Active account lookup: accounts exist but none is active".to_string())
    }
}

/// The one account a private token's sign-in is saved for, among the `owners`
/// holding its profile: the active account when it holds it, or the single
/// holder. Writing it to every holder would hand one account's token to the
/// others, so several holders and none of them active is refused.
fn private_token_signer(owners: Vec<i64>, active: Option<i64>) -> Result<Vec<i64>, String> {
    match active.filter(|user| owners.contains(user)) {
        Some(active) => Ok(vec![active]),
        None if owners.len() == 1 => Ok(owners),
        None => Err(format!(
            "{ACCOUNT_CHANGED}: this profile belongs to other accounts, so the sign-in was not saved"
        )),
    }
}

/// The active account cannot be read (an unopenable partition DB), on every
/// thread: a connection reads it on the runtime's. Set only inside an
/// isolated child (`isolated_account_fixture::in_isolated_child`), whose
/// process runs that one test.
#[cfg(test)]
pub(crate) static ACCOUNT_LOOKUP_FAILS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Save a secret of `profile_id` for the accounts that hold that profile, whichever
/// is active: another account's token must not land in the active one's partition.
/// Every owner is tried, and the first failure is returned; a private token goes
/// to one of them only. A profile no account holds yet goes to the active account.
pub(crate) fn store_profile_credential(
    store: &CredentialStore,
    profile_id: &str,
    credential_id: &str,
    credential_type: Option<&str>,
    secret: &str,
) -> Result<(), String> {
    // Under the list lock: a relocation between the lookup and the writes would leave
    // the new owner's row on the old value.
    let conn = open_or_init_cli()?;
    let _lock = ProfileListLock::acquire(&conn)?;
    let owners = if profile_id.is_empty() {
        Vec::new()
    } else {
        profile_credential_owner_ids(store, profile_id, credential_id)?
    };
    if owners.is_empty() {
        return match credential_type {
            Some(kind) => {
                store_active_signed_in_credential_typed_dual(store, credential_id, kind, secret)
            }
            None => store_active_credential_dual(store, credential_id, secret),
        };
    }
    let owners = match private_token_type(credential_id) {
        None => owners,
        Some(_) => private_token_signer(owners, active_user_id(&conn)?)?,
    };
    let mut first_failure = None;
    for owner in owners {
        let written = match credential_type {
            Some(kind) => store_signed_in_credential_for_user_typed_dual(
                store,
                owner,
                credential_id,
                kind,
                secret,
            ),
            None => store_credential_for_user_dual(store, owner, credential_id, secret),
        };
        if let Err(e) = written {
            first_failure.get_or_insert(e);
        }
    }
    first_failure.map_or(Ok(()), Err)
}

/// Remove a secret of `profile_id` from the vault and from the accounts that hold it,
/// a private token from the active account only. A failure is an error: a row left
/// behind would keep answering readers.
pub(crate) fn delete_profile_credential(
    store: &CredentialStore,
    profile_id: &str,
    credential_id: &str,
) -> Result<(), String> {
    let conn = open_or_init_cli()?;
    let _lock = ProfileListLock::acquire(&conn)?;
    let owners = if profile_id.is_empty() {
        Vec::new()
    } else {
        profile_credential_owner_ids(store, profile_id, credential_id)?
    };
    let active = active_user_id(&conn)?;
    // Only the account holding the profile may sign it out.
    if !owners.is_empty() && !active.is_some_and(|user| owners.contains(&user)) {
        return Err(format!(
            "{ACCOUNT_CHANGED}: this profile belongs to another account, so it was not signed out"
        ));
    }
    // A private token is one account's sign-in: the active account signs out
    // its own, and another account holding the profile keeps its token.
    let mut users = if private_token_type(credential_id).is_some() {
        Vec::new()
    } else {
        owners
    };
    if let Some(active) = active {
        if !users.contains(&active) {
            users.push(active);
        }
    }
    // The vault copy goes only when one of these accounts reads it: a private
    // token of the others is their row alone, and the vault key of that name
    // is the legacy owner's ([`vault_copy_is_another_accounts`]).
    let mut vault_is_theirs = users.is_empty();
    for user in &users {
        if !vault_copy_is_another_accounts(&conn, *user, credential_id)? {
            vault_is_theirs = true;
        }
    }
    // The rows first, as readers prefer them: a failure here leaves the vault for a
    // retry, and a retry after a vault failure finds the rows already gone.
    for user in users {
        delete_user_credential_for(&conn, user, credential_id)?;
    }
    if !vault_is_theirs {
        return Ok(());
    }
    match store.delete(credential_id) {
        Ok(_) | Err(crate::credential_store::CredentialError::NotFound(_)) => Ok(()),
        Err(e) => Err(format!("Delete credential from vault: {e}")),
    }
}

/// Mirror one secret, just written to the vault, into `user_id`'s partition: the
/// keys the prefix classifier knows, and any key whose row already exists (a
/// relocation leaves one), since readers prefer that row. Zero-password safe (a
/// device-wrapped account mirrors via the keyring-derived root_key, no prompt).
fn mirror_credential_for_user(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    secret: &str,
) -> Result<(), String> {
    let conn = open_or_init_cli()?;
    let Some(kind) = partition_credential_type(&conn, user_id, credential_id, None)? else {
        return Ok(());
    };
    mirror_into_partition(&conn, store, user_id, credential_id, &kind, secret)
}

/// Mirror with an EXPLICIT credential type. Used by call-sites that already know
/// the type (OAuth tokens, Jottacloud refresh) and therefore bypass the prefix
/// classifier, which cannot reliably tell `oauth_<p>_<id>` (a per-user token) from
/// `oauth_<p>_client_id` (machine/app config).
pub(crate) fn mirror_credential_for_user_typed(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    // A connection that began with no account writes as the legacy owner:
    // its row follows the vault copy just written ([`legacy_only_copies`]).
    let user_id = if user_id == LEGACY_ONLY_ACCOUNT {
        match legacy_only_copies(credential_id)? {
            LegacyOnlyCopies::VaultAlone => return Ok(()),
            LegacyOnlyCopies::VaultThenOwnersRow(owner) | LegacyOnlyCopies::OwnersRow(owner) => {
                owner
            }
        }
    } else {
        user_id
    };
    let conn = open_or_init_cli()?;
    mirror_into_partition(
        &conn,
        store,
        user_id,
        credential_id,
        credential_type,
        secret,
    )
}

/// A failure here is an error, because readers prefer the partition row: it would
/// go on answering with the old secret after a save reported as done. The writers
/// that wrote the vault first put it back ([`store_vault_then_mirror`]); a rotation
/// writes its one copy itself (Jottacloud) or never goes through here. A locked legacy owner
/// with no row is the one exception: its readers fall back to the vault, and the MUV-2
/// lazy pass mirrors it at the next unlock. Any other account reads only its partition.
fn mirror_into_partition(
    conn: &Connection,
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let refused = match set_user_credential_for(
        conn,
        &root_key,
        user_id,
        credential_id,
        credential_type,
        secret,
    ) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    if refused == "USER_LOCKED"
        && !partition_row_exists(conn, user_id, credential_id)?
        && nonprofile_secret_owner(conn)? == Some(user_id)
    {
        return Ok(());
    }
    Err(format!(
        "{credential_id} reached the vault but not the account store, so the new value may not be the one in use: {refused}"
    ))
}

fn partition_row_exists(
    conn: &Connection,
    user_id: i64,
    credential_id: &str,
) -> Result<bool, String> {
    conn.query_row(
        "SELECT 1 FROM user_credentials WHERE user_id = ?1 AND credential_id = ?2",
        params![user_id, credential_id],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
    .map_err(|e| format!("Read credential row: {e}"))
}

/// Whether `user_id` holds `credential_id` where it reads it, decrypting
/// nothing: its row, which a locked account has too and a delete by key
/// removes, or else the vault copy when that is one it reads.
pub fn holds_credential(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<bool, String> {
    let conn = open_or_init_cli()?;
    if partition_row_exists(&conn, user_id, credential_id)? {
        return Ok(true);
    }
    if vault_copy_is_another_accounts(&conn, user_id, credential_id)? {
        return Ok(false);
    }
    Ok(get_optional_secret(store, credential_id)?.is_some_and(|v| !v.is_empty()))
}

/// Best-effort removal of one secret from `user_id`'s partition, whatever its
/// key. No DEK is needed (DELETE by key); errors are swallowed because the
/// vault delete is the part that matters during the dual-write rollout. This
/// is the partition half of an explicit user delete, never a mass purge (that
/// is MUV-6).
pub fn unmirror_credential_for_user_best_effort_any(user_id: i64, credential_id: &str) {
    if let Ok(conn) = open_or_init_cli() {
        let _ = delete_user_credential_for(&conn, user_id, credential_id);
    }
}

/// Active-user reader (GUI + shared modules: factory/agent/ai_tools/mcp/AI core).
/// Resolves `credential_id` for the active user from the per-user store, falling
/// back to the legacy vault when the row is absent. A locked account is refused.
/// With no active user (pre-MU / fresh install) it reads the vault only.
pub fn resolve_active_credential(
    store: &CredentialStore,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    match active_user_id(&conn)? {
        None => get_optional_secret(store, credential_id),
        Some(user_id) => read_credential_for_user(&conn, store, user_id, credential_id),
    }
}

/// [`resolve_active_credential`] for an explicit `user_id`, whichever account
/// is active: a caller bound to the account it read profiles from keeps
/// reading that account's secrets across a switch.
pub fn resolve_credential_for_user(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    if user_id == LEGACY_ONLY_ACCOUNT {
        return read_legacy_only_credential(store, credential_id);
    }
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    read_credential_for_user(&conn, store, user_id, credential_id)
}

/// Which copies of `credential_id` a connection that began with no account
/// ([`LEGACY_ONLY_ACCOUNT`]) reads and writes, by the key's type, the one
/// rule its reader ([`read_legacy_only_credential`]) and its mirror
/// ([`mirror_credential_for_user_typed`]) follow.
enum LegacyOnlyCopies {
    /// No account yet: the vault is the only copy.
    VaultAlone,
    /// A private token ([`private_token_type`]): it reaches the vault from
    /// the legacy owner alone ([`private_token_user`]), so the vault key is
    /// the owner's own copy, the one read; the owner's row follows a write.
    /// A Jottacloud rotation written there has the row follow it, and a row
    /// that did not follow is the stale one.
    VaultThenOwnersRow(i64),
    /// Any other key, which every account's save writes to the vault
    /// ([`store_vault_then_mirror`]): the legacy owner's row first, and the
    /// vault only where no other account holds a row of that key.
    OwnersRow(i64),
}

fn legacy_only_copies(credential_id: &str) -> Result<LegacyOnlyCopies, String> {
    let owner = captured_list_owner(LEGACY_ONLY_ACCOUNT)?;
    Ok(if owner == LEGACY_ONLY_ACCOUNT {
        LegacyOnlyCopies::VaultAlone
    } else if private_token_type(credential_id).is_some() {
        LegacyOnlyCopies::VaultThenOwnersRow(owner)
    } else {
        LegacyOnlyCopies::OwnersRow(owner)
    })
}

/// The secret `credential_id` for a connection that began with no account,
/// read as the legacy owner's, the account whose list that connection reads
/// ([`captured_list_owner`]), from the copy [`legacy_only_copies`] names.
fn read_legacy_only_credential(
    store: &CredentialStore,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    let owner = match legacy_only_copies(credential_id)? {
        LegacyOnlyCopies::VaultAlone | LegacyOnlyCopies::VaultThenOwnersRow(_) => {
            return get_optional_secret(store, credential_id)
        }
        LegacyOnlyCopies::OwnersRow(owner) => owner,
    };
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let read_secret = |key: &str| get_optional_secret(store, key);
    match read_checked_row_with(&conn, &root_key, owner, credential_id, &read_secret)? {
        CheckedRow::Value(value) => Ok(Some(value)),
        CheckedRow::Withheld => Ok(None),
        CheckedRow::Absent if another_account_holds_row(&conn, owner, credential_id)? => Ok(None),
        CheckedRow::Absent => read_secret(credential_id),
    }
}

/// Whether an account other than `user_id` has a row of `credential_id`,
/// decrypting nothing (a locked account's row counts).
fn another_account_holds_row(
    conn: &Connection,
    user_id: i64,
    credential_id: &str,
) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM user_credentials WHERE credential_id = ?1 AND user_id != ?2)",
        params![credential_id, user_id],
        |row| row.get(0),
    )
    .map_err(|e| format!("Read credential rows: {e}"))
}

/// Owner-aware active-user reader. Only the proven legacy owner may fall back
/// to the global vault; every other user reads exclusively from their partition.
pub fn resolve_active_credential_owner_aware(
    store: &CredentialStore,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    match active_user_id(&conn)? {
        None => get_optional_secret(store, credential_id),
        Some(user_id) => read_credential_for_user_owner_aware(&conn, store, user_id, credential_id),
    }
}

/// [`resolve_active_credential_owner_aware`] for an explicit `user_id`.
pub fn resolve_credential_for_user_owner_aware(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    if user_id == LEGACY_ONLY_ACCOUNT {
        // The legacy owner is the one account the vault fallback is for.
        return read_legacy_only_credential(store, credential_id);
    }
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    read_credential_for_user_owner_aware(&conn, store, user_id, credential_id)
}

/// What [`resolve_credential_for_user_owner_aware`] (`user`, or
/// [`resolve_active_credential_owner_aware`] for `None`) reads, telling a
/// withheld row ([`CheckedRow::Withheld`], which those read as nothing) from
/// no row: a writer that finds the key empty must know whether a row is
/// there all the same.
pub(crate) fn resolve_owner_aware_checked(
    store: &CredentialStore,
    user: Option<i64>,
    credential_id: &str,
) -> Result<CheckedRow, String> {
    let checked = |value: Option<Zeroizing<String>>| match value {
        Some(value) => CheckedRow::Value(value),
        None => CheckedRow::Absent,
    };
    if user == Some(LEGACY_ONLY_ACCOUNT) {
        return read_legacy_only_credential(store, credential_id).map(checked);
    }
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let user_id = match user {
        Some(user_id) => user_id,
        None => match active_user_id(&conn)? {
            Some(user_id) => user_id,
            None => return get_optional_secret(store, credential_id).map(checked),
        },
    };
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let read_secret = |key: &str| get_optional_secret(store, key);
    if nonprofile_secret_owner(&conn)? == Some(user_id) {
        read_credential_with_fallback_inner(&conn, &root_key, user_id, credential_id, &read_secret)
            .map(checked)
    } else {
        read_checked_row_with(&conn, &root_key, user_id, credential_id, &read_secret)
    }
}

/// Owners of a profile-bound credential, including a profile whose first
/// mirror has not been written yet. Refresh propagation must never attribute
/// another profile's secret to whichever user happens to be active now.
pub(crate) fn profile_credential_owner_ids(
    store: &CredentialStore,
    profile_id: &str,
    credential_id: &str,
) -> Result<Vec<i64>, String> {
    let conn = open_or_init_cli()?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let profile_uid = user_crypto::metadata_tag(&root_secret, b"profile-uid", profile_id)?;
    let mut statement = conn
        .prepare(
            "SELECT user_id FROM server_profiles WHERE profile_uid = ?1
             UNION SELECT user_id FROM user_credentials WHERE credential_id = ?2",
        )
        .map_err(|e| format!("Prepare credential owners: {e}"))?;
    let rows = statement
        .query_map(params![profile_uid, credential_id], |row| row.get(0))
        .map_err(|e| format!("Read credential owners: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<i64>>>()
        .map_err(|e| format!("Read credential owner row: {e}"))
}

/// Whether an account other than `user` may hold the native Proton session
/// `uid`: one of its rows holds that session, or it has a saved session this
/// process cannot read (an account locked by its passphrase), which may be
/// that one. A row that holds no session is no copy. Available without the
/// `proton-native` feature too: the shared-import restore withholds a
/// carried session on the same rule.
pub(crate) fn another_account_may_hold_session(
    store: &CredentialStore,
    user: Option<i64>,
    uid: &str,
) -> Result<bool, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let rows = conn
        .prepare(&format!(
            "SELECT user_id, credential_id FROM user_credentials
             WHERE (?1 IS NULL OR user_id <> ?1) AND {}
             ORDER BY user_id, credential_id",
            proton_session_key_sql("credential_id")
        ))
        .and_then(|mut statement| {
            statement
                .query_map(params![user], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|e| format!("Read saved sessions: {e}"))?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    for (other, key) in rows.iter().filter(|(_, key)| is_proton_session_key(key)) {
        match with_user_dek(&conn, &root_secret, *other, |id, dek| {
            read_credential_with_dek(&conn, id, dek, key)
        }) {
            Ok(None) => {}
            Ok(Some(raw)) if proton_session_uid(&raw).as_deref() != Some(uid) => {}
            _ => return Ok(true),
        }
    }
    Ok(false)
}

/// The credential type of a native Proton session row.
#[cfg(feature = "proton-native")]
const PROTON_SESSION_TYPE: &str = "proton_native_session";

/// The key of the native Proton session index: derived from the vault key, so
/// that every account on this device computes the same fingerprint of a
/// session without the key of any account's partition, and a copy of the
/// partition database without the vault cannot confirm a guessed UID.
#[cfg(feature = "proton-native")]
fn session_index_key(root_key: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    let mut key = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<Sha256>::new(None, root_key)
        .expand(b"aeroftp-proton-session-index-v1", key.as_mut())
        .expect("HKDF expand");
    key
}

#[cfg(feature = "proton-native")]
fn session_index_mac(key: &[u8; 32], message: &[u8]) -> String {
    use hmac::Mac;
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(message);
    hex::encode(mac.finalize().into_bytes())
}

/// The fingerprint the index keeps of a session UID: an HMAC under
/// [`session_index_key`], never the UID or a hash anyone could recompute.
#[cfg(feature = "proton-native")]
fn session_fingerprint(key: &[u8; 32], uid: &str) -> String {
    session_index_mac(key, uid.as_bytes())
}

/// Which index key an entry was made with: an entry carried to another
/// device (a backup restore) was made with another vault's key, and its
/// fingerprint compares to nothing here.
#[cfg(feature = "proton-native")]
fn session_index_key_tag(key: &[u8; 32]) -> String {
    session_index_mac(key, b"aeroftp proton session index key tag")[..16].to_string()
}

/// Which ciphertext of a credential row an entry describes (a session index
/// entry, a sign-in stamp): a row rewritten by any writer that did not write
/// the entry again no longer matches it.
fn credential_blob_digest(blob: &[u8], nonce: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(nonce);
    digest.update(blob);
    hex::encode(digest.finalize())
}

/// Indexes the row `credential_id` of `user_id` as it is now: the fingerprint
/// of the session UID its ciphertext holds, bound to that ciphertext. A row
/// that is gone, or holds no session, has no entry.
#[cfg(feature = "proton-native")]
fn index_session_row(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    dek: &SecretKey,
    credential_id: &str,
) -> Result<(), String> {
    let row: Option<(Vec<u8>, Vec<u8>)> = conn
        .query_row(
            "SELECT encrypted_blob, nonce FROM user_credentials
             WHERE user_id = ?1 AND credential_id = ?2",
            params![user_id, credential_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("Read saved session: {e}"))?;
    let uid = row.as_ref().and_then(|(blob, nonce)| {
        let plaintext = user_crypto::decrypt_blob(dek, nonce, blob).ok()?;
        proton_session_uid(std::str::from_utf8(&plaintext).ok()?)
    });
    match (row, uid) {
        (Some((blob, nonce)), Some(uid)) => {
            let key = session_index_key(root_key);
            conn.execute(
                "INSERT INTO proton_session_index(
                     user_id, credential_id, fingerprint, blob_digest, key_tag, updated_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(user_id, credential_id) DO UPDATE SET
                     fingerprint = excluded.fingerprint,
                     blob_digest = excluded.blob_digest,
                     key_tag     = excluded.key_tag,
                     updated_at  = excluded.updated_at",
                params![
                    user_id,
                    credential_id,
                    session_fingerprint(&key, &uid),
                    credential_blob_digest(&blob, &nonce),
                    session_index_key_tag(&key),
                    now_ms()
                ],
            )
            .map_err(|e| format!("Write session index: {e}"))?;
        }
        _ => {
            conn.execute(
                "DELETE FROM proton_session_index WHERE user_id = ?1 AND credential_id = ?2",
                params![user_id, credential_id],
            )
            .map_err(|e| format!("Clear session index: {e}"))?;
        }
    }
    Ok(())
}

/// Indexes the native Proton session `user_id` just saved or refreshed under
/// `credential_id` (see [`other_accounts_holding_session`]).
#[cfg(feature = "proton-native")]
pub(crate) fn index_proton_session(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    with_user_dek(&conn, &root_secret, user_id, |id, dek| {
        index_session_row(&conn, &root_key, id, dek, credential_id)
    })
}

/// Indexes every native Proton session `user_id` holds, when the account is
/// unlocked: the rows saved before the index, or rewritten without it, get
/// their entry then.
#[cfg(feature = "proton-native")]
fn index_account_sessions(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    dek: &SecretKey,
) -> Result<(), String> {
    for credential_id in user_credential_ids_of_type(conn, user_id, PROTON_SESSION_TYPE)? {
        index_session_row(conn, root_key, user_id, dek, &credential_id)?;
    }
    Ok(())
}

/// What the index tells of the accounts other than one about a session.
#[cfg(feature = "proton-native")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OtherAccounts {
    /// No other account holds it.
    HoldNone,
    /// Another account holds a session with the same fingerprint.
    HoldIt,
    /// Another account holds a session the index does not describe (saved
    /// before it, or rewritten since, and not indexed at an unlock yet),
    /// which may be this one.
    MayHoldIt,
}

/// The rows of the accounts other than `user` that hold a native Proton
/// session, with their index entries ([`other_accounts_holding_session`]).
#[cfg(feature = "proton-native")]
type SessionIndexRow = (
    String,
    Vec<u8>,
    Vec<u8>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// What selects a native Proton session row by its key in SQL, for
/// `column`: the three shapes [`is_proton_session_key`] accepts (`_` taken
/// literally, a non-empty tail), so no other secret's ciphertext is loaded.
/// [`is_proton_session_key`] stays the authority over what is read.
fn proton_session_key_sql(column: &str) -> String {
    format!(
        "({column} = '{PROTON_UNBOUND_SESSION}' \
         OR {column} LIKE 'proton\\_native\\_session\\__%' ESCAPE '\\' \
         OR {column} LIKE 'proton\\_native\\_unbound\\_user\\__%' ESCAPE '\\')"
    )
}

#[cfg(feature = "proton-native")]
fn other_accounts_session_rows(
    conn: &Connection,
    user: Option<i64>,
) -> Result<Vec<SessionIndexRow>, String> {
    let rows: Vec<SessionIndexRow> = conn
        .prepare(&format!(
            "SELECT c.credential_id, c.encrypted_blob, c.nonce,
                    i.fingerprint, i.blob_digest, i.key_tag
             FROM user_credentials c
             LEFT JOIN proton_session_index i
                 ON i.user_id = c.user_id AND i.credential_id = c.credential_id
             WHERE (?1 IS NULL OR c.user_id <> ?1) AND {}",
            proton_session_key_sql("c.credential_id")
        ))
        .and_then(|mut statement| {
            statement
                .query_map(params![user], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|e| format!("Read session index: {e}"))?;
    Ok(rows
        .into_iter()
        .filter(|(credential_id, ..)| is_proton_session_key(credential_id))
        .collect())
}

/// Whether an account other than `user` holds the native Proton session
/// `uid`, read from the index alone: no account's partition is decrypted,
/// so a locked account answers as an unlocked one does.
#[cfg(feature = "proton-native")]
pub(crate) fn other_accounts_holding_session(
    store: &CredentialStore,
    user: Option<i64>,
    uid: &str,
) -> Result<OtherAccounts, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    other_accounts_holding_session_in(&conn, &root_key, user, uid)
}

/// [`other_accounts_holding_session`] on `conn`, with the root key.
#[cfg(feature = "proton-native")]
fn other_accounts_holding_session_in(
    conn: &Connection,
    root_key: &[u8; 32],
    user: Option<i64>,
    uid: &str,
) -> Result<OtherAccounts, String> {
    let key = session_index_key(root_key);
    let (fingerprint, key_tag) = (session_fingerprint(&key, uid), session_index_key_tag(&key));
    let mut answer = OtherAccounts::HoldNone;
    for (_, blob, nonce, entry, digest, tag) in other_accounts_session_rows(conn, user)? {
        let describes_the_row = tag.as_deref() == Some(key_tag.as_str())
            && digest.as_deref() == Some(credential_blob_digest(&blob, &nonce).as_str());
        match entry {
            Some(entry) if describes_the_row => {
                if entry == fingerprint {
                    return Ok(OtherAccounts::HoldIt);
                }
            }
            _ => answer = OtherAccounts::MayHoldIt,
        }
    }
    Ok(answer)
}

/// Whether `user_id`'s partition is locked in this process: an account with
/// a passphrase not entered here. Nothing is decrypted.
#[cfg(feature = "proton-native")]
pub(crate) fn partition_locked(store: &CredentialStore, user_id: i64) -> Result<bool, String> {
    let conn = open_or_init_cli()?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    match with_user_dek(&conn, &root_secret, user_id, |_, _| Ok(())) {
        Ok(()) => Ok(false),
        Err(error) if error == "USER_LOCKED" => Ok(true),
        Err(error) => Err(error),
    }
}

/// Marks a native Proton session a profile purge left in a locked account,
/// which cannot read it to end it on Proton ([`defer_proton_revoke`]).
#[cfg(feature = "proton-native")]
const DEFERRED_REVOKE_PREFIX: &str = "proton_deferred_revoke_";

#[cfg(feature = "proton-native")]
fn deferred_revoke_prefix(user_id: i64) -> String {
    format!("{DEFERRED_REVOKE_PREFIX}{user_id}_")
}

#[cfg(feature = "proton-native")]
fn deferred_revoke_key(user_id: i64, profile_id: &str) -> String {
    format!("{}{profile_id}", deferred_revoke_prefix(user_id))
}

/// The session a purge left to an unlock, named without its key: the
/// session index fingerprint of its UID. A refresh keeps the UID and a new
/// sign-in gets another one, so the fingerprint follows the purged session
/// through a refresh and never names a sign-in made after the purge. `None`
/// while no fingerprint of the purged row could be taken: the account's
/// unlock takes it from the row it reads then. Each mark is written with a
/// nonce of its own, so a mark that replaces another of the same profile is
/// never the same value: what removes or rewrites a mark does it only while
/// the mark is still the one it read (empty on a mark written before).
/// `unbound` marks the legacy owner's global unbound session a sign-in
/// replaced and could not remove ([`defer_replaced_unbound_proton_revoke`]):
/// its `profile_id` is empty and it names the global unbound key, not a
/// profile's.
#[cfg(feature = "proton-native")]
#[derive(Serialize, Deserialize)]
struct DeferredRevoke {
    profile_id: String,
    fingerprint: Option<String>,
    #[serde(default)]
    nonce: String,
    #[serde(default)]
    unbound: bool,
}

/// The fingerprint of the session the row `credential_id` of `user_id` holds
/// now: from its plaintext when this process can read the account's
/// partition, otherwise from the index entry, while it describes that very
/// ciphertext. `None` when neither tells.
#[cfg(feature = "proton-native")]
fn saved_session_fingerprint(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
) -> Result<Option<String>, String> {
    let Some((blob, nonce)) = conn
        .query_row(
            "SELECT encrypted_blob, nonce FROM user_credentials
             WHERE user_id = ?1 AND credential_id = ?2",
            params![user_id, credential_id],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .map_err(|e| format!("Read the purged session: {e}"))?
    else {
        return Ok(None);
    };
    let key = session_index_key(root_key);
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let read = with_user_dek(conn, &root_secret, user_id, |_, dek| {
        let plaintext = user_crypto::decrypt_blob(dek, &nonce, &blob)
            .map_err(|_| "the saved session could not be decrypted".to_string())?;
        Ok(std::str::from_utf8(&plaintext)
            .ok()
            .and_then(proton_session_uid))
    });
    match read {
        Ok(uid) => return Ok(uid.map(|uid| session_fingerprint(&key, &uid))),
        Err(error) if error == "USER_LOCKED" => {}
        Err(error) => return Err(error),
    }
    conn.query_row(
        "SELECT fingerprint FROM proton_session_index
         WHERE user_id = ?1 AND credential_id = ?2 AND blob_digest = ?3 AND key_tag = ?4",
        params![
            user_id,
            credential_id,
            credential_blob_digest(&blob, &nonce),
            session_index_key_tag(&key)
        ],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| format!("Read session index: {e}"))
}

#[cfg(feature = "proton-native")]
fn write_deferred_revoke(
    conn: &Connection,
    user_id: i64,
    deferred: &DeferredRevoke,
) -> Result<(), String> {
    let mark = serde_json::to_string(deferred)
        .map_err(|e| format!("Encode deferred Proton revoke: {e}"))?;
    conn.execute(
        "INSERT INTO global_state(key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![
            deferred_revoke_key(user_id, &deferred.profile_id),
            mark,
            now_ms()
        ],
    )
    .map_err(|e| format!("Write deferred Proton revoke: {e}"))?;
    Ok(())
}

/// Leaves the native Proton session of the purged profile `profile_id` in
/// the locked account `user_id`, its row as it is, marked with the session
/// it holds ([`DeferredRevoke`]), so the account's next unlock ends it on
/// Proton and removes it ([`end_deferred_proton_sessions`]): a session is
/// never dropped without its revoke, and a session saved there after the
/// purge is never ended by it.
#[cfg(feature = "proton-native")]
pub fn defer_proton_revoke(
    store: &CredentialStore,
    user_id: i64,
    profile_id: &str,
) -> Result<(), String> {
    let conn = open_or_init_cli()?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let credential_id = crate::providers::proton_native::vault::session_key(Some(profile_id));
    let fingerprint = saved_session_fingerprint(&conn, &root_key, user_id, &credential_id)?;
    write_deferred_revoke(
        &conn,
        user_id,
        &DeferredRevoke {
            profile_id: profile_id.to_string(),
            fingerprint,
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            unbound: false,
        },
    )
}

/// Leaves to `user_id`'s next unlock the global unbound session `uid` that
/// a sign-in of the legacy owner replaced, saved its own session, and then
/// could not remove ([`crate::providers::proton_native::vault::replace_session`]):
/// still saved under the global key and live on Proton, it is marked as a
/// purge marks a session ([`DeferredRevoke`], `unbound`), so the unlock ends
/// it on Proton and removes it ([`end_deferred_proton_sessions`]), while the
/// key still holds that session. A session saved there since is kept.
#[cfg(feature = "proton-native")]
pub(crate) fn defer_replaced_unbound_proton_revoke(
    store: &CredentialStore,
    user_id: i64,
    uid: &str,
) -> Result<(), String> {
    let conn = open_or_init_cli()?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    write_deferred_revoke(
        &conn,
        user_id,
        &DeferredRevoke {
            profile_id: String::new(),
            fingerprint: Some(session_fingerprint(&session_index_key(&root_key), uid)),
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            unbound: true,
        },
    )
}

/// Pins the mark a purge of `profile_id` left to `user_id`'s unlock on the
/// session saved there now, `uid` (`None` when none is), for a writer that
/// saves a session for the profile, under its key lock, before it saves: a
/// mark without a fingerprint gets the one of `uid`, so the mark names the
/// purged session and never the one about to be saved, and a mark over no
/// saved session goes, since nothing is left to end. The mark's value as it
/// stands then, for [`clear_deferred_proton_revoke`] once the save is done,
/// or `None` when no mark is left. The mark stays until then: a save that
/// fails, or a process that exits before the clear, leaves it to the next
/// unlock, which ends the purged session, or drops the mark without a revoke
/// once the row holds another session.
#[cfg(feature = "proton-native")]
pub(crate) fn pin_deferred_proton_revoke(
    store: &CredentialStore,
    user_id: i64,
    profile_id: &str,
    uid: Option<&str>,
) -> Result<Option<String>, String> {
    let mut conn = open_or_init_cli()?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Pin deferred Proton revoke: {e}"))?;
    let key = deferred_revoke_key(user_id, profile_id);
    let value: Option<String> = tx
        .query_row(
            "SELECT value FROM global_state WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Pin deferred Proton revoke: {e}"))?;
    let Some(value) = value else {
        return Ok(None);
    };
    // A mark that names no profile ends nothing: it is cleared as it is.
    let Ok(mut deferred) = serde_json::from_str::<DeferredRevoke>(&value) else {
        return Ok(Some(value));
    };
    if deferred.fingerprint.is_some() {
        return Ok(Some(value));
    }
    let Some(uid) = uid else {
        tx.execute(
            "DELETE FROM global_state WHERE key = ?1 AND value = ?2",
            params![key, value],
        )
        .map_err(|e| format!("Pin deferred Proton revoke: {e}"))?;
        tx.commit()
            .map_err(|e| format!("Pin deferred Proton revoke: {e}"))?;
        return Ok(None);
    };
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    deferred.fingerprint = Some(session_fingerprint(&session_index_key(&root_key), uid));
    let pinned = serde_json::to_string(&deferred)
        .map_err(|e| format!("Encode deferred Proton revoke: {e}"))?;
    tx.execute(
        "UPDATE global_state SET value = ?3, updated_at = ?4 WHERE key = ?1 AND value = ?2",
        params![key, value, pinned, now_ms()],
    )
    .map_err(|e| format!("Pin deferred Proton revoke: {e}"))?;
    tx.commit()
        .map_err(|e| format!("Pin deferred Proton revoke: {e}"))?;
    Ok(Some(pinned))
}

/// Removes the mark [`pin_deferred_proton_revoke`] pinned, once the session
/// saved over it is stored: only while it is still that mark, so one a later
/// purge wrote stays.
#[cfg(feature = "proton-native")]
pub(crate) fn clear_deferred_proton_revoke(
    user_id: i64,
    profile_id: &str,
    pinned: &str,
) -> Result<(), String> {
    open_or_init_cli()?
        .execute(
            "DELETE FROM global_state WHERE key = ?1 AND value = ?2",
            params![deferred_revoke_key(user_id, profile_id), pinned],
        )
        .map_err(|e| format!("Clear deferred Proton revoke: {e}"))?;
    Ok(())
}

/// Ends the sessions a purge left to `user_id`'s unlock, once the unlock
/// has indexed the account's rows: each on Proton unless another account
/// holds it (the session index decides, as at any sign-out), then its row
/// and its mark go. A mark whose profile the account lists again, or whose
/// row holds another session now, goes and the row stays. One that fails
/// keeps its mark, with the fingerprint of the row read now, for the next
/// unlock.
#[cfg(feature = "proton-native")]
fn end_deferred_proton_sessions(conn: &Connection, root_key: &[u8; 32], user_id: i64) {
    let prefix = deferred_revoke_prefix(user_id);
    let marks: Vec<(String, String)> = match conn
        .prepare(
            "SELECT key, value FROM global_state
             WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key",
        )
        .and_then(|mut statement| {
            statement
                .query_map(params![prefix], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        }) {
        Ok(marks) => marks,
        Err(error) => {
            log::warn!("Unlock: the Proton sessions left by a purge could not be listed: {error}");
            return;
        }
    };
    for (mark, value) in marks {
        // A mark that names no profile ends nothing. What goes is the mark
        // as this cleanup left it, and only while it is: a mark a later
        // purge wrote meanwhile stays for its own unlock.
        let ended = match serde_json::from_str::<DeferredRevoke>(&value) {
            Ok(deferred) => end_deferred_proton_session(conn, root_key, user_id, deferred, &value),
            Err(_) => Ok(Some(value)),
        }
        .and_then(|done| match done {
            Some(done) => conn
                .execute(
                    "DELETE FROM global_state WHERE key = ?1 AND value = ?2",
                    params![mark, done],
                )
                .map(|_| ())
                .map_err(|error| error.to_string()),
            None => Ok(()),
        });
        if let Err(error) = ended {
            log::warn!(
                "Unlock: a Proton session left by a purge stays for the next unlock: {error}"
            );
        }
    }
}

/// How long an unlock waits for the sessions a purge left to it
/// ([`end_deferred_proton_sessions`]) before it returns: past it they go on
/// being ended in the background, and a mark not ended by then stays for
/// the next unlock.
#[cfg(all(feature = "proton-native", not(test)))]
const DEFERRED_CLEANUP_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);
/// In a test the cleanup is waited for to its end, unless the test sets a
/// budget ([`DEFERRED_CLEANUP_BUDGET_IN_TEST`]).
#[cfg(all(feature = "proton-native", test))]
const DEFERRED_CLEANUP_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// [`DEFERRED_CLEANUP_BUDGET`], or the one a test set.
#[cfg(feature = "proton-native")]
fn deferred_cleanup_budget() -> std::time::Duration {
    #[cfg(test)]
    if let Some(budget) = *DEFERRED_CLEANUP_BUDGET_IN_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
    {
        return budget;
    }
    DEFERRED_CLEANUP_BUDGET
}

/// The budget of an unlock's deferred cleanup a test set (test only).
#[cfg(all(test, feature = "proton-native"))]
pub(crate) static DEFERRED_CLEANUP_BUDGET_IN_TEST: Mutex<Option<std::time::Duration>> =
    Mutex::new(None);

/// [`end_deferred_proton_sessions`] for `user_id` on a thread of its own,
/// with a connection of its own, waited for up to [`deferred_cleanup_budget`]:
/// an unlock while Proton cannot be reached returns, and the cleanup goes on
/// beyond it. One cleanup per account at a time; an unlock while one runs
/// leaves the marks to it, and to the next unlock what it did not end.
#[cfg(feature = "proton-native")]
fn end_deferred_proton_sessions_within_budget(root_key: &[u8; 32], user_id: i64) {
    static RUNNING: Mutex<Vec<i64>> = Mutex::new(Vec::new());
    {
        let mut running = RUNNING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if running.contains(&user_id) {
            return;
        }
        running.push(user_id);
    }
    let root_key = Zeroizing::new(*root_key);
    let (done, finished) = std::sync::mpsc::channel::<()>();
    let spawned = std::thread::Builder::new()
        .name("proton-deferred-cleanup".into())
        .spawn(move || {
            match open_or_init_cli() {
                Ok(conn) => end_deferred_proton_sessions(&conn, &root_key, user_id),
                Err(error) => log::warn!(
                    "Unlock: the Proton sessions left by a purge stay for the next unlock: {error}"
                ),
            }
            RUNNING
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .retain(|running| *running != user_id);
            let _ = done.send(());
        });
    if let Err(error) = spawned {
        RUNNING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|running| *running != user_id);
        log::warn!("Unlock: the Proton sessions left by a purge stay for the next unlock: {error}");
        return;
    }
    if finished.recv_timeout(deferred_cleanup_budget()).is_err() {
        log::warn!(
            "Unlock: the Proton sessions left by a purge are still being ended in the background"
        );
    }
}

/// Ends the session `deferred` names. A mark made without a fingerprint
/// (the account locked and its row not indexed) gets the one of the row
/// read now, which is the purged session or its refresh: a sign-in saved
/// there since cleared the mark. Under the session's key lock and lease the
/// account must not list the profile again and the row must hold that
/// session (any session, for a row no fingerprint could be taken of, so
/// none is left behind). The mark's value once this is done with it, for
/// the caller to remove while it is still that value, or `None` when the
/// mark changed under it and is not this cleanup's any more.
#[cfg(feature = "proton-native")]
fn end_deferred_proton_session(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    mut deferred: DeferredRevoke,
    value: &str,
) -> Result<Option<String>, String> {
    use crate::providers::proton_native::vault;
    #[cfg(test)]
    if let Some(hook) = BEFORE_DEFERRED_SESSION_END
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        hook();
    }
    let mut value = value.to_string();
    if deferred.unbound {
        // The global unbound session a sign-in replaced: ended while the key
        // still holds it and its mark is still there, and the account's
        // profiles are not asked, since no profile names it.
        let root_key = Zeroizing::new(*root_key);
        let Some(purged) = deferred.fingerprint else {
            return Ok(Some(value));
        };
        vault::end_replaced_unbound_session_blocking(
            vault::SessionOwner::for_user(Some(user_id)),
            move |uid| {
                let conn = open_or_init_cli()?;
                Ok(read_deferred_revoke(&conn, user_id, "")?.is_some()
                    && session_fingerprint(&session_index_key(&root_key), uid) == purged)
            },
        )
        .map_err(|error| error.to_string())?;
        return Ok(Some(value));
    }
    let credential_id = vault::session_key(Some(&deferred.profile_id));
    if deferred.fingerprint.is_none() {
        deferred.fingerprint = saved_session_fingerprint(conn, root_key, user_id, &credential_id)?;
        // Onto the mark only while it is the one read: a sign-in, an import
        // or a duplicate that saved a session for the profile since pinned
        // or cleared it, and the fingerprint just read is that session's.
        if deferred.fingerprint.is_some() {
            match fingerprint_deferred_revoke(conn, user_id, &deferred, &value)? {
                Some(fingerprinted) => value = fingerprinted,
                None => return Ok(None),
            }
        }
    }
    let root_key = Zeroizing::new(*root_key);
    let profile_id = deferred.profile_id.clone();
    vault::end_purged_session_blocking(
        vault::SessionOwner::for_user(Some(user_id)),
        &profile_id,
        move |uid| {
            let conn = open_or_init_cli()?;
            // The mark as it is now, under the key lock every writer of a
            // session for the profile holds while it clears the mark: one
            // gone is a session saved since, never the purged one.
            let Some(mark) = read_deferred_revoke(&conn, user_id, &deferred.profile_id)? else {
                return Ok(false);
            };
            let listed = list_server_profiles_for(&conn, &root_key, user_id)?
                .iter()
                .any(|profile| {
                    value_str(profile, &["id", "uid", "profileUid"])
                        == Some(deferred.profile_id.as_str())
                });
            let fingerprint = session_fingerprint(&session_index_key(&root_key), uid);
            Ok(!listed && mark.fingerprint.is_none_or(|purged| purged == fingerprint))
        },
    )
    .map_err(|error| error.to_string())?;
    #[cfg(test)]
    if let Some(hook) = AFTER_DEFERRED_SESSION_END
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        hook();
    }
    Ok(Some(value))
}

/// Puts the fingerprint of `deferred` on its mark, only while the mark is
/// still `value`. The mark's new value, or `None` when it was not.
#[cfg(feature = "proton-native")]
fn fingerprint_deferred_revoke(
    conn: &Connection,
    user_id: i64,
    deferred: &DeferredRevoke,
    value: &str,
) -> Result<Option<String>, String> {
    let mark = serde_json::to_string(deferred)
        .map_err(|e| format!("Encode deferred Proton revoke: {e}"))?;
    conn.execute(
        "UPDATE global_state SET value = ?3, updated_at = ?4 WHERE key = ?1 AND value = ?2",
        params![
            deferred_revoke_key(user_id, &deferred.profile_id),
            value,
            mark,
            now_ms()
        ],
    )
    .map(|updated| (updated == 1).then_some(mark))
    .map_err(|e| format!("Write deferred Proton revoke: {e}"))
}

/// The mark a purge left for `profile_id` of `user_id`, as stored now. One
/// that does not decode is an error: it ends nothing, and stays.
#[cfg(feature = "proton-native")]
fn read_deferred_revoke(
    conn: &Connection,
    user_id: i64,
    profile_id: &str,
) -> Result<Option<DeferredRevoke>, String> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM global_state WHERE key = ?1",
            params![deferred_revoke_key(user_id, profile_id)],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Read deferred Proton revoke: {e}"))?;
    value
        .map(|value| {
            serde_json::from_str(&value).map_err(|e| format!("Read deferred Proton revoke: {e}"))
        })
        .transpose()
}

/// Runs once in the next deferred cleanup, after the marks are read and
/// before the first is ended (test only).
#[cfg(all(test, feature = "proton-native"))]
pub(crate) static BEFORE_DEFERRED_SESSION_END: Mutex<Option<Box<dyn FnOnce() + Send>>> =
    Mutex::new(None);

/// Runs once in the next deferred cleanup, after a mark's session was ended
/// or kept and before the mark is removed (test only).
#[cfg(all(test, feature = "proton-native"))]
pub(crate) static AFTER_DEFERRED_SESSION_END: Mutex<Option<Box<dyn FnOnce() + Send>>> =
    Mutex::new(None);

/// The account that owns legacy unbound/global secrets, for one-time reads
/// of sessions saved before native Proton had explicit user ownership.
pub(crate) fn legacy_credential_owner_id() -> Result<Option<i64>, String> {
    nonprofile_secret_owner(&open_or_init_cli()?)
}

/// Capture an explicit credential owner, or the active user exactly once, and
/// whether that user may read pre-partition global credentials, on one
/// connection. No user means the installation has not entered the multi-user
/// ownership model yet, so the legacy fallback stays open.
/// The account a reader names when the connection began with NO active
/// account (T9): the legacy owner only, never whichever account is active by
/// the time it reads. `i64::MIN` is no row id (AUTOINCREMENT ids are
/// positive), so any reader or writer not taught about it finds no account
/// and fails closed; it never falls back to the active one, since it is not
/// `None`. Taught: [`credential_owner_and_fallback`], [`mcp_list_server_profiles`],
/// [`resolve_credential_for_user`], [`resolve_credential_for_user_owner_aware`]
/// (the legacy stores) and [`mirror_credential_for_user_typed`] (no partition
/// to mirror into).
pub const LEGACY_ONLY_ACCOUNT: i64 = i64::MIN;

pub fn credential_owner_and_fallback(explicit: Option<i64>) -> Result<(Option<i64>, bool), String> {
    if explicit == Some(LEGACY_ONLY_ACCOUNT) {
        // The legacy owner: no partition, the legacy stores only.
        return Ok((None, true));
    }
    let conn = open_or_init_cli()?;
    let user = match explicit {
        Some(user) => Some(user),
        None => active_user_id(&conn)?,
    };
    let allowed = user.is_none() || nonprofile_secret_owner(&conn)? == user;
    Ok((user, allowed))
}

/// `Err("USER_LOCKED")` for a passphrase account whose session is not unlocked.
/// Needs no vault, so readers can refuse before any global, cached or plaintext
/// fallback even while the credential store is closed.
pub fn ensure_credential_user_unlocked(user_id: i64) -> Result<(), String> {
    let conn = open_or_init_cli()?;
    let row = read_user_key_row(&conn, user_id)?;
    if !row.has_passphrase {
        return Ok(());
    }
    if session_unlocks(user_id, &row.dek_verifier)? {
        Ok(())
    } else {
        Err("USER_LOCKED".to_string())
    }
}

/// Whether the session holds `user_id`'s key.
fn session_unlocks(user_id: i64, dek_verifier: &[u8]) -> Result<bool, String> {
    let _session = hold_session()?;
    HELD_SESSION.with_borrow(|held| match held.as_ref().and_then(Option::as_ref) {
        Some((session_user, dek)) if *session_user == user_id => {
            user_crypto::verify_dek(dek, dek_verifier)
        }
        _ => Ok(false),
    })
}

/// Whether the active account of the database at `db_path` is protected by a
/// passphrase and locked now, so its list cannot be written.
pub fn active_user_locked_in_db(db_path: &std::path::Path) -> Result<bool, String> {
    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("Open user partitions DB: {e}"))?;
    let Some(user_id) = active_user_id(&conn)? else {
        return Ok(false);
    };
    let row = read_user_key_row(&conn, user_id)?;
    if !row.has_passphrase {
        return Ok(false);
    }
    // A restored database can give this numeric id to another account, so the id
    // alone does not say that this process's session opens it.
    Ok(!session_unlocks(user_id, &row.dek_verifier)?)
}

/// Strict write of one secret into `user_id`'s partition only, never the
/// global vault. For credentials that belong to that user alone.
pub fn store_credential_for_user(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    store_credential_for_user_as(
        store,
        user_id,
        credential_id,
        credential_type,
        secret,
        false,
    )
}

/// [`store_credential_for_user`] for a sign-in of `user_id`'s own: a private
/// token is stamped in the same write ([`set_signed_in_credential_for`]),
/// which is what keeps it on a profile id another account also holds
/// ([`align_private_tokens_inner`]). Only a sign-in writer of that account
/// calls this; a restore, an import, a copy or a relocation never does.
pub fn store_signed_in_credential_for_user(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    store_credential_for_user_as(store, user_id, credential_id, credential_type, secret, true)
}

fn store_credential_for_user_as(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
    signed_in: bool,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let write = if signed_in {
        set_signed_in_credential_for
    } else {
        set_user_credential_for
    };
    write(
        &conn,
        &root_key,
        user_id,
        credential_id,
        credential_type,
        secret,
    )
}

fn read_credential_for_user(
    conn: &Connection,
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = read_credential_with_fallback(conn, store, &root_key, user_id, credential_id);
    root_key.zeroize();
    result
}

fn read_credential_for_user_owner_aware(
    conn: &Connection,
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    let mut root_key = store.derive_user_partition_wrapping_key();
    let read_secret = |key: &str| get_optional_secret(store, key);
    let result =
        read_credential_owner_aware_inner(conn, &root_key, user_id, credential_id, &read_secret);
    root_key.zeroize();
    result
}

fn read_credential_owner_aware_inner(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
    read_secret: &SecretReader<'_>,
) -> Result<Option<Zeroizing<String>>, String> {
    if nonprofile_secret_owner(conn)? == Some(user_id) {
        read_credential_with_fallback_inner(conn, root_key, user_id, credential_id, read_secret)
    } else {
        Ok(read_checked_row_with(conn, root_key, user_id, credential_id, read_secret)?.value())
    }
}

/// Active-user dual writer (GUI): write the vault first, then mirror in-scope
/// keys, and any key that already has a row, into the active user's partition.
/// Other keys land in the vault only, which is the correct MUV-3 behaviour for the
/// generic `store_credential` command. A failed mirror is an error
/// ([`mirror_into_partition`]).
///
/// The account is the one active when the call starts: the vault write may wait
/// for another process's lock, and an account switched to meanwhile must not
/// receive this secret.
pub fn store_active_credential_dual(
    store: &CredentialStore,
    credential_id: &str,
    secret: &str,
) -> Result<(), String> {
    store_credential_dual_as(store, active_user_id_for_write()?, credential_id, secret)
}

/// The account a GUI save mirrors into, read when the request arrives, before
/// it is handed to a blocking thread ([`store_credential_dual_as`]).
pub fn active_user_for_save() -> Result<Option<i64>, String> {
    active_user_id_for_write()
}

/// [`store_active_credential_dual`] for `user_id`, the account active when the
/// request arrived.
pub fn store_credential_dual_as(
    store: &CredentialStore,
    user_id: Option<i64>,
    credential_id: &str,
    secret: &str,
) -> Result<(), String> {
    store_credential_dual_with(
        store,
        credential_id,
        secret,
        || Ok(user_id),
        |user_id| mirror_credential_for_user(store, user_id, credential_id, secret),
    )
}

/// [`store_active_credential_dual`] with the account lookup and the mirror
/// passed in: `active_user` runs before the vault write.
fn store_credential_dual_with(
    store: &CredentialStore,
    credential_id: &str,
    secret: &str,
    active_user: impl FnOnce() -> Result<Option<i64>, String>,
    mirror: impl FnOnce(i64) -> Result<(), String>,
) -> Result<(), String> {
    let user_id = active_user()?;
    store_vault_then_mirror(store, credential_id, secret, || match user_id {
        Some(user_id) => mirror(user_id),
        None => Ok(()),
    })
}

/// Writes `secret` to the vault key `credential_id`, then `mirror`, the
/// account's half: whole or not at all. A mirror that fails puts the vault key
/// back as it was ([`undo_vault_write`]), so a save reported as failed never
/// leaves its value where a reader of the vault finds it.
pub(crate) fn store_vault_then_mirror(
    store: &CredentialStore,
    credential_id: &str,
    secret: &str,
    mirror: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    #[cfg(test)]
    run_dual_write_hook(&BEFORE_VAULT_WRITE);
    // The value this write replaces, read under the same vault lock: one read
    // before it would be undone over a value another writer stored between.
    let previous = store
        .store_returning_previous(credential_id, secret)
        .map_err(|e| format!("Store credential in vault: {e}"))?;
    #[cfg(test)]
    run_dual_write_hook(&BEFORE_MIRROR);
    mirror().map_err(|error| {
        undo_vault_write(
            store,
            credential_id,
            secret,
            previous.as_deref().map(String::as_str),
            &error,
        )
    })
}

/// A step a test runs once inside [`store_vault_then_mirror`].
#[cfg(test)]
type DualWriteHook = RefCell<Option<Box<dyn FnOnce()>>>;

#[cfg(test)]
thread_local! {
    /// Runs once on this thread at the start of the next
    /// [`store_vault_then_mirror`], before its vault write: another writer
    /// storing meanwhile.
    pub(crate) static BEFORE_VAULT_WRITE: DualWriteHook = const { RefCell::new(None) };
    /// Runs once on this thread in the next [`store_vault_then_mirror`],
    /// between its vault write and its mirror.
    pub(crate) static BEFORE_MIRROR: DualWriteHook = const { RefCell::new(None) };
}

#[cfg(test)]
fn run_dual_write_hook(hook: &'static std::thread::LocalKey<DualWriteHook>) {
    if let Some(hook) = hook.take() {
        hook();
    }
}

/// Puts the vault key `credential_id` back as it was before `written`: the
/// `previous` value, or no key. Only while it still holds `written`, so a
/// value another writer stored since stays. The error the caller returns,
/// saying what became of the vault copy.
pub(crate) fn undo_vault_write(
    store: &CredentialStore,
    credential_id: &str,
    written: &str,
    previous: Option<&str>,
    error: &str,
) -> String {
    let undone = match previous {
        Some(previous) => store.replace_secret_if_unchanged(credential_id, written, previous),
        None => store.delete_secret_if_unchanged(credential_id, written),
    };
    match undone {
        Ok(true) => format!("{error}; the vault copy was put back as it was"),
        Ok(false) => format!("{error}; the vault copy was rewritten since, so it stays as written"),
        Err(undo) => format!("{error}; the vault copy could not be put back: {undo}"),
    }
}

/// Explicit-user dual writer (CLI `--user`): same as
/// [`store_active_credential_dual`] but mirrors into `user_id`'s partition. The
/// caller resolves `user_id` from `ensure_active_user_unlocked` so the secret is
/// scoped to the invocation's `--user`, not the persisted active user.
pub fn store_credential_for_user_dual(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    secret: &str,
) -> Result<(), String> {
    // A private token keeps its type, so another account's stays in its
    // partition (a profile copy, `profile duplicate`).
    if let Some(kind) = private_token_type(credential_id) {
        return store_credential_for_user_typed_dual(store, user_id, credential_id, kind, secret);
    }
    store_vault_then_mirror(store, credential_id, secret, || {
        mirror_credential_for_user(store, user_id, credential_id, secret)
    })
}

/// The type `credential_id` is stored with in a partition: `credential_type` when
/// the caller knows it, the one its prefix gives, or the one its row already
/// carries. `None`: the key stays in the vault alone, as other writers keep it.
fn partition_credential_type(
    conn: &Connection,
    user_id: i64,
    credential_id: &str,
    credential_type: Option<&str>,
) -> Result<Option<String>, String> {
    if let Some(known) = credential_type.or_else(|| muv3_credential_type(credential_id)) {
        return Ok(Some(known.to_string()));
    }
    conn.query_row(
        "SELECT credential_type FROM user_credentials WHERE user_id = ?1 AND credential_id = ?2",
        params![user_id, credential_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| format!("Read credential type: {e}"))
}

/// The keys of `user_id`'s rows of one `credential_type`, read without
/// decrypting any of them: a caller that reads each row on its own is not
/// stopped by one it cannot decrypt.
pub fn user_credential_ids_of_type(
    conn: &Connection,
    user_id: i64,
    credential_type: &str,
) -> Result<Vec<String>, String> {
    conn.prepare(
        "SELECT credential_id FROM user_credentials
         WHERE user_id = ?1 AND credential_type = ?2 ORDER BY credential_id",
    )
    .and_then(|mut statement| {
        statement
            .query_map(params![user_id, credential_type], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()
    })
    .map_err(|e| format!("Read user credentials: {e}"))
}

/// Every row of `user_id` of one `credential_type`, decrypted, by key: all the
/// copies an account holds of one kind of token, for a caller that must see
/// each of them (the profiles sharing one native Proton session). One row it
/// cannot decrypt is an error.
pub fn user_credentials_of_type(
    store: &CredentialStore,
    user_id: i64,
    credential_type: &str,
) -> Result<Vec<(String, Zeroizing<String>)>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let ids = user_credential_ids_of_type(&conn, user_id, credential_type)?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let mut rows = Vec::with_capacity(ids.len());
    let mut failure = None;
    for id in ids {
        match read_checked_row(&conn, store, &root_key, user_id, &id).map(CheckedRow::value) {
            Ok(Some(value)) => rows.push((id, value)),
            Ok(None) => {}
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }
    root_key.zeroize();
    failure.map_or(Ok(rows), Err)
}

/// Write one secret of an import to everything that reads it, for `user_id`: the
/// partition alone for a token private to that user, otherwise the vault and the
/// partition row a reader prefers. A partition write that fails is an error even
/// for a locked account: its reader would go on using the old secret, under a
/// profile reported as imported.
pub fn store_imported_credential_for_user(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: Option<&str>,
    secret: &str,
) -> Result<(), String> {
    // A write that names no type still holds a token when its key does.
    if let Some(kind) = credential_type.or_else(|| private_token_type(credential_id)) {
        if let Some(private) = private_token_user(Some(user_id), kind)? {
            return store_credential_for_user(store, private, credential_id, kind, secret);
        }
    }
    store_vault_then_mirror(store, credential_id, secret, || {
        let conn = open_or_init_cli()?;
        let Some(kind) = partition_credential_type(&conn, user_id, credential_id, credential_type)?
        else {
            return Ok(());
        };
        let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
        set_user_credential_for(&conn, &root_key, user_id, credential_id, &kind, secret)
    })
}

/// Remove an imported credential from both the user's partition and the vault.
/// Both deletes must succeed or the import commit stops before writing profiles.
pub fn delete_imported_credential_for_user(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<(), String> {
    let mut conn = open_or_init_cli()?;
    let _session = hold_user_session(&conn, user_id);
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Start credential delete: {e}"))?;
    delete_user_credential_for(&tx, user_id, credential_id)?;
    // Another account's private token is its row alone: the vault key of that
    // name is the legacy owner's and stays.
    if vault_copy_is_another_accounts(&tx, user_id, credential_id)? {
        return tx
            .commit()
            .map_err(|e| format!("Commit credential delete: {e}"));
    }
    let before_vault = get_optional_secret(store, credential_id)?;
    store
        .delete(credential_id)
        .map_err(|e| format!("Delete credential from vault: {e}"))?;
    if let Err(e) = tx.commit() {
        let cause = format!("Commit credential delete: {e}");
        if let Some(prev) = before_vault {
            if let Err(undo) = store.store(credential_id, prev.as_str()) {
                return Err(format!(
                    "{cause}; the vault could not be put back to match the partition: {undo}"
                ));
            }
        }
        return Err(cause);
    }
    Ok(())
}

/// What one credential key holds before an import writes it, in the vault and
/// in `user`'s partition ([`snapshot_credential_for_user`]).
pub struct CredentialSnapshot {
    vault: Option<Zeroizing<String>>,
    partition: Option<Zeroizing<String>>,
    credential_type: Option<String>,
    /// Whether the account's own sign-in stamped the partition row, so a
    /// restore of it puts the stamp back with it.
    signed_in: bool,
}

/// Read what `credential_id` holds now, in both places a dual write reaches. An
/// error means it could not be read, and then it must not be written either.
pub fn snapshot_credential_for_user(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<CredentialSnapshot, String> {
    let conn = open_or_init_cli()?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    let credential_type = conn
        .query_row(
            "SELECT credential_type FROM user_credentials WHERE user_id = ?1 AND credential_id = ?2",
            params![user_id, credential_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Read credential type: {e}"))?;
    // The row and its stamp from one read of its bytes.
    let (partition, signed_in) =
        match get_user_credential_and_digest_for(&conn, &root_key, user_id, credential_id)? {
            Some((row, digest)) => (
                Some(row),
                stamp_matches(&conn, user_id, credential_id, &digest)?,
            ),
            None => (None, false),
        };
    Ok(CredentialSnapshot {
        vault: get_optional_secret(store, credential_id)?,
        partition,
        credential_type,
        signed_in,
    })
}

/// Undo a write of `written` to `credential_id`, place by place: a place still
/// holding `written` gets back what `before` holds there, or loses the key if it
/// held none; a place holding anything else was changed by someone else since
/// and stays as it is ([`crate::profile_import_plan::rollback_action`]). In the
/// partition that is atomic. The vault has no such transaction across processes,
/// so there a value stored between the check and the restore can be lost, and the
/// profile it belongs to asks for a new sign-in.
pub fn restore_credential_for_user(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: Option<&str>,
    written: &str,
    before: &CredentialSnapshot,
) -> Result<(), String> {
    restore_credential_op_for_user(
        store,
        user_id,
        credential_id,
        credential_type,
        false,
        written,
        before,
    )
}

/// Undo either an import write or an import delete (`written_delete == true`)
/// place by place ([`crate::profile_import_plan::rollback_action_for_write`]).
pub fn restore_credential_op_for_user(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: Option<&str>,
    written_delete: bool,
    written: &str,
    before: &CredentialSnapshot,
) -> Result<(), String> {
    use crate::profile_import_plan::{rollback_action_for_write, RollbackAction, SecretWrite};
    let mut conn = open_or_init_cli()?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    // The session and the partition's transaction first: if either cannot be had,
    // nothing is restored. The session before the reservation ([`SessionHold`]). Check
    // and restore share the transaction: a token refreshed in between stays.
    let _session = hold_user_session(&conn, user_id);
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Start credential restore: {e}"))?;
    let partition = get_user_credential_for(&tx, &root_key, user_id, credential_id)?;
    // Another account's private token was written to its row alone: the vault
    // key of that name is the legacy owner's, and the undo leaves it.
    let vault_is_theirs = !vault_copy_is_another_accounts(&tx, user_id, credential_id)?;
    let vault = if vault_is_theirs {
        get_optional_secret(store, credential_id)?
    } else {
        None
    };
    let write_op = if written_delete {
        SecretWrite::delete(credential_id.to_string(), None)
    } else {
        SecretWrite::new(credential_id.to_string(), None, written)
    };
    // The partition first: until the commit it can still go back by itself, so a
    // failure here or in the vault leaves both places holding what they held.
    match rollback_action_for_write(
        partition.as_ref().map(|v| v.as_str()),
        &write_op,
        before.partition.as_ref().map(|v| v.as_str()),
    ) {
        RollbackAction::Leave => {}
        RollbackAction::Restore(value) => {
            let kind = partition_credential_type(
                &tx,
                user_id,
                credential_id,
                before.credential_type.as_deref().or(credential_type),
            )?
            .ok_or_else(|| format!("No credential type for {credential_id}"))?;
            set_user_credential_for(&tx, &root_key, user_id, credential_id, &kind, value)?;
            // The value goes back as it was held: stamped by the account's own
            // sign-in, or not, never stamped by the restore itself.
            if before.signed_in {
                stamp_sign_in(&tx, user_id, credential_id)?;
            } else {
                forget_sign_in(&tx, user_id, credential_id)?;
            }
        }
        RollbackAction::Delete => delete_user_credential_for(&tx, user_id, credential_id)?,
    }
    let vault_action = if vault_is_theirs {
        rollback_action_for_write(
            vault.as_ref().map(|v| v.as_str()),
            &write_op,
            before.vault.as_ref().map(|v| v.as_str()),
        )
    } else {
        RollbackAction::Leave
    };
    let vault_changed = vault_action != RollbackAction::Leave;
    match vault_action {
        RollbackAction::Leave => {}
        RollbackAction::Restore(value) => store
            .store(credential_id, value)
            .map_err(|e| format!("Restore credential in vault: {e}"))?,
        RollbackAction::Delete => store
            .delete(credential_id)
            .map_err(|e| format!("Delete credential from vault: {e}"))?,
    }
    if let Err(e) = tx.commit() {
        let cause = format!("Commit credential restore: {e}");
        // The partition keeps what it held, so the vault goes back to match it.
        if vault_changed {
            let undone = match vault.as_ref() {
                Some(value) => store.store(credential_id, value.as_str()),
                None => store.delete(credential_id),
            };
            if let Err(undo) = undone {
                return Err(format!(
                    "{cause}; the vault could not be put back to match the partition: {undo}"
                ));
            }
        }
        return Err(cause);
    }
    Ok(())
}

/// Active-user dual delete (GUI): delete from the vault and from the active
/// user's partition, a row left behind being an error. Part of an explicit
/// user delete, never a purge.
/// The account is the one active when the call starts, as for
/// [`store_active_credential_dual`]. A lookup that fails is an error before
/// either copy changes: read as no account, it deleted the vault copy and
/// left the partition's, which readers prefer.
pub fn delete_active_credential_dual(
    store: &CredentialStore,
    credential_id: &str,
) -> Result<(), String> {
    delete_credential_dual_as(store, active_user_id_for_write()?, credential_id)
}

/// [`delete_active_credential_dual`] for `user_id`, the account active when
/// the request arrived.
pub fn delete_credential_dual_as(
    store: &CredentialStore,
    user_id: Option<i64>,
    credential_id: &str,
) -> Result<(), String> {
    delete_credential_dual_with(
        store,
        credential_id,
        || user_id,
        |user_id| unmirror_deleted_credential(user_id, credential_id),
    )
}

/// The partition half of a delete. Every reader takes the account's row
/// ahead of the vault, so a row left behind is the value the app goes on
/// using: its removal must succeed or the delete fails (and, for a
/// Jottacloud refresh chain, no deletion is recorded), whatever the key.
fn unmirror_deleted_credential(user_id: i64, credential_id: &str) -> Result<(), String> {
    #[cfg(test)]
    if PARTITION_DELETE_REFUSED.with(std::cell::Cell::get) {
        return Err("Delete credential from partition: refused".to_string());
    }
    try_unmirror_active_credential(user_id, credential_id)
        .map_err(|e| format!("Delete credential from partition: {e}"))
}

#[cfg(test)]
thread_local! {
    /// The partition refuses the row delete of a dual delete.
    pub(crate) static PARTITION_DELETE_REFUSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// [`delete_active_credential_dual`] with the account lookup and the unmirror
/// passed in: `active_user` runs before the vault delete. The account it gives
/// owns the deletion whatever the key, and `None` is no account: never one
/// activated since the caller read it. That account's partition copy goes, and
/// so does a Jottacloud rotation kept for it. Both copies go inside
/// [`delete_refresh_chain`], so no connection begins between them and reads
/// the one left.
///
/// A private token of an account other than the legacy owner is its row
/// alone ([`private_account_delete`]): the vault key of that name is the
/// owner's and stays, and the row's removal must succeed.
///
/// [`delete_refresh_chain`]: crate::providers::jottacloud::delete_refresh_chain
pub(crate) fn delete_credential_dual_with(
    store: &CredentialStore,
    credential_id: &str,
    active_user: impl FnOnce() -> Option<i64>,
    unmirror: impl FnOnce(i64) -> Result<(), String>,
) -> Result<(), String> {
    let user_id = active_user();
    crate::providers::jottacloud::delete_refresh_chain(store, user_id, credential_id, || {
        if let Some(user) = private_account_delete(user_id, credential_id)? {
            return delete_partition_credential_checked(user, credential_id);
        }
        store
            .delete(credential_id)
            .map_err(|e| format!("Delete credential from vault: {e}"))?;
        match user_id {
            Some(user_id) => unmirror(user_id),
            None => Ok(()),
        }
    })
}

/// Explicit-user dual delete (CLI `--user`): vault delete + removal from
/// `user_id`'s partition, both reported. Another account's private token: its
/// row only, as [`delete_credential_dual_with`].
pub fn delete_credential_for_user_dual(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<(), String> {
    crate::providers::jottacloud::delete_refresh_chain(store, Some(user_id), credential_id, || {
        if let Some(user) = private_account_delete(Some(user_id), credential_id)? {
            return delete_partition_credential_checked(user, credential_id);
        }
        store
            .delete(credential_id)
            .map_err(|e| format!("Delete credential from vault: {e}"))?;
        unmirror_deleted_credential(user_id, credential_id)
    })
}

/// `Some(user)` when deleting `credential_id` for `user` must leave the vault
/// alone: a private token of an account other than the legacy owner, whose
/// vault key of that name is the owner's ([`vault_copy_is_another_accounts`]).
/// A partition DB that cannot be opened is an error before anything changes,
/// since only the DB tells whose the vault key is; any other key never opens it.
fn private_account_delete(
    user_id: Option<i64>,
    credential_id: &str,
) -> Result<Option<i64>, String> {
    match user_id {
        Some(user) if private_token_type(credential_id).is_some() => {
            let conn = open_or_init_cli()?;
            // A delete is a write: the owner it is checked against is
            // recorded, so a later change of admins does not move it.
            nonprofile_secret_owner(&conn)?;
            Ok(vault_copy_is_another_accounts(&conn, user, credential_id)?.then_some(user))
        }
        _ => Ok(None),
    }
}

/// Remove `credential_id` from `user_id`'s partition and return the error, for a
/// caller whose report must name what is left; the best-effort unmirror drops it.
pub fn delete_partition_credential_checked(
    user_id: i64,
    credential_id: &str,
) -> Result<(), String> {
    delete_user_credential_for(&open_or_init_cli()?, user_id, credential_id)
}

/// [`delete_credential_for_user_dual`] that reports both halves: the vault entry
/// and the partition row are both attempted, and each failure is returned. For a
/// rollback that must say which secrets it could not remove. A private token of
/// an account other than the legacy owner is its row alone: the vault key of
/// that name is the owner's ([`vault_copy_is_another_accounts`]) and stays.
pub fn delete_credential_for_user_checked(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<(), String> {
    let conn = open_or_init_cli();
    delete_credential_checked_with(
        store,
        conn.as_ref().map_err(Clone::clone),
        user_id,
        credential_id,
    )
}

/// [`delete_credential_for_user_checked`] on `conn`, the partition DB or why it
/// could not be opened. Without it the vault entry of any other secret is still
/// deleted, and the open error returned beside it; the vault key of a private
/// token is kept, since only the DB tells whether it is this account's.
pub(crate) fn delete_credential_checked_with(
    store: &CredentialStore,
    conn: Result<&Connection, String>,
    user_id: i64,
    credential_id: &str,
) -> Result<(), String> {
    let partition = conn
        .clone()
        .and_then(|conn| delete_user_credential_for(conn, user_id, credential_id));
    if private_token_type(credential_id).is_some() {
        match conn.and_then(|conn| vault_copy_is_another_accounts(conn, user_id, credential_id)) {
            Ok(true) => return partition,
            Ok(false) => {}
            Err(error) => {
                return Err(format!(
                    "{error}; the vault key was kept, since without the partition database it cannot be told whose it is"
                ))
            }
        }
    }
    let vault = store
        .delete(credential_id)
        .map_err(|e| format!("Delete credential from vault: {e}"));
    match (vault, partition) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(vault), Err(partition)) => Err(format!("{vault}; {partition}")),
    }
}

// --- MUV-4: OAuth / Jottacloud token cutover (explicit-type dual-write) ------
//
// OAuth tokens (`oauth_<provider>_<id>`) and Jottacloud refresh tokens
// (`jottacloud_refresh_<id>`) rewrite themselves on refresh, so the same
// dual-write + reader-fallback net as MUV-3 applies, with one twist: the
// call-sites know the credential type, so they pass it explicitly instead of
// relying on the `server_`-only prefix classifier. Readers use the shared
// `resolve_active_credential` (partition first, vault fallback). The vault stays
// source of truth + fallback during the rollout; no entry is removed (MUV-6).

/// Mirror of an explicitly-typed secret into the active user's partition. For
/// call-sites with bespoke vault-write logic (OAuth runtime, Jottacloud persist)
/// that have already written the vault and only need the partition mirror. No
/// active user -> no-op. A failed mirror is an error ([`mirror_into_partition`]).
pub fn mirror_active_credential(
    store: &CredentialStore,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    match active_user_id_for_write()? {
        Some(user_id) => {
            mirror_credential_for_user_typed(store, user_id, credential_id, credential_type, secret)
        }
        None => Ok(()),
    }
}

/// Removal of one secret from the active user's partition. The partition
/// half of an explicit token delete (OAuth/Jottacloud logout), so a failure,
/// and an account that cannot be read, are returned: the row left behind is
/// the one every reader takes first.
pub fn unmirror_active_credential(credential_id: &str) -> Result<(), String> {
    match active_user_id_for_write()? {
        Some(user_id) => unmirror_deleted_credential(user_id, credential_id),
        None => Ok(()),
    }
}

/// Token types that belong to one user only: OAuth tokens, Jottacloud
/// refresh chains and native Proton Drive sessions.
const PRIVATE_TOKEN_TYPES: [&str; 3] = ["oauth", "jottacloud_refresh", "proton_native_session"];

/// Prefix of a native Proton Drive session saved for one profile, and the key
/// of the one saved before a profile was (the legacy owner's).
const PROTON_SESSION_PREFIX: &str = "proton_native_session_";
const PROTON_UNBOUND_SESSION: &str = "proton_native_session";
/// Prefix of the session an account signed in to before saving the profile.
const PROTON_UNBOUND_USER_PREFIX: &str = "proton_native_unbound_user_";

/// Whether `key` holds a native Proton Drive session.
fn is_proton_session_key(key: &str) -> bool {
    key == PROTON_UNBOUND_SESSION
        || key
            .strip_prefix(PROTON_SESSION_PREFIX)
            .is_some_and(|tail| !tail.is_empty())
        || key
            .strip_prefix(PROTON_UNBOUND_USER_PREFIX)
            .is_some_and(|tail| !tail.is_empty())
}

/// The Proton session UID a saved session names, for telling copies of one
/// session apart without a provider build.
pub(crate) fn proton_session_uid(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw)
        .ok()?
        .get("uid")?
        .as_str()
        .map(str::to_string)
}

/// The providers whose OAuth token lives under `oauth_<slug>` (the singleton)
/// or `oauth_<slug>_<profile id>`.
const OAUTH_TOKEN_SLUGS: [&str; 9] = [
    "google",
    "googlephotos",
    "dropbox",
    "onedrive",
    "box",
    "pcloud",
    "zohoworkdrive",
    "yandexdisk",
    "fourshared",
];

/// The app settings that share the `oauth_<slug>_` prefix with the tokens:
/// machine-wide, never one user's.
const OAUTH_APP_SETTING_SUFFIXES: [&str; 3] = ["client_id", "client_secret", "region"];

/// The machine-global key of a GitHub App's PEM, `github_pem_<app id>_<installation
/// id>` (`github_pem_vault_key`): both are numeric GitHub ids, which a profile id
/// (`srv_<millis>_<random>`) never is, so it is not the `github_pem_<profile>` of a
/// profile.
fn is_github_app_pem_key(credential_id: &str) -> bool {
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    credential_id
        .strip_prefix("github_pem_")
        .and_then(|tail| tail.split_once('_'))
        .is_some_and(|(app, installation)| numeric(app) && numeric(installation))
}

/// The type of [`PRIVATE_TOKEN_TYPES`] a key holds, by the names its writers
/// give it: the one place a reader, writer or delete that knows only the key
/// learns that the key is one user's. An OAuth token (`oauth_<slug>`,
/// `oauth_<slug>_<profile>`, not the app settings next to it), a profile's
/// authorization state that [`crate::profile_auth`] writes as `oauth`
/// (`profile_auth_*`, `github_oauth_<profile>`, `github_pem_<profile>`, not the
/// globals `github_oauth_token`, `github_pat` and the GitHub App keys
/// [`is_github_app_pem_key`]), or a Jottacloud refresh chain. `None` for every
/// other key.
pub fn private_token_type(credential_id: &str) -> Option<&'static str> {
    if credential_id == "jottacloud_refresh" || credential_id.starts_with("jottacloud_refresh_") {
        return Some("jottacloud_refresh");
    }
    if is_proton_session_key(credential_id) {
        return Some("proton_native_session");
    }
    if let Some(rest) = credential_id.strip_prefix("oauth_") {
        let token = OAUTH_TOKEN_SLUGS.iter().any(|slug| {
            rest == *slug
                || rest
                    .strip_prefix(slug)
                    .and_then(|tail| tail.strip_prefix('_'))
                    .is_some_and(|tail| {
                        !tail.is_empty() && !OAUTH_APP_SETTING_SUFFIXES.contains(&tail)
                    })
        });
        return token.then_some("oauth");
    }
    if matches!(credential_id, "github_oauth_token" | "github_pat")
        || is_github_app_pem_key(credential_id)
    {
        return None;
    }
    crate::profile_auth::DURABLE_KEY_PREFIXES
        .iter()
        .chain(crate::profile_auth::TRANSIENT_KEY_PREFIXES.iter())
        .any(|prefix| {
            credential_id
                .strip_prefix(prefix)
                .and_then(|tail| tail.strip_prefix('_'))
                .is_some_and(|tail| !tail.is_empty())
        })
        .then_some("oauth")
}

/// Whether `user_id` is the legacy owner, whose private tokens are also the
/// vault's ([`vault_copy_is_another_accounts`]); answered on a read-only
/// connection too.
pub(crate) fn is_legacy_owner(conn: &Connection, user_id: i64) -> Result<bool, String> {
    Ok(nonprofile_secret_owner_unpersisted(conn)? == Some(user_id))
}

/// What tells account `user_id` apart from every other across databases:
/// its DEK verifier, an HMAC tag of its random DEK. Each database numbers its
/// accounts on its own, so two of them can give one id to two people; every
/// copy of one account's database carries the same verifier (a transport
/// re-key re-wraps the DEK and keeps it), and only an administrator's reset,
/// which wipes the account's secrets, changes it. Read without unlocking.
pub(crate) fn account_identity(conn: &Connection, user_id: i64) -> Result<[u8; 16], String> {
    let verifier: Vec<u8> = conn
        .query_row(
            "SELECT dek_verifier FROM users WHERE id = ?1",
            params![user_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("Read the identity of account {user_id}: {e}"))?;
    verifier
        .try_into()
        .map_err(|_| format!("The identity of account {user_id} is not readable"))
}

/// Whether the vault copy of `credential_id` is someone else's for
/// `user_id`: a private token ([`private_token_type`]) and an account other
/// than the legacy owner, whose key of that name it is. Such an account reads
/// and writes its own partition row only. Reads `conn` only, so a read-only
/// connection does.
pub(crate) fn vault_copy_is_another_accounts(
    conn: &Connection,
    user_id: i64,
    credential_id: &str,
) -> Result<bool, String> {
    Ok(private_token_type(credential_id).is_some()
        && nonprofile_secret_owner_unpersisted(conn)? != Some(user_id))
}

/// The user whose tokens of `credential_type` must stay in its own partition:
/// anyone but the legacy owner. Written to the shared vault, a token would be
/// readable through the legacy owner's fallback. The legacy owner and pre-MU
/// installs keep the dual write; other credential types are unchanged.
pub fn private_token_user(
    user_id: Option<i64>,
    credential_type: &str,
) -> Result<Option<i64>, String> {
    if !PRIVATE_TOKEN_TYPES.contains(&credential_type) {
        return Ok(None);
    }
    Ok(match credential_owner_and_fallback(user_id)? {
        (Some(user), false) => Some(user),
        _ => None,
    })
}

/// Explicitly-typed dual writer (active user): vault first, then mirror into the
/// active user's partition. For import/bridge sites without bespoke vault logic.
pub fn store_active_credential_typed_dual(
    store: &CredentialStore,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    store_active_credential_typed_dual_as(store, credential_id, credential_type, secret, false)
}

/// [`store_active_credential_typed_dual`] for `user_id`, the account active
/// when the request arrived ([`active_user_for_save`]): a writer of several
/// secrets reads the account once, so a switch while one of them waits sends
/// none of the others elsewhere. `None` (no account yet) writes the vault, as
/// the active-account writer does then.
pub fn store_typed_credential_dual_as(
    store: &CredentialStore,
    user_id: Option<i64>,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    match user_id {
        Some(user_id) => store_credential_for_user_typed_dual(
            store,
            user_id,
            credential_id,
            credential_type,
            secret,
        ),
        None => store
            .store(credential_id, secret)
            .map_err(|e| format!("Store credential in vault: {e}")),
    }
}

/// [`store_active_credential_typed_dual`] for a sign-in of the active
/// account's own ([`store_signed_in_credential_for_user`]).
fn store_active_signed_in_credential_typed_dual(
    store: &CredentialStore,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    store_active_credential_typed_dual_as(store, credential_id, credential_type, secret, true)
}

fn store_active_credential_typed_dual_as(
    store: &CredentialStore,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
    signed_in: bool,
) -> Result<(), String> {
    // One lookup for both halves: an account switched to between the vault
    // write and the mirror must not receive this account's token.
    match active_user_id_for_write()? {
        Some(user) => store_credential_for_user_typed_dual_as(
            store,
            user,
            credential_id,
            credential_type,
            secret,
            signed_in,
        ),
        None => store_vault_then_mirror(store, credential_id, secret, || Ok(())),
    }
}

/// Explicitly-typed dual writer (CLI `--user`): vault first, then mirror into
/// `user_id`'s partition.
pub fn store_credential_for_user_typed_dual(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    store_credential_for_user_typed_dual_as(
        store,
        user_id,
        credential_id,
        credential_type,
        secret,
        false,
    )
}

/// [`store_credential_for_user_typed_dual`] for a sign-in of `user_id`'s own
/// ([`store_signed_in_credential_for_user`]).
pub fn store_signed_in_credential_for_user_typed_dual(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    store_credential_for_user_typed_dual_as(
        store,
        user_id,
        credential_id,
        credential_type,
        secret,
        true,
    )
}

fn store_credential_for_user_typed_dual_as(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
    signed_in: bool,
) -> Result<(), String> {
    if let Some(user) = private_token_user(Some(user_id), credential_type)? {
        return store_credential_for_user_as(
            store,
            user,
            credential_id,
            credential_type,
            secret,
            signed_in,
        );
    }
    store_vault_then_mirror(store, credential_id, secret, || {
        mirror_credential_for_user_typed(store, user_id, credential_id, credential_type, secret)
    })
}

/// MU-7: list other users that already store a profile with the SAME dedup
/// signature (HMAC of canonical protocol/host/user/port keyed by the device
/// partition root key). Used by the frontend to warn when a saved profile
/// points at an account another user already has. Excludes the requesting
/// user. Returns only public user metadata, never any portion of the
/// encrypted blob.
pub fn cross_user_dedup_matches(
    conn: &Connection,
    root_key: &[u8; 32],
    requesting_user_id: i64,
    profile: &Value,
) -> Result<Vec<CrossUserDedupMatch>, String> {
    // EF-19(a): an empty S3 access key hashes to a constant dedup_key, so two
    // unrelated keyless S3 accounts would cross-warn. Ambiguous surface → no
    // warning (conservative: this guards the soft warning only; storage
    // aggregation semantics are unchanged, see storage_dedup::dedup_key).
    // Cross-ref: storage_dedup::dedup_key is aggregation-only, never identity.
    let protocol = value_str(profile, &["protocol"]).unwrap_or("ftp");
    let username = value_str(profile, &["username", "user", "email", "account"]).unwrap_or("");
    if protocol == "s3" && username.trim().is_empty() {
        return Ok(Vec::new());
    }

    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let uid_seed = value_str(profile, &["id"]).unwrap_or("");
    let dedup_tag = profile_dedup_key(&root_secret, profile, uid_seed)?;
    let mut stmt = conn
        .prepare(
            "SELECT u.id, u.name, u.avatar_emoji, u.avatar_color
               FROM server_profiles sp
               JOIN users u ON u.id = sp.user_id
              WHERE sp.dedup_key = ?1 AND u.id != ?2
              ORDER BY u.sort_order ASC, u.name_canonical ASC",
        )
        .map_err(|e| format!("Prepare cross-user dedup query: {e}"))?;
    let rows = stmt
        .query_map(params![dedup_tag, requesting_user_id], |row| {
            Ok(CrossUserDedupMatch {
                user_id: row.get(0)?,
                user_name: row.get(1)?,
                user_avatar_emoji: row.get(2)?,
                user_avatar_color: row.get(3)?,
            })
        })
        .map_err(|e| format!("Query cross-user dedup: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("Read cross-user dedup rows: {e}"))
}

pub fn create_user(
    conn: &mut Connection,
    root_key: &[u8; 32],
    name: &str,
    avatar_emoji: Option<&str>,
    avatar_color: Option<&str>,
    passphrase: Option<&str>,
) -> Result<UserMetadata, String> {
    let (display, canonical) = normalize_name(name)?;
    validate_avatar_fields(avatar_emoji, avatar_color)?;
    let sort_order = conn
        .query_row(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM users",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|e| format!("Read next user sort order: {e}"))?;
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let dek = user_crypto::generate_dek();
    let mut kdf_salt: Option<Vec<u8>> = None;
    let mut kdf_params: Option<String> = None;
    let wrapping_key = if let Some(passphrase) = passphrase {
        let salt = user_crypto::random_salt();
        let params = default_kdf_params();
        kdf_salt = Some(salt.to_vec());
        kdf_params = Some(user_crypto::params_to_json(&params)?);
        user_crypto::derive_wrapping_key(passphrase, &salt, &params)?
    } else {
        root_secret
    };
    let has_passphrase = passphrase.is_some();
    let wrapped_dek = user_crypto::wrap_dek(&wrapping_key, &dek)?;
    let verifier = user_crypto::compute_dek_verifier(&dek)?.to_vec();
    let tx = conn
        .transaction()
        .map_err(|e| format!("Start create user: {e}"))?;
    let now = now_ms();
    tx.execute(
        "INSERT INTO users(
             name, name_canonical, avatar_emoji, avatar_color, has_passphrase,
             kdf_salt, kdf_params, wrapped_dek, dek_verifier, sort_order,
             created_at, updated_at, last_unlocked_at
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?12)",
        params![
            display,
            canonical,
            avatar_emoji,
            avatar_color,
            if has_passphrase { 1 } else { 0 },
            kdf_salt,
            kdf_params,
            wrapped_dek,
            verifier,
            sort_order,
            now,
            if has_passphrase { None } else { Some(now) }
        ],
    )
    .map_err(|e| format!("Create user: {e}"))?;
    let id = tx.last_insert_rowid();
    tx.commit()
        .map_err(|e| format!("Commit create user: {e}"))?;

    let active = active_user_id(conn)?;
    Ok(UserMetadata {
        id,
        name: display,
        avatar_emoji: avatar_emoji.map(ToOwned::to_owned),
        avatar_color: avatar_color.map(ToOwned::to_owned),
        has_passphrase,
        sort_order,
        created_at: now,
        updated_at: now,
        last_unlocked_at: if has_passphrase { None } else { Some(now) },
        is_active: active == Some(id),
        // New users created via Manage Users are NEVER admin by default.
        // Promotion to admin is an explicit action via
        // user_partitions_set_admin, gated on the caller being admin.
        is_admin: false,
        // A freshly created user is never the default/favourite account; the
        // flag is set on demand via user_partitions_set_default_user.
        is_default: false,
    })
}

pub fn create_passphrase_less_user(
    conn: &mut Connection,
    root_key: &[u8; 32],
    name: &str,
    avatar_emoji: Option<&str>,
    avatar_color: Option<&str>,
) -> Result<UserMetadata, String> {
    create_user(conn, root_key, name, avatar_emoji, avatar_color, None)
}

pub fn unlock_user(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    passphrase: Option<&str>,
) -> Result<UserUnlockStatus, String> {
    unlock_user_inner(
        conn, root_key, user_id, passphrase, /*promote_to_active=*/ true,
    )
}

/// Transient unlock: validates the passphrase and primes the DEK session,
/// but does NOT touch `active_user_id`. Used by the CLI `--user` flag so a
/// per-invocation override never silently switches the persistent active
/// user (mirrors `aws --profile X` / `kubectl --context X` semantics).
pub fn unlock_user_transient(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    passphrase: Option<&str>,
) -> Result<UserUnlockStatus, String> {
    unlock_user_inner(
        conn, root_key, user_id, passphrase, /*promote_to_active=*/ false,
    )
}

fn unlock_user_inner(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    passphrase: Option<&str>,
    promote_to_active: bool,
) -> Result<UserUnlockStatus, String> {
    // Promoting another account is a switch ([`set_active_user`]).
    let _lock = if promote_to_active && active_user_id(conn)? != Some(user_id) {
        ProfileListLock::acquire(conn)?
    } else {
        None
    };
    let row = read_user_key_row(conn, user_id)?;
    let root_secret = user_crypto::secret_key_from_bytes(root_key);

    let dek = if row.has_passphrase {
        check_lockout(conn, user_id)?;
        let passphrase = passphrase.ok_or_else(|| "PASSPHRASE_REQUIRED".to_string())?;
        match unwrap_user_dek_with_passphrase(&row, passphrase) {
            Ok(dek) => {
                reset_lockout(conn, user_id)?;
                dek
            }
            Err(_) => {
                record_unlock_failure(conn, user_id)?;
                return Err("WRONG_PASSPHRASE".to_string());
            }
        }
    } else {
        if passphrase.is_some() {
            return Err("PASSPHRASE_NOT_NEEDED".to_string());
        }
        clear_user_session();
        unwrap_user_dek_with_key(&row, &root_secret)?
    };

    if promote_to_active {
        set_active_user_row(conn, user_id)?;
    }
    let security_reset =
        if security_reset_pending(conn, user_id)? || security_reset_blocked(conn, user_id) {
            let _reset_lock = if _lock.is_none() {
                Some(ProfileListLock::acquire(conn)?)
            } else {
                None
            };
            complete_security_reset_with_dek(conn, &root_secret, user_id, &dek)?
        } else {
            Vec::new()
        };
    // The account's sessions saved before the index, or rewritten without
    // it, are indexed now that its key is at hand.
    #[cfg(feature = "proton-native")]
    if let Err(error) = index_account_sessions(conn, root_key, user_id, &dek) {
        log::warn!("Unlock: the account's Proton sessions were not indexed: {error}");
    }
    if row.has_passphrase {
        set_user_session(user_id, dek)?;
    }
    // Once the account reads its partition, the sessions a purge could not
    // end while it was locked are ended, for a bounded time: offline, each
    // waits for Proton, and the unlock is not held for that.
    #[cfg(feature = "proton-native")]
    end_deferred_proton_sessions_within_budget(root_key, user_id);
    let mut status = user_unlock_status(conn)?;
    status.security_reset = security_reset;
    Ok(status)
}

/// The caller holds the profile-list lock (or owns a private staged database)
/// and has verified this DEK. The pending marker leaves only with a committed reset.
fn complete_security_reset_with_dek(
    conn: &Connection,
    root_secret: &SecretKey,
    user_id: i64,
    dek: &SecretKey,
) -> Result<Vec<crate::profile_security::ProfileReset>, String> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| format!("Begin security reset: {e}"))?;
    let result = (|| {
        let mut profiles = read_profiles_with_dek(conn, user_id, dek)?;
        let reset = crate::profile_security::make_profile_list_safe(&mut profiles);
        if !reset.is_empty() {
            insert_profiles_with_dek(conn, root_secret, user_id, dek, &profiles)?;
        }
        conn.execute(
            "DELETE FROM global_state WHERE key = ?1",
            params![security_reset_pending_key(user_id)],
        )
        .map_err(|e| format!("Clear pending security reset: {e}"))?;
        Ok(reset)
    })();
    match result {
        Ok(reset) => {
            if let Err(e) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(format!("Commit security reset: {e}"));
            }
            unblock_security_reset(conn, user_id);
            Ok(reset)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Complete the staged active account's pending reset only when this process
/// already holds its verified session key. Other restored accounts stay pending.
pub(crate) fn finish_active_profile_reset_in_db(
    store: &CredentialStore,
    db_path: &std::path::Path,
) -> Result<Option<Vec<crate::profile_security::ProfileReset>>, String> {
    let conn = open_restored_db(db_path)?;
    let Some(user_id) = active_user_id(&conn)? else {
        return Ok(None);
    };
    let row = read_user_key_row(&conn, user_id)?;
    if !row.has_passphrase || !security_reset_pending(&conn, user_id)? {
        return Ok(None);
    }
    let _session = hold_session()?;
    HELD_SESSION.with_borrow(|held| {
        let Some((session_user, dek)) = held.as_ref().and_then(Option::as_ref) else {
            return Ok(None);
        };
        if *session_user != user_id || !user_crypto::verify_dek(dek, &row.dek_verifier)? {
            return Ok(None);
        }
        let mut root_key = store.derive_user_partition_wrapping_key();
        let root_secret = user_crypto::secret_key_from_bytes(&root_key);
        root_key.zeroize();
        complete_security_reset_with_dek(&conn, &root_secret, user_id, dek).map(Some)
    })
}

pub fn verify_user_passphrase(
    conn: &Connection,
    _root_key: &[u8; 32],
    user_id: i64,
    passphrase: Option<&str>,
) -> Result<(), String> {
    let row = read_user_key_row(conn, user_id)?;
    if !row.has_passphrase {
        if passphrase.is_some() {
            return Err("PASSPHRASE_NOT_NEEDED".to_string());
        }
        return Ok(());
    }

    check_lockout(conn, user_id)?;
    let passphrase = passphrase.ok_or_else(|| "PASSPHRASE_REQUIRED".to_string())?;
    match unwrap_user_dek_with_passphrase(&row, passphrase) {
        Ok(_) => {
            reset_lockout(conn, user_id)?;
            Ok(())
        }
        Err(_) => {
            record_unlock_failure(conn, user_id)?;
            Err("WRONG_PASSPHRASE".to_string())
        }
    }
}

pub fn change_user_passphrase(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    old_passphrase: Option<&str>,
    new_passphrase: Option<&str>,
) -> Result<(), String> {
    let row = read_user_key_row(conn, user_id)?;
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let dek = if row.has_passphrase {
        check_lockout(conn, user_id)?;
        let old = old_passphrase.ok_or_else(|| "PASSPHRASE_REQUIRED".to_string())?;
        match unwrap_user_dek_with_passphrase(&row, old) {
            Ok(dek) => {
                reset_lockout(conn, user_id)?;
                dek
            }
            Err(_) => {
                record_unlock_failure(conn, user_id)?;
                return Err("WRONG_PASSPHRASE".to_string());
            }
        }
    } else {
        if old_passphrase.is_some() {
            return Err("PASSPHRASE_NOT_NEEDED".to_string());
        }
        unwrap_user_dek_with_key(&row, &root_secret)?
    };

    let mut kdf_salt: Option<Vec<u8>> = None;
    let mut kdf_params: Option<String> = None;
    let wrapping_key = if let Some(new_passphrase) = new_passphrase {
        let salt = user_crypto::random_salt();
        let params = default_kdf_params();
        kdf_salt = Some(salt.to_vec());
        kdf_params = Some(user_crypto::params_to_json(&params)?);
        user_crypto::derive_wrapping_key(new_passphrase, &salt, &params)?
    } else {
        root_secret
    };
    let has_passphrase = new_passphrase.is_some();
    let wrapped_dek = user_crypto::wrap_dek(&wrapping_key, &dek)?;
    conn.execute(
        // The default account opens without a prompt: one given a passphrase
        // stops being the default.
        "UPDATE users
         SET has_passphrase = ?1, kdf_salt = ?2, kdf_params = ?3,
             wrapped_dek = ?4, updated_at = ?5,
             is_default = CASE WHEN ?1 = 1 THEN 0 ELSE is_default END
         WHERE id = ?6",
        params![
            if has_passphrase { 1 } else { 0 },
            kdf_salt,
            kdf_params,
            wrapped_dek,
            now_ms(),
            user_id
        ],
    )
    .map_err(|e| format!("Change user passphrase: {e}"))?;

    if active_user_id(conn)? == Some(user_id) {
        if has_passphrase {
            set_user_session(user_id, dek)?;
        } else {
            clear_user_session();
        }
    }
    Ok(())
}

// ============ Keystore portability (F-012) ============
//
// A passphrase-less user partition wraps its DEK under the machine-bound
// `root_key` = HKDF(per-machine vault_key). A `user_partitions.db` carried to
// another machine in a keystore backup therefore decrypts to nothing there:
// the destination's `vault_key` differs, so AES-KW unwrap fails the integrity
// check ("Unwrap user data key: integrity check failed") and every profile
// under that DEK is unreadable. Passphrase-protected partitions are already
// portable (wrapped under Argon2id(passphrase)); only passphrase-less ones
// need help.
//
// The fix carries, alongside the backup, each passphrase-less user's DEK
// re-wrapped under a *transport key* derived from the backup password. On
// import the destination unwraps with that transport key and re-wraps under
// its own local `root_key`, so the restored partition becomes readable on the
// new machine without ever moving the raw `vault_key` between machines.

/// Transport-wrapped DEKs for passphrase-less partitions, produced at export.
pub struct TransportExport {
    /// Argon2id salt for the transport wrapping key (raw bytes).
    pub salt: Vec<u8>,
    /// Argon2id params JSON for the transport wrapping key.
    pub kdf_params: String,
    /// `user_id` -> AES-KW(transport_key, DEK) for each passphrase-less user.
    pub wrapped_deks: HashMap<i64, Vec<u8>>,
}

/// Outcome of the import-side re-key pass, surfaced in the import summary.
#[derive(Debug, Default, Clone, Copy)]
pub struct TransportRekeyReport {
    /// Passphrase-less partitions re-keyed from the transport key to this
    /// machine's local `root_key` (now readable here).
    pub rekeyed: u32,
    /// Passphrase-less partitions already readable with the local `root_key`
    /// (same-machine re-import); left untouched.
    pub already_local: u32,
    /// Passphrase-less partitions still unreadable here: imported from another
    /// machine with no usable transport DEK (e.g. an old backup). The UI
    /// should advise re-exporting from the source with a passphrase set.
    pub unreadable: u32,
    /// Passphrase-protected partitions (already portable; nothing to do).
    pub passphrase_protected: u32,
}

/// Build the transport-wrapped DEKs for every passphrase-less user, so a
/// keystore backup of `user_partitions.db` is portable across machines.
///
/// `root_key` is THIS machine's local wrapping key (the export runs on the
/// source machine where the partitions are readable). `password` is the
/// backup password; the transport key is `Argon2id(password, salt)`.
///
/// MUV-5 / R-MUV-3: this same transported DEK makes the per-user secrets
/// portable, not just the profile metadata. The `user_credentials` rows ride
/// along inside the bundled `user_partitions.db` and are encrypted under the
/// user's DEK (`set_user_credential_with_dek` -> `encrypt_blob(dek, ...)`), the
/// exact key this sidecar carries. Once [`rekey_transport_deks`] re-wraps the
/// DEK to the destination's local `root_key`, the credential rows decrypt with
/// no per-row change. No secret-specific wrapping is needed: the DEK is the
/// single confidentiality boundary for both profiles and secrets. (Covered by
/// `user_credentials_row_is_portable_via_transport_dek`.)
///
/// Returns `Ok(None)` when there is no passphrase-less user to make portable
/// (so the caller omits the section entirely and old readers stay happy).
pub fn export_transport_deks(
    conn: &Connection,
    root_key: &[u8; 32],
    password: &str,
) -> Result<Option<TransportExport>, String> {
    let users = list_users(conn)?;
    if !users.iter().any(|u| !u.has_passphrase) {
        return Ok(None);
    }

    let salt = user_crypto::random_salt();
    let params = default_kdf_params();
    let transport_key = user_crypto::derive_wrapping_key(password, &salt, &params)?;
    let root_secret = user_crypto::secret_key_from_bytes(root_key);

    let mut wrapped_deks: HashMap<i64, Vec<u8>> = HashMap::new();
    for user in &users {
        if user.has_passphrase {
            continue;
        }
        let row = read_user_key_row(conn, user.id)?;
        let dek = unwrap_user_dek_with_key(&row, &root_secret)?;
        let wrapped = user_crypto::wrap_dek(&transport_key, &dek)?;
        wrapped_deks.insert(user.id, wrapped);
    }

    Ok(Some(TransportExport {
        salt: salt.to_vec(),
        kdf_params: user_crypto::params_to_json(&params)?,
        wrapped_deks,
    }))
}

/// Re-key imported passphrase-less partitions to THIS machine's local
/// `root_key`, using the transport-wrapped DEKs produced by
/// [`export_transport_deks`]. Run after a keystore restore overwrote
/// `user_partitions.db` with the source machine's copy.
///
/// Rows already readable with the local `root_key` (same-machine re-import)
/// are left untouched. Rows with no usable transport DEK are counted as
/// unreadable and left as-is (no data is destroyed; the UI warns the user).
/// `transport_wrapped` empty (e.g. an old backup with no transport section)
/// degrades cleanly to detect-and-report with no re-keying.
pub fn rekey_transport_deks(
    conn: &Connection,
    local_root_key: &[u8; 32],
    password: &str,
    salt: &[u8],
    kdf_params: &str,
    transport_wrapped: &HashMap<i64, Vec<u8>>,
) -> Result<TransportRekeyReport, String> {
    let local_secret = user_crypto::secret_key_from_bytes(local_root_key);
    let mut transport_key: Option<SecretKey> = None;
    let mut report = TransportRekeyReport::default();

    for user in list_users(conn)? {
        if user.has_passphrase {
            report.passphrase_protected += 1;
            continue;
        }
        let row = read_user_key_row(conn, user.id)?;
        // Already readable on this machine (same-machine re-import)?
        if unwrap_user_dek_with_key(&row, &local_secret).is_ok() {
            report.already_local += 1;
            continue;
        }
        // Needs re-keying; do we have a transport DEK for this user?
        let Some(blob) = transport_wrapped.get(&user.id) else {
            report.unreadable += 1;
            continue;
        };
        // Derive the (expensive) transport key once, lazily.
        if transport_key.is_none() {
            let params = user_crypto::params_from_json(kdf_params)?;
            let salt16: [u8; 16] = salt
                .try_into()
                .map_err(|_| "INVALID_TRANSPORT_SALT_SIZE".to_string())?;
            transport_key = Some(user_crypto::derive_wrapping_key(
                password, &salt16, &params,
            )?);
        }
        let tk = transport_key.as_ref().expect("transport key derived above");
        let dek = match user_crypto::unwrap_dek(tk, blob) {
            Ok(dek) => dek,
            Err(_) => {
                report.unreadable += 1;
                continue;
            }
        };
        // The transport DEK must match the row's verifier before we commit it.
        if !user_crypto::verify_dek(&dek, &row.dek_verifier)? {
            report.unreadable += 1;
            continue;
        }
        let rewrapped = user_crypto::wrap_dek(&local_secret, &dek)?;
        conn.execute(
            "UPDATE users
             SET wrapped_dek = ?1, updated_at = ?2
             WHERE id = ?3 AND has_passphrase = 0",
            params![rewrapped, now_ms(), user.id],
        )
        .map_err(|e| format!("Re-key imported user DEK: {e}"))?;
        report.rekeyed += 1;
    }

    Ok(report)
}

pub fn set_active_user(conn: &Connection, user_id: i64) -> Result<(), String> {
    // A switch must not land between an import's account check and its writes.
    let _lock = ProfileListLock::acquire(conn)?;
    clear_user_session();
    set_active_user_row(conn, user_id)
}

pub fn is_admin_user(conn: &Connection, user_id: i64) -> Result<bool, String> {
    let value: Option<i64> = conn
        .query_row(
            "SELECT is_admin FROM users WHERE id = ?1",
            params![user_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("Read user is_admin: {e}"))?;
    Ok(value.unwrap_or(0) != 0)
}

pub fn count_admin_users(conn: &Connection) -> Result<i64, String> {
    conn.query_row("SELECT COUNT(*) FROM users WHERE is_admin = 1", [], |row| {
        row.get(0)
    })
    .map_err(|e| format!("Count admin users: {e}"))
}

/// Self-only-or-admin gate. CMS/CRM-grade rule: the only writer that
/// may touch a user's mutable account fields (name, passphrase, avatar,
/// destructive delete) is the user themselves OR an admin acting on
/// their behalf. Anyone else (a peer with the vault unlocked AS a
/// non-admin account) is rejected with NOT_AUTHORIZED.
///
/// Returns Ok(true) when the caller is the target itself, Ok(false)
/// when the caller is an admin acting on a peer, and an Err otherwise.
/// Callers that need to forbid admin-on-peer (e.g. self-only changes
/// such as password rotation that require knowing the current
/// passphrase) check the boolean and reject explicitly.
pub fn ensure_user_can_modify(conn: &Connection, target_user_id: i64) -> Result<bool, String> {
    let status = user_unlock_status(conn)?;
    let Some(actor_id) = status.unlocked_user_id else {
        return Err("VAULT_LOCKED".to_string());
    };
    if actor_id == target_user_id {
        return Ok(true);
    }
    if is_admin_user(conn, actor_id)? {
        return Ok(false);
    }
    Err("NOT_AUTHORIZED".to_string())
}

/// Grant or revoke admin privileges. Only an admin may call. The
/// system must always retain at least one admin: revoking the last
/// admin returns CANNOT_DEMOTE_LAST_ADMIN.
pub fn set_user_admin(
    conn: &mut Connection,
    target_user_id: i64,
    is_admin: bool,
) -> Result<(), String> {
    let status = user_unlock_status(conn)?;
    let actor_id = status
        .unlocked_user_id
        .ok_or_else(|| "VAULT_LOCKED".to_string())?;
    if !is_admin_user(conn, actor_id)? {
        return Err("NOT_AUTHORIZED".to_string());
    }
    let current = is_admin_user(conn, target_user_id)?;
    if current == is_admin {
        return Ok(());
    }
    if !is_admin && count_admin_users(conn)? <= 1 {
        return Err("CANNOT_DEMOTE_LAST_ADMIN".to_string());
    }
    let changed = conn
        .execute(
            "UPDATE users SET is_admin = ?1, updated_at = ?2 WHERE id = ?3",
            params![if is_admin { 1 } else { 0 }, now_ms(), target_user_id],
        )
        .map_err(|e| format!("Update user is_admin: {e}"))?;
    if changed == 0 {
        return Err("USER_NOT_FOUND".to_string());
    }
    Ok(())
}

/// Admin-only destructive password recovery. The crypto reality: a
/// passphrase-protected user's DEK is wrapped with Argon2id(passphrase)
/// and nothing else, so without the passphrase no one (not even admin)
/// can decrypt that user's encrypted blobs. The only recovery path is
/// to wipe the target's DEK + server_profiles + user_settings +
/// user_credentials + lockout state and issue a brand-new DEK wrapped by the new
/// passphrase. The frontend MUST present this as a destructive
/// operation with a triple confirmation, listing the byte count that
/// will be lost. Returns USER_NOT_FOUND if the target does not exist
/// and NOT_AUTHORIZED if the caller is not an admin.
pub fn admin_reset_user_passphrase(
    conn: &mut Connection,
    root_key: &[u8; 32],
    target_user_id: i64,
    new_passphrase: Option<&str>,
) -> Result<(), String> {
    let _lock = ProfileListLock::acquire(conn)?;
    let status = user_unlock_status(conn)?;
    let actor_id = status
        .unlocked_user_id
        .ok_or_else(|| "VAULT_LOCKED".to_string())?;
    if !is_admin_user(conn, actor_id)? {
        return Err("NOT_AUTHORIZED".to_string());
    }
    if actor_id == target_user_id {
        // Admin acting on self uses the regular change_user_passphrase
        // flow, which preserves their data. Reject here to keep the
        // destructive path from being used by mistake against self.
        return Err("ADMIN_RESET_NOT_FOR_SELF".to_string());
    }
    // Generate the new DEK + wrapping key OUTSIDE the transaction so a
    // KDF failure does not leave the partition half-wiped.
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let new_dek = user_crypto::generate_dek();
    let mut kdf_salt: Option<Vec<u8>> = None;
    let mut kdf_params: Option<String> = None;
    let wrapping_key = if let Some(new_passphrase) = new_passphrase {
        let salt = user_crypto::random_salt();
        let params = default_kdf_params();
        kdf_salt = Some(salt.to_vec());
        kdf_params = Some(user_crypto::params_to_json(&params)?);
        user_crypto::derive_wrapping_key(new_passphrase, &salt, &params)?
    } else {
        root_secret
    };
    let has_passphrase = new_passphrase.is_some();
    let wrapped_dek = user_crypto::wrap_dek(&wrapping_key, &new_dek)?;
    let verifier = user_crypto::compute_dek_verifier(&new_dek)?.to_vec();
    let tx = conn
        .transaction()
        .map_err(|e| format!("Start admin reset passphrase: {e}"))?;
    // Wipe encrypted partition data (the new DEK cannot decrypt them).
    tx.execute(
        "DELETE FROM server_profiles WHERE user_id = ?1",
        params![target_user_id],
    )
    .map_err(|e| format!("Wipe target server_profiles: {e}"))?;
    tx.execute(
        "DELETE FROM user_settings WHERE user_id = ?1",
        params![target_user_id],
    )
    .map_err(|e| format!("Wipe target user_settings: {e}"))?;
    // Its credential rows too: the new DEK cannot read them, and a row left
    // would keep its session index entry and sign-in stamp (their triggers
    // drop both with the row), so another account's sign-out of the same
    // Proton session would count it as a copy still held and not revoke.
    tx.execute(
        "DELETE FROM user_credentials WHERE user_id = ?1",
        params![target_user_id],
    )
    .map_err(|e| format!("Wipe target user_credentials: {e}"))?;
    tx.execute(
        "DELETE FROM global_state WHERE key = ?1",
        params![lockout_key(target_user_id)],
    )
    .map_err(|e| format!("Wipe target lockout: {e}"))?;
    let changed = tx
        .execute(
            "UPDATE users
             SET has_passphrase = ?1, kdf_salt = ?2, kdf_params = ?3,
                 wrapped_dek = ?4, dek_verifier = ?5, last_unlocked_at = NULL,
                 updated_at = ?6,
                 is_default = CASE WHEN ?1 = 1 THEN 0 ELSE is_default END
             WHERE id = ?7",
            params![
                if has_passphrase { 1 } else { 0 },
                kdf_salt,
                kdf_params,
                wrapped_dek,
                verifier,
                now_ms(),
                target_user_id
            ],
        )
        .map_err(|e| format!("Admin reset passphrase: {e}"))?;
    if changed == 0 {
        return Err("USER_NOT_FOUND".to_string());
    }
    tx.commit()
        .map_err(|e| format!("Commit admin reset passphrase: {e}"))?;
    Ok(())
}

pub fn rename_user(conn: &Connection, user_id: i64, name: &str) -> Result<(), String> {
    let (display, canonical) = normalize_name(name)?;
    let changed = conn
        .execute(
            "UPDATE users
             SET name = ?1, name_canonical = ?2, updated_at = ?3
             WHERE id = ?4",
            params![display, canonical, now_ms(), user_id],
        )
        .map_err(|e| format!("Rename user: {e}"))?;
    if changed == 0 {
        return Err("USER_NOT_FOUND".to_string());
    }
    Ok(())
}

pub fn set_user_avatar(
    conn: &Connection,
    user_id: i64,
    avatar_emoji: Option<&str>,
    avatar_color: Option<&str>,
) -> Result<(), String> {
    validate_avatar_fields(avatar_emoji, avatar_color)?;
    let changed = conn
        .execute(
            "UPDATE users
             SET avatar_emoji = ?1, avatar_color = ?2, updated_at = ?3
             WHERE id = ?4",
            params![avatar_emoji, avatar_color, now_ms(), user_id],
        )
        .map_err(|e| format!("Set user avatar: {e}"))?;
    if changed == 0 {
        return Err("USER_NOT_FOUND".to_string());
    }
    Ok(())
}

pub fn reorder_users(conn: &mut Connection, user_ids: &[i64]) -> Result<(), String> {
    let tx = conn
        .transaction()
        .map_err(|e| format!("Start reorder users: {e}"))?;
    let now = now_ms();
    let mut seen = HashSet::new();
    for (index, user_id) in user_ids.iter().enumerate() {
        if !seen.insert(*user_id) {
            return Err("DUPLICATE_USER_ID".to_string());
        }
        let changed = tx
            .execute(
                "UPDATE users
                 SET sort_order = ?1, updated_at = ?2
                 WHERE id = ?3",
                params![index as i64, now, user_id],
            )
            .map_err(|e| format!("Reorder users: {e}"))?;
        if changed == 0 {
            return Err("USER_NOT_FOUND".to_string());
        }
    }
    tx.commit()
        .map_err(|e| format!("Commit reorder users: {e}"))?;
    Ok(())
}

/// Set or clear the default / favourite user (the account auto-unlocked on
/// launch). Single-winner: when `make_default` is true the flag is cleared on
/// every other user first, so at most one default exists; when false only the
/// target row is cleared. The default is the auto-unlock account, so it must be
/// password-free (a protected account always shows its prompt): marking a
/// passphrase-protected user returns `DEFAULT_REQUIRES_NO_PASSPHRASE`. Shared by
/// the GUI Manage Users star and the CLI `users -i` Fav verb. Per Ehud #311.
pub fn set_default_user(
    conn: &mut Connection,
    user_id: i64,
    make_default: bool,
) -> Result<(), String> {
    let now = now_ms();
    // The passphrase check and the flag share one write transaction, so a
    // passphrase added meanwhile lands before it (and the flag is refused) or
    // after it (and its write clears the flag).
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Start set default user: {e}"))?;
    if make_default {
        let has_passphrase: Option<i64> = tx
            .query_row(
                "SELECT has_passphrase FROM users WHERE id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Read user has_passphrase: {e}"))?;
        match has_passphrase {
            None => return Err("USER_NOT_FOUND".to_string()),
            Some(flag) if flag != 0 => return Err("DEFAULT_REQUIRES_NO_PASSPHRASE".to_string()),
            Some(_) => {}
        }
        // Single-winner: drop the flag everywhere, then stamp the target.
        tx.execute(
            "UPDATE users SET is_default = 0, updated_at = ?1 WHERE is_default = 1",
            params![now],
        )
        .map_err(|e| format!("Clear previous default user: {e}"))?;
        let changed = tx
            .execute(
                "UPDATE users SET is_default = 1, updated_at = ?1 WHERE id = ?2",
                params![now, user_id],
            )
            .map_err(|e| format!("Set default user: {e}"))?;
        if changed == 0 {
            return Err("USER_NOT_FOUND".to_string());
        }
    } else {
        let changed = tx
            .execute(
                "UPDATE users SET is_default = 0, updated_at = ?1 WHERE id = ?2",
                params![now, user_id],
            )
            .map_err(|e| format!("Clear default user: {e}"))?;
        if changed == 0 {
            return Err("USER_NOT_FOUND".to_string());
        }
    }
    tx.commit()
        .map_err(|e| format!("Commit set default user: {e}"))?;
    Ok(())
}

pub fn delete_user(conn: &mut Connection, user_id: i64) -> Result<(), String> {
    let _lock = ProfileListLock::acquire(conn)?;
    if active_user_id(conn)? == Some(user_id) {
        clear_user_session();
    }
    let total_users: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
        .map_err(|e| format!("Count users: {e}"))?;
    if total_users <= 1 {
        return Err("CANNOT_DELETE_LAST_USER".to_string());
    }
    // Last-admin protection: even if more than one user exists, never
    // leave the install without an admin. The frontend should disable
    // the delete button on the last admin row, but the backend keeps
    // the guard regardless.
    if is_admin_user(conn, user_id)? && count_admin_users(conn)? <= 1 {
        return Err("CANNOT_DELETE_LAST_ADMIN".to_string());
    }
    let active_before_delete = active_user_id(conn)?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("Start delete user: {e}"))?;
    let changed = tx
        .execute("DELETE FROM users WHERE id = ?1", params![user_id])
        .map_err(|e| format!("Delete user: {e}"))?;
    if changed == 0 {
        return Err("USER_NOT_FOUND".to_string());
    }
    if active_before_delete == Some(user_id) {
        let now = now_ms();
        let active_after_delete: Option<i64> = tx
            .query_row(
                "SELECT id FROM users ORDER BY sort_order ASC, name_canonical ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Choose replacement active user: {e}"))?;
        if let Some(active_id) = active_after_delete {
            upsert_global_state(&tx, ACTIVE_USER_KEY, &active_id.to_string(), now)?;
        }
    }
    tx.commit()
        .map_err(|e| format!("Commit delete user: {e}"))?;
    Ok(())
}

pub fn user_storage_stats(conn: &Connection) -> Result<Vec<UserStorageStats>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT u.id,
                    (SELECT COUNT(*)
                       FROM server_profiles sp
                      WHERE sp.user_id = u.id) AS profile_count,
                    (SELECT COUNT(*)
                       FROM user_settings us
                      WHERE us.user_id = u.id) AS settings_count,
                    COALESCE((SELECT SUM(LENGTH(sp.encrypted_blob) + LENGTH(sp.nonce))
                                FROM server_profiles sp
                               WHERE sp.user_id = u.id), 0)
                    +
                    COALESCE((SELECT SUM(LENGTH(us.encrypted_blob) + LENGTH(us.nonce))
                                FROM user_settings us
                               WHERE us.user_id = u.id), 0) AS encrypted_bytes
               FROM users u
              ORDER BY u.sort_order ASC, u.name_canonical ASC",
        )
        .map_err(|e| format!("Prepare user storage stats: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(UserStorageStats {
                user_id: row.get(0)?,
                profile_count: row.get(1)?,
                settings_count: row.get(2)?,
                encrypted_bytes: row.get(3)?,
            })
        })
        .map_err(|e| format!("Query user storage stats: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("Read user storage stats: {e}"))
}

/// MUV-2: best-effort eager credential migration on the CLI side. The CLI has
/// the store in hand (no Master-Password gate), so it just runs when there is
/// eager-pending work. Never fails the caller.
fn run_eager_credential_migration_cli(conn: &Connection, store: &CredentialStore) {
    if !has_eager_pending_users(conn).unwrap_or(false) {
        return;
    }
    let mut root_key = store.derive_user_partition_wrapping_key();
    let _ = migrate_credentials_eager_all(conn, store, &root_key);
    root_key.zeroize();
}

/// This and the GUI's [`init_or_migrate`] are the writers of profile rows that do
/// not take [`ProfileListLock`], through the legacy-payload migration of a
/// database without a schema yet: the secret writes of an import call this while
/// the import holds the lock, which is not re-entrant. That migration runs in its
/// own IMMEDIATE transaction and moves the revision, so an import in progress
/// would plan again; on a database with a schema it writes no profile row, and
/// neither do the schema upgrades.
pub fn init_or_migrate_cli(store: &CredentialStore) -> Result<MigrationReport, String> {
    let mut conn = open_or_init_cli()?;
    // Same cascade as the GUI path (`init_or_migrate`): bring an existing
    // database forward (v2 -> v3 -> v4) before falling through to the
    // legacy-payload migration that a v1/fresh database needs.
    let report = if apply_pending_upgrades(&mut conn)? {
        already_migrated_report()
    } else {
        let profiles = store.get("config_server_profiles").ok();
        let settings = store
            .get("config_app_settings")
            .ok()
            .or_else(|| store.get("aeroftp_settings").ok());
        let mut root_key = store.derive_user_partition_wrapping_key();
        let result = migrate_legacy_payloads(
            &mut conn,
            profiles.as_deref(),
            settings.as_deref(),
            &root_key,
        );
        root_key.zeroize();
        result?
    };
    // MUV-2: eager credential copy for device-wrapped users (no-op once done).
    run_eager_credential_migration_cli(&conn, store);
    Ok(report)
}

pub fn cli_list_users(store: &CredentialStore) -> Result<Vec<UserMetadata>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    list_users(&conn)
}

pub fn cli_get_active_user(store: &CredentialStore) -> Result<Option<UserMetadata>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    get_active_user(&conn)
}

pub fn cli_create_user(
    store: &CredentialStore,
    name: &str,
    avatar_emoji: Option<&str>,
    avatar_color: Option<&str>,
    passphrase: Option<&str>,
) -> Result<UserMetadata, String> {
    init_or_migrate_cli(store)?;
    let mut conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = create_user(
        &mut conn,
        &root_key,
        name,
        avatar_emoji,
        avatar_color,
        passphrase,
    );
    root_key.zeroize();
    result
}

pub fn cli_unlock_user(
    store: &CredentialStore,
    user_id: i64,
    passphrase: Option<&str>,
) -> Result<UserUnlockStatus, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = unlock_user(&conn, &root_key, user_id, passphrase);
    // MUV-2 lazy: a passphrase account's DEK is now primed, so migrate its
    // legacy secrets. Best-effort: a failure must not undo a good unlock.
    if result.is_ok() {
        let _ = migrate_credentials_for_user(&conn, store, &root_key, user_id);
    }
    root_key.zeroize();
    result
}

/// Transient unlock for CLI `--user` flag: primes the DEK session without
/// promoting the target user to active (`active_user_id` stays untouched).
pub fn cli_unlock_user_transient(
    store: &CredentialStore,
    user_id: i64,
    passphrase: Option<&str>,
) -> Result<UserUnlockStatus, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = unlock_user_transient(&conn, &root_key, user_id, passphrase);
    if result.is_ok() {
        let _ = migrate_credentials_for_user(&conn, store, &root_key, user_id);
    }
    root_key.zeroize();
    result
}

pub fn cli_verify_user_passphrase(
    store: &CredentialStore,
    user_id: i64,
    passphrase: Option<&str>,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = verify_user_passphrase(&conn, &root_key, user_id, passphrase);
    root_key.zeroize();
    result
}

pub fn cli_change_user_passphrase(
    store: &CredentialStore,
    user_id: i64,
    old_passphrase: Option<&str>,
    new_passphrase: Option<&str>,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = change_user_passphrase(&conn, &root_key, user_id, old_passphrase, new_passphrase);
    root_key.zeroize();
    result
}

pub fn cli_set_active_user(store: &CredentialStore, user_id: i64) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    set_active_user(&conn, user_id)
}

pub fn cli_rename_user(store: &CredentialStore, user_id: i64, name: &str) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    rename_user(&conn, user_id, name)
}

pub fn cli_reorder_users(store: &CredentialStore, user_ids: &[i64]) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let mut conn = open_or_init_cli()?;
    reorder_users(&mut conn, user_ids)
}

pub fn cli_delete_user(store: &CredentialStore, user_id: i64) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let mut conn = open_or_init_cli()?;
    delete_user(&mut conn, user_id)
}

/// CLI front-end for [`set_default_user`]: mark (or clear) the default /
/// favourite user that auto-unlocks on launch. Shared vault, so the flag
/// round-trips with the GUI Manage Users star. Per Ehud #311 (D1).
pub fn cli_set_default_user(
    store: &CredentialStore,
    user_id: i64,
    make_default: bool,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let mut conn = open_or_init_cli()?;
    set_default_user(&mut conn, user_id, make_default)
}

pub fn cli_storage_stats(store: &CredentialStore) -> Result<Vec<UserStorageStats>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    user_storage_stats(&conn)
}

pub fn cli_lock_session() {
    clear_user_session();
}

/// Read the active user's server profiles from the per-user partition.
///
/// Returns `Err("USER_LOCKED")` if the active user has a passphrase and the
/// session has not been unlocked yet. Returns `Err("NO_ACTIVE_USER")` if no
/// user exists (uncommon, migration always creates `default`).
pub fn cli_list_active_server_profiles(store: &CredentialStore) -> Result<Vec<Value>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = list_active_server_profiles(&conn, &root_key);
    root_key.zeroize();
    result
}

/// Keep the legacy `config_server_profiles` vault blob equal to the active
/// user's partition list.
///
/// The partition is the source of truth, but the blob is still read by
/// AeroAgent, MCP profile lookup, the partition rebuild, and the keystore
/// export and import. The CLI mirrored every active-user save into it; the GUI
/// never did, so for a GUI user the blob stayed frozen at the moment of the
/// partition migration, and every reader above saw deleted profiles, old names
/// and old quotas (#736). Only the active user is ever mirrored, the same rule
/// as the CLI, so no other partition's profile names reach the shared blob.
///
/// Writes only when the list differs (compared as JSON values, so key order in
/// the serialized form does not cause a rewrite). Returns whether it wrote.
pub fn mirror_active_profiles_to_legacy_blob(
    store: &CredentialStore,
    profiles: &[Value],
) -> Result<bool, String> {
    let current = get_optional_store_entry(store, LEGACY_PROFILES_BLOB)?
        .and_then(|raw| serde_json::from_str::<Vec<Value>>(&raw).ok());
    if current.as_deref() == Some(profiles) {
        return Ok(false);
    }
    let serialized =
        serde_json::to_string(profiles).map_err(|e| format!("Serialize profiles: {e}"))?;
    store
        .store(LEGACY_PROFILES_BLOB, &serialized)
        .map_err(|e| format!("Write {LEGACY_PROFILES_BLOB}: {e}"))?;
    Ok(true)
}

/// Read the active user's profiles from the partition database at `db_path`
/// and mirror them into the legacy blob (see
/// [`mirror_active_profiles_to_legacy_blob`]).
///
/// Opens the database read-only and never migrates or creates it: the keystore
/// export must not mutate the file it is snapshotting, and an import may have
/// just written a database from an older schema. `Ok(false)` when there is no
/// database; an `Err` (locked partition, unreadable DEK, older schema) leaves
/// the blob untouched, and the caller decides whether that is worth a warning.
pub fn refresh_legacy_profiles_blob_from_db(
    store: &CredentialStore,
    db_path: &std::path::Path,
) -> Result<bool, String> {
    if !db_path.is_file() {
        return Ok(false);
    }
    mirror_active_profiles_to_legacy_blob(store, &read_active_profiles_from_db(store, db_path)?)
}

/// The active user's profiles in the partition database at `db_path`, opened
/// read-only (never migrated or created). Used by the keystore import preview
/// on this machine's database and on a temporary copy of the backup's.
pub fn read_active_profiles_from_db(
    store: &CredentialStore,
    db_path: &std::path::Path,
) -> Result<Vec<Value>, String> {
    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("Open user partitions DB: {e}"))?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = list_active_server_profiles(&conn, &root_key);
    root_key.zeroize();
    result
}

/// Read server profiles for a specific user id without touching
/// `active_user_id`. Used by the CLI when `--user <name>` scopes a single
/// invocation to a partition without persisting the switch.
pub fn cli_list_server_profiles_for_user(
    store: &CredentialStore,
    user_id: i64,
) -> Result<Vec<Value>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = list_server_profiles_for(&conn, &root_key, user_id);
    root_key.zeroize();
    result
}

/// What the error of [`cli_replace_server_profiles_for_user`] and
/// [`cli_update_server_profiles_for_user`] starts with when
/// the database could not be opened or brought to this build's schema: the
/// one failure after which the CLI may keep the list in the legacy blob alone.
/// A write that fails on an open database (another process holding it) is
/// that write's error, and the import or edit that made it did not happen.
pub const PARTITION_UNAVAILABLE: &str = "USER_PARTITION_UNAVAILABLE";

/// Companion writer for [`cli_list_server_profiles_for_user`]. Replaces the
/// target user's profile rows without changing `active_user_id`.
pub fn cli_replace_server_profiles_for_user(
    store: &CredentialStore,
    user_id: i64,
    profiles: &[Value],
) -> Result<Vec<Value>, String> {
    let unavailable = |e: String| format!("{PARTITION_UNAVAILABLE}: {e}");
    init_or_migrate_cli(store).map_err(unavailable)?;
    let mut conn = open_or_init_cli().map_err(unavailable)?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = replace_server_profiles_for(&mut conn, &root_key, user_id, profiles);
    root_key.zeroize();
    result
}

/// CLI counterpart of [`update_server_profiles_for`] for the target user.
pub fn cli_update_server_profiles_for_user(
    store: &CredentialStore,
    user_id: i64,
    apply: impl FnOnce(&mut Vec<Value>) -> bool,
) -> Result<(Vec<Value>, bool), String> {
    let unavailable = |e: String| format!("{PARTITION_UNAVAILABLE}: {e}");
    init_or_migrate_cli(store).map_err(unavailable)?;
    let mut conn = open_or_init_cli().map_err(unavailable)?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = update_server_profiles_for(&mut conn, &root_key, user_id, apply);
    root_key.zeroize();
    result
}

/// `user_id`'s profiles and their revision, refused with [`ACCOUNT_CHANGED`]
/// unless `user_id` is the active account.
pub fn cli_read_active_profiles_at_revision(
    store: &CredentialStore,
    user_id: i64,
) -> Result<(Vec<Value>, String), String> {
    let conn = open_or_init_cli()?;
    if active_user_id(&conn)? != Some(user_id) {
        return Err(ACCOUNT_CHANGED.to_string());
    }
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    list_server_profiles_at_revision(&conn, &root_key, user_id)
}

/// Mirror `user_id`'s list as it stands now into the legacy blob, under
/// [`ProfileListLock`]: two writers mirroring after their own write could leave
/// the older list there. A no-op unless `user_id` is the active account. A failure
/// is only logged: the partition holds the list, and the next load heals the blob.
pub fn mirror_latest_profiles_to_legacy_blob(store: &CredentialStore, user_id: i64) {
    let mirrored = (|| {
        let conn = open_or_init_cli()?;
        let _lock = ProfileListLock::acquire(&conn)?;
        if active_user_id(&conn)? != Some(user_id) {
            return Ok(());
        }
        let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
        let list = list_server_profiles_for(&conn, &root_key, user_id)?;
        mirror_active_profiles_to_legacy_blob(store, &list).map(|_| ())
    })();
    if let Err(e) = mirrored {
        tracing::warn!("Could not mirror server profiles to the legacy blob: {e}");
    }
}

/// Mirror the active account's list for a load, which must not wait for a writer
/// and must not overwrite what one left in the blob: a keystore restore adopts it. A
/// writer holding [`ProfileListLock`] mirrors by itself, and the next load heals the
/// blob otherwise.
fn mirror_after_load(
    conn: &Connection,
    store: &CredentialStore,
    root_key: &[u8; 32],
    user_id: i64,
) {
    let _lock = match conn.path() {
        Some(path) if !path.is_empty() => {
            match ProfileListLock::acquire_within(
                std::path::Path::new(path),
                std::time::Duration::ZERO,
            ) {
                Ok(lock) => Some(lock),
                Err(e) => {
                    tracing::debug!("Legacy blob not mirrored by this load: {e}");
                    return;
                }
            }
        }
        _ => None,
    };
    let mirrored = active_user_id(conn).and_then(|active| {
        if active != Some(user_id) {
            return Ok(false);
        }
        let list = list_server_profiles_for(conn, root_key, user_id)?;
        mirror_active_profiles_to_legacy_blob(store, &list)
    });
    if let Err(e) = mirrored {
        tracing::warn!("Could not mirror server profiles to the legacy blob: {e}");
    }
}

/// CLI-path [`update_server_profiles_at_revision`].
pub fn cli_update_profiles_at_revision(
    store: &CredentialStore,
    user_id: i64,
    revision: &str,
    apply: impl FnOnce(&mut Vec<Value>),
) -> Result<(Vec<Value>, Vec<Value>), String> {
    let mut conn = open_or_init_cli()?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    update_server_profiles_at_revision(&mut conn, &root_key, user_id, revision, apply)
}

/// CLI counterpart of [`user_partitions_get_active_setting`]: read a per-user
/// `user_settings` scope for the active user, decrypted as JSON. Returns
/// `Ok(None)` when no row exists. Used by the CLI to keep low-stakes per-user
/// state (server groups, favourites) in the active user's partition instead of
/// the single global vault blob. Internal and MCP scopes are rejected.
pub fn cli_get_active_setting(
    store: &CredentialStore,
    scope: &str,
) -> Result<Option<Value>, String> {
    if is_reserved_public_setting_scope(scope) {
        return Err("USER_SETTING_RESERVED_SCOPE".to_string());
    }
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = get_active_user_setting(&conn, &root_key, scope);
    root_key.zeroize();
    result
}

/// CLI counterpart of [`user_partitions_set_active_setting`]: upsert a per-user
/// `user_settings` scope for the active user.
pub fn cli_set_active_setting(
    store: &CredentialStore,
    scope: &str,
    value: &Value,
) -> Result<(), String> {
    if is_reserved_public_setting_scope(scope) {
        return Err("USER_SETTING_RESERVED_SCOPE".to_string());
    }
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = set_active_user_setting(&conn, &root_key, scope, value);
    root_key.zeroize();
    result
}

/// CLI counterpart of [`get_user_setting_for`]: read a per-user `user_settings`
/// scope for the given `user_id` without changing `active_user_id`. Rejects
/// internal and MCP scopes (same rule as the active wrappers). Returns `Ok(None)`
/// when no row exists for that (user, scope). A passphrase-protected user that
/// is not the active session yields `Err("USER_LOCKED")`; the caller decides
/// how to surface it.
pub fn cli_get_user_setting(
    store: &CredentialStore,
    user_id: i64,
    scope: &str,
) -> Result<Option<Value>, String> {
    if is_reserved_public_setting_scope(scope) {
        return Err("USER_SETTING_RESERVED_SCOPE".to_string());
    }
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = get_user_setting_for(&conn, &root_key, user_id, scope);
    root_key.zeroize();
    result
}

/// CLI counterpart of [`set_user_setting_for`]: upsert a per-user
/// `user_settings` scope for the given `user_id` without touching
/// `active_user_id`. Same reserved-scope guard as the active path.
pub fn cli_set_user_setting(
    store: &CredentialStore,
    user_id: i64,
    scope: &str,
    value: &Value,
) -> Result<(), String> {
    if is_reserved_public_setting_scope(scope) {
        return Err("USER_SETTING_RESERVED_SCOPE".to_string());
    }
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = set_user_setting_for(&conn, &root_key, user_id, scope, value);
    root_key.zeroize();
    result
}

/// MUV-5: resolve the active user's server profiles for the MCP subprocess.
///
/// The MCP server runs headless and must scope to the persisted active user
/// instead of the legacy single-user `config_server_profiles` blob. A
/// device-wrapped active user resolves with no prompt; a passphrase-protected
/// active user is unlocked transiently from `AEROFTP_USER_PASSPHRASE` (the same
/// env the CLI honours), priming the process session so the subsequent
/// `resolve_active_credential` reads of `server_<id>` decrypt as well.
///
/// Falls back to the dual-maintained legacy blob only when there is no active
/// user or the partition errors (downgrade). A locked partition refuses access;
/// MUV-6 drops the fallback once the vault is purged.
pub fn mcp_list_active_server_profiles(store: &CredentialStore) -> Result<Vec<Value>, String> {
    mcp_list_server_profiles(store, None)
}

/// The account whose list a connection that began with no account reads
/// ([`LEGACY_ONLY_ACCOUNT`]): the legacy owner, whose stores its credentials
/// come from, once accounts exist, since the legacy blob then mirrors
/// whichever account is active. `None` while no account exists: the blob is
/// the list. An owner that cannot be resolved refuses.
fn legacy_only_list_owner() -> Result<Option<i64>, String> {
    if !cli_db_path()?.exists() {
        return Ok(None);
    }
    let conn = open_or_init_cli()?;
    let accounts: bool = conn
        .query_row("SELECT EXISTS(SELECT 1 FROM users)", [], |row| row.get(0))
        .map_err(|e| format!("Read accounts: {e}"))?;
    if !accounts {
        return Ok(None);
    }
    nonprofile_secret_owner_unpersisted(&conn)?
        .map(Some)
        .ok_or_else(|| "The legacy account this connection began with no longer exists".to_string())
}

/// The account a capture of `account` reads its profile and that profile's
/// secrets from, the same one for both: [`LEGACY_ONLY_ACCOUNT`] is the legacy
/// owner once accounts exist ([`legacy_only_list_owner`]), whose list the
/// profile comes from; its secrets then come from the owner's rows, and from
/// the vault only where the owner is proven to hold the key. The vault alone
/// is not the owner's: every account's save writes it. With no account the
/// capture stays legacy-only.
pub(crate) fn captured_list_owner(account: i64) -> Result<i64, String> {
    if account != LEGACY_ONLY_ACCOUNT {
        return Ok(account);
    }
    Ok(legacy_only_list_owner()?.unwrap_or(LEGACY_ONLY_ACCOUNT))
}

/// [`mcp_list_active_server_profiles`] for the account `user`, whichever is
/// active now: a connection bound to the account it began with reads that
/// account's list across a switch (`cloud_provider_factory`). `None` reads
/// the active account, as the MCP server does. An account that no longer
/// exists refuses, never falls back to another list.
pub fn mcp_list_server_profiles(
    store: &CredentialStore,
    user: Option<i64>,
) -> Result<Vec<Value>, String> {
    let user = match user {
        Some(LEGACY_ONLY_ACCOUNT) => match legacy_only_list_owner()? {
            Some(owner) => Some(owner),
            None => return read_legacy_server_profiles_blob(store),
        },
        other => other,
    };
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let active = match user {
        Some(id) => list_users(&conn)?
            .into_iter()
            .find(|candidate| candidate.id == id)
            .ok_or_else(|| "The account this connection began with no longer exists".to_string())?,
        None => match get_active_user(&conn)? {
            Some(user) => user,
            None => return read_legacy_server_profiles_blob(store),
        },
    };

    // Unlock a passphrase-protected active user transiently from the env so the
    // session DEK is primed for both this profile read and later credential
    // reads. A wrong/absent passphrase leaves the account locked and fails closed.
    if active.has_passphrase && session_user_id() != Some(active.id) {
        if let Ok(passphrase) = std::env::var("AEROFTP_USER_PASSPHRASE") {
            if !passphrase.is_empty() {
                let mut root_key = store.derive_user_partition_wrapping_key();
                let _ = unlock_user_transient(&conn, &root_key, active.id, Some(&passphrase));
                root_key.zeroize();
            }
        }
    }

    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = match user {
        Some(id) => list_server_profiles_for(&conn, &root_key, id),
        None => list_active_server_profiles(&conn, &root_key),
    };
    root_key.zeroize();
    match result {
        Ok(profiles) => Ok(profiles),
        Err(e) if user.is_none() && e == "NO_ACTIVE_USER" => {
            read_legacy_server_profiles_blob(store)
        }
        Err(e) => Err(e),
    }
}

/// Read and parse the legacy single-user `config_server_profiles` vault blob.
/// The MUV-5 rollout fallback for [`mcp_list_active_server_profiles`]; removed
/// with the rest of the legacy reads at MUV-6.
pub(crate) fn read_legacy_server_profiles_blob(
    store: &CredentialStore,
) -> Result<Vec<Value>, String> {
    let raw = store
        .get("config_server_profiles")
        .map_err(|e| format!("Failed to read profiles: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("Failed to parse profiles: {e}"))
}

/// CLI bridge: read a credential preferring the per-user store and falling back
/// to the legacy vault (MUV-2 rollout net). MUV-3 points the CLI's credential
/// resolution at this.
pub fn cli_read_credential_with_fallback(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = read_credential_with_fallback(&conn, store, &root_key, user_id, credential_id);
    root_key.zeroize();
    result
}

/// CLI bridge for the cross-user `profile-copy` / `profile-move` commands (N4).
/// The active (source) user must already be unlocked by the caller, e.g. via
/// `ensure_active_user_unlocked`; the target passphrase (if the target account
/// is protected) is supplied here. Profile and credential relocation share the
/// same production path as the GUI, including native session guards.
#[allow(clippy::too_many_arguments)]
pub fn cli_relocate_server_profile(
    store: &CredentialStore,
    source_user_id: i64,
    target_user_id: i64,
    profile_id: &str,
    new_profile_id: &str,
    target_passphrase: Option<&str>,
    remove_from_source: bool,
) -> Result<ProfileRelocation, String> {
    init_or_migrate_cli(store)?;
    let mut conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = relocate_server_profile_and_credentials(
        &mut conn,
        store,
        &root_key,
        source_user_id,
        target_user_id,
        profile_id,
        new_profile_id,
        target_passphrase,
        remove_from_source,
    );
    root_key.zeroize();
    result
}

/// Resolve a target user by name (CLI `--user` flag). Returns the user metadata
/// or `Err("USER_NOT_FOUND: <name>")` if the canonical lookup fails.
pub fn cli_find_user_by_name(store: &CredentialStore, name: &str) -> Result<UserMetadata, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let (_, canonical) = normalize_name(name)?;
    let users = list_users(&conn)?;
    users
        .into_iter()
        .find(|u| {
            normalize_name(&u.name)
                .map(|(_, c)| c == canonical)
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("USER_NOT_FOUND: {}", name))
}

// ============ WI-4d: CLI bridges for the P2P (peer) secret store ============
// Thin wrappers around the WI-4b `peer_identity` vault facade, following the same template as the
// `cli_*` server-profile bridges: open the partition db, derive the root wrapping key, and (for
// private material) run the facade call inside `with_partition_dek` so the per-user DEK is threaded
// and zeroized. Public material (the AeroFTP-ID, contacts, drive namespaces+roles) needs no DEK. These
// stay opaque-bytes in/out; the peer-l0 crypto lives in `crate::peer`, the CLI handler orchestrates.

/// Store (or replace) the active user's P2P identity. Refuses to clobber an existing identity unless
/// `force` (re-keying is the explicit WI-4e path). `secret_bytes`/`public_id` are produced by
/// `crate::peer::generate_identity`.
pub fn cli_peer_identity_store(
    store: &CredentialStore,
    user_id: i64,
    secret_bytes: &[u8],
    public_id: &str,
    force: bool,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    if !force && crate::peer_identity::identity_public_id(&conn, user_id)?.is_some() {
        return Err("PEER_IDENTITY_EXISTS".to_string());
    }
    let mut root_key = store.derive_user_partition_wrapping_key();
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let result = with_user_dek(&conn, &root_secret, user_id, |_uid, dek| {
        crate::peer_identity::store_identity(&conn, user_id, dek, secret_bytes, public_id)
    });
    root_key.zeroize();
    result
}

/// The active user's public AeroFTP-ID, or `None` if no identity exists. Public: no DEK.
pub fn cli_peer_identity_show(
    store: &CredentialStore,
    user_id: i64,
) -> Result<Option<String>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    crate::peer_identity::identity_public_id(&conn, user_id)
}

/// Load + decrypt the active user's 64-byte identity secret (wiped on drop), or `None` if unset.
pub fn cli_peer_identity_load(
    store: &CredentialStore,
    user_id: i64,
) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let result = with_user_dek(&conn, &root_secret, user_id, |_uid, dek| {
        crate::peer_identity::load_identity(&conn, user_id, dek)
    });
    root_key.zeroize();
    result
}

/// Add (or rename) a contact. Public material: no DEK.
pub fn cli_peer_contact_add(
    store: &CredentialStore,
    user_id: i64,
    contact_id: &str,
    alias: &str,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    crate::peer_identity::add_contact(&conn, user_id, contact_id, alias)
}

/// List the active user's contacts as `(contact_id, alias)`. Public: no DEK.
pub fn cli_peer_contact_list(
    store: &CredentialStore,
    user_id: i64,
) -> Result<Vec<(String, String)>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    crate::peer_identity::list_contacts(&conn, user_id)
}

/// Remove a contact (no-op if absent). Public: no DEK.
pub fn cli_peer_contact_remove(
    store: &CredentialStore,
    user_id: i64,
    contact_id: &str,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    crate::peer_identity::remove_contact(&conn, user_id, contact_id)
}

/// Store (or replace) the per-drive content key for `namespace_id` under `role`
/// (`publisher`/`replicator`). Private blob -> needs the DEK.
pub fn cli_peer_drive_store(
    store: &CredentialStore,
    user_id: i64,
    namespace_id: &str,
    role: &str,
    content_key: &[u8],
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let result = with_user_dek(&conn, &root_secret, user_id, |_uid, dek| {
        crate::peer_identity::store_drive(&conn, user_id, dek, namespace_id, role, content_key)
    });
    root_key.zeroize();
    result
}

/// Load + decrypt the per-drive content key for `namespace_id` (wiped on drop), or `None`.
pub fn cli_peer_drive_load(
    store: &CredentialStore,
    user_id: i64,
    namespace_id: &str,
) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let result = with_user_dek(&conn, &root_secret, user_id, |_uid, dek| {
        crate::peer_identity::load_drive(&conn, user_id, dek, namespace_id)
    });
    root_key.zeroize();
    result
}

/// A per-drive content key resolved from the ACTIVE user's partition, paired
/// with that user's id so callers can scope per-user state without a second
/// active-user lookup. The key is wiped on drop.
pub type ActiveUserDriveKey = (i64, zeroize::Zeroizing<Vec<u8>>);

/// GUI sibling of [`cli_peer_drive_load`] for the AeroShare runtime: load +
/// decrypt the per-drive content key for `namespace_id` from the ACTIVE
/// user's partition in the app vault (AppHandle-scoped DB). `None` when no
/// key for the namespace was imported into this partition.
pub fn gui_peer_drive_load(
    app: &AppHandle,
    namespace_id: &str,
) -> Result<Option<ActiveUserDriveKey>, String> {
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let result = peer_drive_load_for(&conn, &root_secret, user.id, namespace_id);
    root_key.zeroize();
    result.map(|opt| opt.map(|key| (user.id, key)))
}

/// [`gui_peer_drive_load`] for the account `user_id`, whichever is active now:
/// a share resolves the key of the account that started it, since a lookup
/// under another account finds no key and re-keys the drive out from under
/// the earlier grants. A locked account refuses (`USER_LOCKED`).
pub fn gui_peer_drive_load_as(
    app: &AppHandle,
    user_id: i64,
    namespace_id: &str,
) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let result = peer_drive_load_for(&conn, &root_secret, user_id, namespace_id);
    root_key.zeroize();
    result
}

/// `user_id`'s key for the drive `namespace_id`, under that account's DEK
/// whichever account is active; `None` when it has none.
fn peer_drive_load_for(
    conn: &Connection,
    root_secret: &SecretKey,
    user_id: i64,
    namespace_id: &str,
) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    with_user_dek(conn, root_secret, user_id, |_uid, dek| {
        crate::peer_identity::load_drive(conn, user_id, dek, namespace_id)
    })
}

/// Files `content_key` for the drive `namespace_id` under `user_id`'s DEK,
/// whichever account is active.
fn peer_drive_store_for(
    conn: &Connection,
    root_secret: &SecretKey,
    user_id: i64,
    namespace_id: &str,
    role: &str,
    content_key: &[u8],
) -> Result<(), String> {
    with_user_dek(conn, root_secret, user_id, |_uid, dek| {
        crate::peer_identity::store_drive(conn, user_id, dek, namespace_id, role, content_key)
    })
}

// ============ AeroShare P1 task 4: GUI bridges for the peer secret store ============
// AppHandle-scoped siblings of the `cli_peer_*` bridges above, for the Tauri commands in
// `crate::peer_commands`. Same custody rules: private material runs inside `with_user_dek`
// (DEK threaded + zeroized), public material (AFIDs, aliases, namespaces, roles) needs no DEK.
// All operate on the ACTIVE user partition.

/// GUI: the active user's AeroFTP-ID. With `auto_create`, mints + custodies a
/// fresh identity when none exists (the receiver-side "show my AFID" flow and
/// the first share both need this). Returns `(user_id, afid, created)`;
/// `None` only when no identity exists AND `auto_create` is false.
pub fn gui_peer_identity_get_or_create(
    app: &AppHandle,
    auto_create: bool,
) -> Result<Option<(i64, String, bool)>, String> {
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    if let Some(afid) = crate::peer_identity::identity_public_id(&conn, user.id)? {
        return Ok(Some((user.id, afid, false)));
    }
    if !auto_create {
        return Ok(None);
    }
    let (secret, afid) = crate::peer::generate_identity();
    let mut root_key = store.derive_user_partition_wrapping_key();
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let result = with_user_dek(&conn, &root_secret, user.id, |_uid, dek| {
        crate::peer_identity::store_identity(&conn, user.id, dek, &secret, &afid)
    });
    root_key.zeroize();
    result?;
    Ok(Some((user.id, afid, true)))
}

/// GUI: load + decrypt the active user's 64-byte identity secret (wiped on
/// drop), or `None` when the partition has no identity yet.
pub fn gui_peer_identity_load_secret(
    app: &AppHandle,
) -> Result<Option<ActiveUserDriveKey>, String> {
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let result = with_user_dek(&conn, &root_secret, user.id, |_uid, dek| {
        crate::peer_identity::load_identity(&conn, user.id, dek)
    });
    root_key.zeroize();
    result.map(|opt| opt.map(|key| (user.id, key)))
}

/// GUI: add (or rename) a contact in the active user's partition. Public: no DEK.
pub fn gui_peer_contact_add(app: &AppHandle, contact_id: &str, alias: &str) -> Result<(), String> {
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    crate::peer_identity::add_contact(&conn, user.id, contact_id, alias)
}

/// GUI: the active user's contacts as `(contact_id, alias)`. Public: no DEK.
pub fn gui_peer_contact_list(app: &AppHandle) -> Result<Vec<(String, String)>, String> {
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    crate::peer_identity::list_contacts(&conn, user.id)
}

/// GUI: remove a contact from the active user's partition. Public: no DEK.
/// No-op if the contact is absent.
pub fn gui_peer_contact_remove(app: &AppHandle, contact_id: &str) -> Result<(), String> {
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    crate::peer_identity::remove_contact(&conn, user.id, contact_id)
}

/// GUI: mute a sender AFID in the active user's partition. Public: no DEK.
pub fn gui_peer_mute_add(app: &AppHandle, contact_id: &str) -> Result<(), String> {
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    crate::peer_identity::add_mute(&conn, user.id, contact_id)
}

/// GUI: the active user's muted AFIDs. Public: no DEK.
pub fn gui_peer_mute_list(app: &AppHandle) -> Result<Vec<String>, String> {
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    crate::peer_identity::list_mutes(&conn, user.id)
}

/// GUI: unmute a sender AFID. Public: no DEK. No-op if not muted.
pub fn gui_peer_mute_remove(app: &AppHandle, contact_id: &str) -> Result<(), String> {
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    crate::peer_identity::remove_mute(&conn, user.id, contact_id)
}

/// GUI: the active user's AeroShare settings (friends-only gate + discovery
/// mode), falling back to defaults when unset. Public: no DEK.
pub fn gui_peer_settings_get(
    app: &AppHandle,
) -> Result<crate::peer_identity::PeerSettings, String> {
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    crate::peer_identity::get_settings(&conn, user.id)
}

/// GUI: store the active user's AeroShare settings. Public: no DEK.
pub fn gui_peer_settings_set(
    app: &AppHandle,
    settings: &crate::peer_identity::PeerSettings,
) -> Result<(), String> {
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    crate::peer_identity::set_settings(&conn, user.id, settings)
}

/// The verdict of the inbound gate for a single incoming signal.
pub enum InboundDecision {
    /// Surface the signal to the UI.
    Allow,
    /// Suppress the signal. The string is a stable reason for the run log
    /// (`muted` | `not-a-friend`).
    Drop(&'static str),
}

/// GUI: decide whether an inbound knock/action/offer from `sender_afid` should
/// reach the UI, applying the active user's per-sender mute and the friends-only
/// gate (a saved-contacts allowlist) in a single DB open. The in-memory
/// rate-limit is applied separately by the runtime. Public material: no DEK.
///
/// Fails OPEN on a missing partition or read error (returns `Allow`): the gate
/// is an attention-DoS mitigation, not an authorization boundary (every sender
/// is already cryptographically authenticated by iroh), so it must never drop a
/// legitimate signal because the vault was momentarily unreadable.
pub fn gui_peer_inbound_decision(app: &AppHandle, sender_afid: &str) -> InboundDecision {
    let conn = match open_or_init(app) {
        Ok(c) => c,
        Err(_) => return InboundDecision::Allow,
    };
    let Ok(Some(user)) = get_active_user(&conn) else {
        return InboundDecision::Allow;
    };
    if crate::peer_identity::is_muted(&conn, user.id, sender_afid).unwrap_or(false) {
        return InboundDecision::Drop("muted");
    }
    let friends_only = crate::peer_identity::get_settings(&conn, user.id)
        .map(|s| s.friends_only)
        .unwrap_or(false);
    if friends_only
        && !crate::peer_identity::is_contact(&conn, user.id, sender_afid).unwrap_or(true)
    {
        return InboundDecision::Drop("not-a-friend");
    }
    InboundDecision::Allow
}

/// GUI: the active user's persisted discovery mode (`both|dht|n0|none`),
/// defaulting to `both`. Read by the endpoint builder. Public: no DEK.
pub fn gui_peer_discovery_mode(app: &AppHandle) -> Result<String, String> {
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    Ok(crate::peer_identity::get_settings(&conn, user.id)?.discovery_mode)
}

/// GUI: rotate the active user's P2P identity, minting a fresh AFID and
/// overwriting the stored secret (the old AFID and every share link / served
/// drive ticket that encoded the old NodeId become unreachable). Returns the
/// new AeroFTP-ID. The caller is responsible for stopping/restarting the
/// receiver so the new identity seeds the endpoint.
pub fn gui_peer_identity_rotate(app: &AppHandle) -> Result<String, String> {
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    let (secret, afid) = crate::peer::generate_identity();
    let mut root_key = store.derive_user_partition_wrapping_key();
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let result = with_user_dek(&conn, &root_secret, user.id, |_uid, dek| {
        crate::peer_identity::store_identity(&conn, user.id, dek, &secret, &afid)
    });
    root_key.zeroize();
    result?;
    Ok(afid)
}

/// GUI: store (or replace) the per-drive content key for `namespace_id` under
/// `role` in the active user's partition. Returns the active `user_id`.
pub fn gui_peer_drive_store(
    app: &AppHandle,
    namespace_id: &str,
    role: &str,
    content_key: &[u8],
) -> Result<i64, String> {
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    gui_peer_drive_store_as(app, user.id, namespace_id, role, content_key)
}

/// [`gui_peer_drive_store`] for the account `user_id`, whichever is active now.
/// A share publication waits for its drive to be ready, and the account can
/// change meanwhile: the key goes to the account that started the share, never
/// to the one active when publication completes (deep scan item 46). An
/// account locked behind its passphrase meanwhile refuses (`USER_LOCKED`), so
/// the key is never filed under the wrong account.
pub fn gui_peer_drive_store_as(
    app: &AppHandle,
    user_id: i64,
    namespace_id: &str,
    role: &str,
    content_key: &[u8],
) -> Result<i64, String> {
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let result = peer_drive_store_for(
        &conn,
        &root_secret,
        user_id,
        namespace_id,
        role,
        content_key,
    );
    root_key.zeroize();
    result?;
    Ok(user_id)
}

/// GUI: the active user's drives as `(namespace_id, role)`. Public: no DEK.
pub fn gui_peer_drive_list(app: &AppHandle) -> Result<Vec<(String, String)>, String> {
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    crate::peer_identity::list_drives(&conn, user.id)
}

/// List the active user's drives as `(namespace_id, role)`. Public: no DEK.
pub fn cli_peer_drive_list(
    store: &CredentialStore,
    user_id: i64,
) -> Result<Vec<(String, String)>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    crate::peer_identity::list_drives(&conn, user_id)
}

/// Forget a drive (no-op if absent). Public: no DEK.
pub fn cli_peer_drive_forget(
    store: &CredentialStore,
    user_id: i64,
    namespace_id: &str,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    crate::peer_identity::delete_drive(&conn, user_id, namespace_id)
}

#[tauri::command]
pub async fn user_partitions_init(app: AppHandle) -> Result<MigrationReport, String> {
    init_or_migrate(&app)
}

// ============ Repair multi-user data (F-012 W4) ============

/// Health of the active user partition on THIS machine. The Repair panel
/// surfaces proactively when `active_user_readable` is false: the DEK cannot be
/// unwrapped with the local root_key, which is the headline cross-machine
/// import symptom (an imported passphrase-less partition stays bound to the
/// source machine's vault_key).
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PartitionHealth {
    /// True when the active user's server profiles decrypt with the local
    /// root_key. False = the DEK is bound to another machine (needs repair).
    pub active_user_readable: bool,
    /// Name of the active user, for the panel copy.
    pub active_user_name: Option<String>,
    /// Profiles readable for the active user (0 when unreadable).
    pub profile_count: u32,
    /// Raw backend error code when unreadable (mapped to copy by the frontend
    /// via `mapUserPartitionError`).
    pub error_code: Option<String>,
    /// Whether the legacy credential vault still holds profiles to rebuild
    /// from (gates the "Rebuild from this device" option).
    pub can_rebuild_from_device: bool,
}

/// Outcome of a "rebuild from this device" repair. The active partition is
/// reconstructed from the legacy credential vault (`config_server_profiles`)
/// under THIS machine's local root_key, after the possibly-broken
/// `user_partitions.db` is snapshotted and removed.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PartitionRebuildReport {
    /// Profiles recovered into the rebuilt partition, readable on this machine.
    pub recovered_profiles: u32,
    /// Path of the timestamped snapshot of the pre-rebuild user_partitions.db
    /// (None when there was nothing to preserve).
    pub backup_path: Option<String>,
    /// Whether a fresh default user had to be created.
    pub created_default_user: bool,
}

#[tauri::command]
pub async fn user_partitions_health(app: AppHandle) -> Result<PartitionHealth, String> {
    init_or_migrate(&app)?;
    let conn = open_or_init(&app)?;
    let active_user_name = get_active_user(&conn)?.map(|u| u.name);

    let store = CredentialStore::from_cache();
    let can_rebuild_from_device = store
        .as_ref()
        .and_then(|s| s.get("config_server_profiles").ok())
        .map(|p| {
            let trimmed = p.trim();
            !trimmed.is_empty() && trimmed != "[]"
        })
        .unwrap_or(false);

    let mut health = PartitionHealth {
        active_user_name,
        can_rebuild_from_device,
        ..Default::default()
    };

    let Some(store) = store else {
        health.error_code = Some("STORE_NOT_READY".to_string());
        return Ok(health);
    };
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = list_active_server_profiles(&conn, &root_key);
    root_key.zeroize();
    match result {
        Ok(profiles) => {
            health.active_user_readable = true;
            health.profile_count = profiles.len() as u32;
        }
        Err(e) => {
            health.active_user_readable = false;
            health.error_code = Some(e);
        }
    }
    Ok(health)
}

/// Copy the partition database at `path` before a rebuild removes it, with
/// every committed transaction: those still in its write-ahead log too, which
/// a copy of the main file alone misses whenever another connection holds a
/// read open or no checkpoint ran yet. `VACUUM INTO` writes one consistent
/// file; a database it cannot read (a damaged one) is copied with its `-wal`
/// and `-shm` files under matching names, from which SQLite recovers the
/// log when the backup is opened. An error leaves nothing removed.
fn snapshot_before_rebuild(path: &std::path::Path) -> Result<PathBuf, String> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup = path.with_file_name(format!("{DB_FILENAME}.pre-rebuild-{stamp}.bak"));
    let vacuumed = Connection::open(path)
        .and_then(|conn| conn.execute("VACUUM INTO ?1", params![backup.to_string_lossy()]));
    if let Err(vacuum) = vacuumed {
        let _ = std::fs::remove_file(&backup);
        std::fs::copy(path, &backup).map_err(|e| {
            format!("Snapshot user_partitions.db before rebuild: {vacuum}; copy: {e}")
        })?;
        for suffix in ["-wal", "-shm"] {
            let with = |file: &std::path::Path| {
                let mut name = file.as_os_str().to_os_string();
                name.push(suffix);
                PathBuf::from(name)
            };
            let sidecar = with(path);
            if sidecar.is_file() {
                std::fs::copy(&sidecar, with(&backup)).map_err(|e| {
                    format!("Snapshot user_partitions.db{suffix} before rebuild: {e}")
                })?;
            }
        }
    }
    Ok(backup)
}

/// A rebuild refused because a credential writer holds the partition database.
const REBUILD_WHILE_WRITTEN: &str =
    "The account database is being written right now, so it was not rebuilt; try again";

#[cfg(test)]
thread_local! {
    /// Runs once on this thread in the next [`remove_partitions_db_for_rebuild`],
    /// after its snapshot and before the removal: a credential writer
    /// committing meanwhile.
    static AFTER_REBUILD_SNAPSHOT: DualWriteHook = const { RefCell::new(None) };
    /// Runs once on this thread in the next [`remove_partitions_db_for_rebuild`],
    /// right before it takes the database's write lock.
    static BEFORE_REBUILD_WRITE_LOCK: DualWriteHook = const { RefCell::new(None) };
}

/// Empty the partition database in place, inside the write transaction
/// [`remove_partitions_db_for_rebuild`] already holds: every table, view,
/// trigger and index is dropped and the transaction is committed, so the
/// file a later open finds holds no schema, what a missing file would, with
/// no window in which a commit could land in a file that is then removed.
/// Foreign keys are off on this connection, so the drop order is free.
fn empty_partition_database(conn: &Connection) -> Result<(), String> {
    let objects = {
        let mut stmt = conn
            .prepare("SELECT type, name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")
            .map_err(|e| format!("List partition schema objects: {e}"))?;
        let listed = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("List partition schema objects: {e}"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| format!("List partition schema objects: {e}"))?;
        listed
    };
    for (kind, name) in objects {
        conn.execute_batch(&format!(
            "DROP {kind} IF EXISTS \"{}\"",
            name.replace('"', "\"\"")
        ))
        .map_err(|e| format!("Empty partition database ({kind} {name}): {e}"))?;
    }
    conn.execute_batch("COMMIT")
        .map_err(|e| format!("Empty partition database: {e}"))
}

/// Snapshot the partition database at `path` ([`snapshot_before_rebuild`])
/// and empty it for a rebuild; the backup, `None` for no database. A
/// credential writer that takes no [`ProfileListLock`] (a token refresh,
/// [`replace_user_credential_where`]) is held off from before the snapshot
/// to the emptying by a write transaction of this function's own, so the
/// backup holds every commit the emptied database had; a database a writer
/// holds now is not rebuilt ([`REBUILD_WHILE_WRITTEN`]). The file is emptied
/// where it stands, inside that transaction: there is no removal a commit
/// could fall into the gap of, on Windows included, where a file a
/// connection holds open cannot be removed at all. A database whose
/// `BEGIN IMMEDIATE` fails for a reason other than a held lock cannot be
/// emptied in place, and removing its file would leave exactly that gap —
/// a commit or a replacement landing between the check and the removal —
/// so the rebuild refuses it too ([`REBUILD_WHILE_WRITTEN`]).
fn remove_partitions_db_for_rebuild(path: &std::path::Path) -> Result<Option<PathBuf>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let guard =
        Connection::open(path).map_err(|e| format!("Open user_partitions.db for rebuild: {e}"))?;
    // Best effort, before the guard: a reader holding the log keeps it, and
    // VACUUM INTO reads through it all the same.
    let _ = guard.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    // The session before SQLite's write lock, the order every writer takes
    // them in ([`SessionHold`]): taken after it, a passphrase account's writer
    // holding the session while it waits on the database would wait out its
    // busy timeout, and its write would fail.
    let mut session = hold_session().unwrap_or(SessionHold(None));
    #[cfg(test)]
    run_dual_write_hook(&BEFORE_REBUILD_WRITE_LOCK);
    let write_locked = match guard.execute_batch("BEGIN IMMEDIATE") {
        Ok(()) => true,
        Err(rusqlite::Error::SqliteFailure(e, _))
            if matches!(
                e.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            return Err(REBUILD_WHILE_WRITTEN.to_string());
        }
        Err(_) => false,
    };
    let backup = snapshot_before_rebuild(path)?;
    #[cfg(test)]
    run_dual_write_hook(&AFTER_REBUILD_SNAPSHOT);
    // Drop any unlocked session before the database is emptied or removed.
    session.clear();
    if write_locked {
        empty_partition_database(&guard)?;
    } else {
        // `BEGIN IMMEDIATE` failed for a reason other than a held lock, so
        // the file cannot be emptied in place — and removing it would open
        // the gap a commit or a replacement could land in, on any platform.
        // `PRAGMA data_version` cannot close that gap: it is per connection,
        // so two reads prove nothing about the file. Refuse the rebuild.
        return Err(REBUILD_WHILE_WRITTEN.to_string());
    }
    drop(session);
    invalidate_session_consumers();
    Ok(Some(backup))
}

/// Rebuild the active partition from this device's legacy credential vault.
/// Snapshots and empties the current (possibly machine-bound, unreadable)
/// `user_partitions.db`, then re-runs the legacy migration so a fresh default
/// partition is created under THIS machine's local root_key. This is the
/// manual local-reset the owner ran by hand during the F-012 incident,
/// productized with an automatic pre-step backup.
#[tauri::command]
pub async fn user_partitions_repair_rebuild(
    app: AppHandle,
) -> Result<PartitionRebuildReport, String> {
    let path = db_path(&app)?;
    let mut report = PartitionRebuildReport::default();
    let lock = ProfileListLock::acquire_for_path(&path)?;

    if let Some(backup) = remove_partitions_db_for_rebuild(&path)? {
        report.backup_path = Some(backup.to_string_lossy().into_owned());
    }

    // Rebuild from the legacy credential vault under the local root_key.
    let migration = init_or_migrate(&app)?;
    drop(lock);
    report.created_default_user = migration.created_default_user;

    // Count what came back readable on this machine.
    if let Some(store) = CredentialStore::from_cache() {
        let conn = open_or_init(&app)?;
        let mut root_key = store.derive_user_partition_wrapping_key();
        let profiles = list_active_server_profiles(&conn, &root_key);
        root_key.zeroize();
        if let Ok(profiles) = profiles {
            report.recovered_profiles = profiles.len() as u32;
        }
    }
    Ok(report)
}

#[tauri::command]
pub async fn user_partitions_list_users(app: AppHandle) -> Result<Vec<UserMetadata>, String> {
    init_or_migrate(&app)?;
    let conn = open_or_init(&app)?;
    list_users(&conn)
}

/// Backward-compat self-heal (BUG-LT2a): the redacted `hasStoredAeroCrypt*`
/// flags are persisted into the profile blob at save/import time, but the normal
/// load path returns them verbatim. Profiles created before those flags existed
/// (or imported without them) therefore load with the flags unset even though
/// the overlay secret IS present in the vault, and the UI
/// (`getProfileOverlayHint` / `maybeAutoUnlockProfileOverlay`) gates the
/// crypt-overlay auto-unlock on exactly those flags, so the overlay silently
/// fails to arm and the dual panel shows raw ciphertext. Recompute the four
/// flags from the credential store on every load (the vault is the source of
/// truth) for profiles that carry an ENABLED overlay binding, using the SAME
/// lookup the connect path reads (`get_credential` == `store.get`) so the flag
/// can never disagree with what the unlock will actually retrieve. Cheap: the
/// store is the in-memory cache and only enabled-overlay profiles are probed.
///
/// `has_secret` is injected (not a `&CredentialStore`) so the pure reconcile
/// logic is unit-testable without a live vault.
fn reconcile_overlay_secret_flags<F: Fn(&str) -> bool>(has_secret: F, profiles: &mut [Value]) {
    for profile in profiles.iter_mut() {
        let enabled = profile
            .get("aeroCryptOverlay")
            .and_then(|b| b.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !enabled {
            continue;
        }
        let id = match profile.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => continue,
        };
        if let Some(map) = profile.as_object_mut() {
            map.insert(
                "hasStoredAeroCryptPassword".into(),
                Value::Bool(has_secret(&format!("aerocrypt_overlay_pw_{id}"))),
            );
            map.insert(
                "hasStoredAeroCryptSalt".into(),
                Value::Bool(has_secret(&format!("aerocrypt_overlay_salt_{id}"))),
            );
            map.insert(
                "hasStoredAeroCryptKeyfilePath".into(),
                Value::Bool(has_secret(&format!("aerocrypt_overlay_keyfile_path_{id}"))),
            );
            map.insert(
                "hasStoredAeroCryptConfig".into(),
                Value::Bool(has_secret(&format!("aerocrypt_overlay_config_{id}"))),
            );
        }
    }
}

#[tauri::command]
pub async fn user_partitions_load_active_server_profiles(
    app: AppHandle,
) -> Result<Vec<Value>, String> {
    load_active_server_profiles_scoped(&app).map(|scoped| scoped.profiles)
}

/// The profiles of [`user_partitions_load_active_server_profiles`] and the id
/// of the account they belong to, for a caller that will write the list back
/// through `user_partitions_save_active_server_profiles` with that id.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedServerProfiles {
    pub user_id: i64,
    pub profiles: Vec<Value>,
    /// Hands the write-back its check that no other writer saved in between.
    pub revision: String,
}

#[tauri::command]
pub async fn user_partitions_load_active_server_profiles_scoped(
    app: AppHandle,
) -> Result<ScopedServerProfiles, String> {
    load_active_server_profiles_scoped(&app)
}

fn load_active_server_profiles_scoped(app: &AppHandle) -> Result<ScopedServerProfiles, String> {
    init_or_migrate(app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let mut conn = open_or_init(app)?;
    let result = active_user_id(&conn)
        .and_then(|user| user.ok_or_else(|| "NO_ACTIVE_USER".to_string()))
        .and_then(|user_id| {
            list_server_profiles_at_revision(&conn, &root_key, user_id)
                .map(|(profiles, revision)| (user_id, profiles, revision))
        });
    let (user_id, mut profiles, mut revision) = match result {
        Ok(scoped) => scoped,
        Err(e) => {
            root_key.zeroize();
            return Err(e);
        }
    };
    let read = profiles.clone();
    // rclone-crypt secrets an older import left in a profile's options move to
    // the vault and a binding, once; the list is re-read, merged and written
    // in one transaction, and only when a move applied. Secrets and profiles
    // go to the account the list was read from, not to one switched to since.
    let notes = crate::bridge_commands::migrate_legacy_rclone_crypt_on_load(
        &mut profiles,
        |key, secret| store_credential_for_user_dual(&store, user_id, key, secret),
        |key| {
            resolve_credential_for_user(&store, user_id, key)
                .ok()
                .flatten()
                .map(|s| s.to_string())
        },
        |apply| update_server_profiles_for(&mut conn, &root_key, user_id, apply),
    );
    for note in &notes {
        tracing::warn!("{note}");
    }
    // A move rewrote the rows: the revision read above would refuse the next
    // write, so list and revision are read again, together.
    if profiles != read {
        match list_server_profiles_at_revision(&conn, &root_key, user_id) {
            Ok(fresh) => (profiles, revision) = fresh,
            Err(e) => {
                root_key.zeroize();
                return Err(e);
            }
        }
    }
    // #736: heal a legacy blob that an older build left frozen, so AeroAgent
    // and MCP see the same list as My Servers without waiting for a save.
    // Best-effort: the list is what the caller asked for.
    mirror_after_load(&conn, &store, &root_key, user_id);
    root_key.zeroize();
    // BUG-LT2a backward-compat: recompute the redacted overlay-secret flags from
    // the vault so legacy crypt profiles auto-unlock without a manual re-save.
    reconcile_overlay_secret_flags(
        |key| store.get(key).map(|v| !v.is_empty()).unwrap_or(false),
        &mut profiles,
    );
    Ok(ScopedServerProfiles {
        user_id,
        profiles,
        revision,
    })
}

/// Replace the active account's profiles. With `expected_user_id` (the id a
/// scoped load returned) the write is bound to that account and refused with
/// [`ACCOUNT_CHANGED`] when another account became active since the read. With
/// `expected_revision` as well (the revision of that load) it is refused with
/// [`PROFILES_CHANGED`] when another writer saved since.
#[tauri::command]
pub async fn user_partitions_save_active_server_profiles(
    app: AppHandle,
    mut profiles: Vec<Value>,
    expected_user_id: Option<i64>,
    expected_revision: Option<String>,
) -> Result<Vec<Value>, String> {
    crate::launch_url::launch_secret::strip_from_profiles(&mut profiles);
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let mut conn = open_or_init(&app)?;
    let result = match expected_user_id {
        Some(expected) => replace_expected_server_profiles(
            &mut conn,
            &root_key,
            expected,
            expected_revision.as_deref(),
            &profiles,
        ),
        None => replace_active_server_profiles(&mut conn, &root_key, &profiles),
    };
    root_key.zeroize();
    // The list stored, whose names the write made unique: the caller shows
    // this one, not the one it sent.
    let stored = result?;
    // The blob mirrors the account now active, in the latest list, not this one:
    // another writer may have saved since. The write succeeded, so a mirror
    // failure is a warning (#736).
    match active_user_id(&conn) {
        Ok(Some(user)) => mirror_latest_profiles_to_legacy_blob(&store, user),
        Ok(None) => {}
        Err(e) => tracing::warn!("Could not read the active account to mirror the list: {e}"),
    }
    Ok(stored)
}

/// Replace the active account's list with the legacy blob, for a keystore restore
/// that holds [`ProfileListLock`]: what it left there is the list to keep. A load must
/// not come first, since it mirrors the partition over the blob. `None` when the blob
/// holds no list (a backup may carry none) or there is no active account, or for an
/// empty one unless `allow_empty`: a restore's decisions can leave a real empty list.
/// Returns the list stored.
pub(crate) fn adopt_legacy_blob_under_lock(
    conn: &mut Connection,
    store: &CredentialStore,
    root_key: &[u8; 32],
    allow_empty: bool,
) -> Result<Option<Vec<Value>>, String> {
    let Some(raw) = get_optional_store_entry(store, LEGACY_PROFILES_BLOB)? else {
        return Ok(None);
    };
    let Ok(list) = serde_json::from_str::<Vec<Value>>(&raw) else {
        return Ok(None);
    };
    if list.is_empty() && !allow_empty {
        return Ok(None);
    }
    let Some(user_id) = active_user_id(conn)? else {
        return Ok(None);
    };
    let stored = replace_server_profiles_under_lock(conn, root_key, user_id, &list)?;
    if let Err(e) = mirror_active_profiles_to_legacy_blob(store, &stored) {
        tracing::warn!("Could not mirror server profiles to the legacy blob: {e}");
    }
    Ok(Some(stored))
}

/// Keep `profiles` in the legacy store, for a frontend whose partition could not
/// be reached. Under [`ProfileListLock`], this refuses with [`PROFILES_CHANGED`] if
/// the partition can be reached now, since the next load would replace the blob
/// with the partition's list, or if the blob no longer holds `expected_list`, the
/// list the caller read and changed. The list held decides who had a name first,
/// as the partition's does (`util::profile_name`). Resolves to the list stored.
#[tauri::command]
pub async fn user_partitions_store_legacy_server_profiles(
    app: AppHandle,
    mut profiles: Vec<Value>,
    expected_list: Option<Vec<Value>>,
) -> Result<Vec<Value>, String> {
    crate::launch_url::launch_secret::strip_from_profiles(&mut profiles);
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let lock = ProfileListLock::acquire_for_path(&db_path(&app)?)?;
    if !lock.is_exclusive() {
        return Err(NO_FILE_LOCKS.to_string());
    }
    // A store that opens holds the list, even with its account locked: the next load
    // would replace the blob with its list.
    if init_or_migrate(&app)
        .and_then(|_| open_or_init(&app))
        .is_ok()
    {
        return Err(PROFILES_CHANGED.to_string());
    }
    let held = get_optional_store_entry(&store, LEGACY_PROFILES_BLOB)?
        .map(|raw| serde_json::from_str::<Vec<Value>>(&raw))
        .transpose()
        .map_err(|e| format!("Invalid saved profiles: {e}"))?;
    let held = held.unwrap_or_default();
    if expected_list.is_some_and(|expected| expected != held) {
        return Err(PROFILES_CHANGED.to_string());
    }
    let (stored, _) = crate::util::profile_name::unique_profile_names(&held, &profiles);
    let serialized = serde_json::to_string(&stored).map_err(|e| e.to_string())?;
    store
        .store(LEGACY_PROFILES_BLOB, &serialized)
        .map_err(|e| format!("Store server profiles in vault: {e}"))?;
    Ok(stored)
}

/// N4: copy or move a saved server profile from the ACTIVE (source) user into
/// another user account. The source is always the active user, so the caller
/// only supplies the target. `new_profile_id` is generated by the frontend
/// (same `srv_<ts>_<rand>` convention as Duplicate) so the relocated copy is
/// fully independent of the original. `target_passphrase` is required only when
/// the target account is passphrase protected.
#[tauri::command]
pub async fn user_partitions_relocate_server_profile(
    app: AppHandle,
    target_user_id: i64,
    profile_id: String,
    new_profile_id: String,
    target_passphrase: Option<String>,
    move_profile: bool,
) -> Result<ProfileRelocation, String> {
    let target_passphrase = target_passphrase.map(Zeroizing::new);
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut conn = open_or_init(&app)?;
    let source_user_id = active_user_id(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    let root_key = Zeroizing::new(store.derive_user_partition_wrapping_key());
    tokio::task::spawn_blocking(move || {
        relocate_server_profile_and_credentials(
            &mut conn,
            &store,
            &root_key,
            source_user_id,
            target_user_id,
            &profile_id,
            &new_profile_id,
            target_passphrase.as_deref().map(String::as_str),
            move_profile,
        )
    })
    .await
    .map_err(|error| format!("Profile relocation task failed: {error}"))?
}

/// Generic per-user setting access (MU-4 foundation). Settings are keyed by
/// scope (e.g. `aerosync_schedule`, `aerosync_profiles`) and encrypted with
/// the active user's DEK. Internal and MCP scopes are blocked at this boundary
/// so generic settings cannot overwrite backups or bypass MCP authorization.
/// Returns JSON null when no setting exists for that scope.
#[tauri::command]
pub async fn user_partitions_get_active_setting(
    app: AppHandle,
    scope: String,
) -> Result<Option<Value>, String> {
    if is_reserved_public_setting_scope(&scope) {
        return Err("USER_SETTING_RESERVED_SCOPE".to_string());
    }
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let conn = open_or_init(&app)?;
    let result = get_active_user_setting(&conn, &root_key, &scope);
    root_key.zeroize();
    result
}

#[tauri::command]
pub async fn user_partitions_set_active_setting(
    app: AppHandle,
    scope: String,
    value: Value,
) -> Result<(), String> {
    if is_reserved_public_setting_scope(&scope) {
        return Err("USER_SETTING_RESERVED_SCOPE".to_string());
    }
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let conn = open_or_init(&app)?;
    let result = set_active_user_setting(&conn, &root_key, &scope, &value);
    root_key.zeroize();
    result
}

#[tauri::command]
pub async fn user_partitions_delete_active_setting(
    app: AppHandle,
    scope: String,
) -> Result<(), String> {
    if is_reserved_public_setting_scope(&scope) {
        return Err("USER_SETTING_RESERVED_SCOPE".to_string());
    }
    init_or_migrate(&app)?;
    let conn = open_or_init(&app)?;
    delete_active_user_setting(&conn, &scope)
}

// MCP catalogs carry backend-issued permissions and must only use their guarded API.
fn is_reserved_public_setting_scope(scope: &str) -> bool {
    scope.starts_with("__") || scope.starts_with("aeroagent_mcp_")
}

#[cfg(test)]
mod private_mcp_setting_tests {
    #[test]
    fn reserves_catalogs_and_future_mcp_scopes_without_blocking_ui_settings() {
        for scope in [
            "__backup",
            "aeroagent_mcp_servers",
            "aeroagent_mcp_http_servers",
            "aeroagent_mcp_future",
        ] {
            assert!(super::is_reserved_public_setting_scope(scope));
        }
        for scope in [
            "aerosync_schedule",
            "aeroagent_preferences",
            "aeroagent_mcp",
            "",
        ] {
            assert!(!super::is_reserved_public_setting_scope(scope));
        }
    }
}

/// MU-7: ask "is this profile already saved by another user account?". Called
/// after a save from the connection form (`warnIfSavedByOtherAccount` in
/// `src/utils/crossUserDedupWarning.ts`) to surface a soft warning that never
/// blocks the save. Returns the public metadata of every OTHER user with a
/// matching dedup_key; intra-user duplicates stay with the frontend's own
/// duplicate check, which records them in the Activity Log (R11).
#[tauri::command]
pub async fn user_partitions_find_cross_user_dedup(
    app: AppHandle,
    profile: Value,
) -> Result<Vec<CrossUserDedupMatch>, String> {
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let conn = open_or_init(&app)?;
    let active = active_user_id(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    let result = cross_user_dedup_matches(&conn, &root_key, active, &profile);
    root_key.zeroize();
    result
}

#[tauri::command]
pub async fn user_partitions_add_user(
    app: AppHandle,
    name: String,
    avatar_emoji: Option<String>,
    avatar_color: Option<String>,
    mut passphrase: Option<String>,
) -> Result<UserMetadata, String> {
    init_or_migrate(&app)?;
    let mut conn = open_or_init(&app)?;
    let existing_users = match list_users(&conn) {
        Ok(users) => users,
        Err(err) => {
            if let Some(passphrase) = passphrase.as_mut() {
                passphrase.zeroize();
            }
            return Err(err);
        }
    };
    if !existing_users.is_empty() {
        let status = match user_unlock_status(&conn) {
            Ok(status) => status,
            Err(err) => {
                if let Some(passphrase) = passphrase.as_mut() {
                    passphrase.zeroize();
                }
                return Err(err);
            }
        };
        let actor_id = match status.unlocked_user_id {
            Some(actor_id) => actor_id,
            None => {
                if let Some(passphrase) = passphrase.as_mut() {
                    passphrase.zeroize();
                }
                return Err("VAULT_LOCKED".to_string());
            }
        };
        let actor_is_admin = match is_admin_user(&conn, actor_id) {
            Ok(value) => value,
            Err(err) => {
                if let Some(passphrase) = passphrase.as_mut() {
                    passphrase.zeroize();
                }
                return Err(err);
            }
        };
        if !actor_is_admin {
            if let Some(passphrase) = passphrase.as_mut() {
                passphrase.zeroize();
            }
            return Err("NOT_AUTHORIZED".to_string());
        }
    }
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = create_user(
        &mut conn,
        &root_key,
        &name,
        avatar_emoji.as_deref(),
        avatar_color.as_deref(),
        passphrase.as_deref(),
    );
    if let Some(passphrase) = passphrase.as_mut() {
        passphrase.zeroize();
    }
    root_key.zeroize();
    result
}

/// True when `candidate` matches an existing user name (case-insensitive,
/// trimmed). Pure helper backing the copy-user auto-naming (mirrors the CLI's
/// `user_name_taken`).
fn user_name_is_taken(users: &[UserMetadata], candidate: &str) -> bool {
    users
        .iter()
        .any(|u| u.name.eq_ignore_ascii_case(candidate.trim()))
}

/// The auto name for a user copy: `"<src> (copy)"`, then `"... (copy 2)"`, etc.
/// when the base is taken. Pure, case-insensitive; mirrors the CLI's
/// `next_user_copy_name` so GUI and CLI produce identical copy names.
fn next_user_copy_name(users: &[UserMetadata], src_name: &str) -> String {
    let base = format!("{src_name} (copy)");
    if !user_name_is_taken(users, &base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{src_name} (copy {n})");
        if !user_name_is_taken(users, &candidate) {
            return candidate;
        }
        n += 1;
    }
}

fn credential_free_profile_copies(profiles: &[Value]) -> Vec<Value> {
    profiles
        .iter()
        .map(|profile| {
            let mut copied = profile.clone();
            let mut options = copied.get("options").cloned();
            crate::profile_export::strip_options_borne_secrets(&mut options);
            // The option-borne secrets an import report strips too: the
            // rclone-crypt passwords, an API key, a key passphrase and the
            // injected Proton session (`rclone_import::options_without_secrets`).
            if let Some(map) = options.as_mut().and_then(|value| value.as_object_mut()) {
                for key in crate::rclone_import::IMPORT_SECRET_OPTION_KEYS {
                    map.remove(*key);
                }
            }
            if options
                .as_ref()
                .is_some_and(|value| value.as_object().is_some_and(|map| map.is_empty()))
            {
                options = None;
            }
            let fresh_id = format!("srv_{}", uuid::Uuid::new_v4());
            if let Some(object) = copied.as_object_mut() {
                match options {
                    Some(value) => {
                        object.insert("options".to_string(), value);
                    }
                    None => {
                        object.remove("options");
                    }
                }
                object.insert("id".to_string(), Value::String(fresh_id.clone()));
                for alias in ["uid", "profileUid"] {
                    if object.contains_key(alias) {
                        object.insert(alias.to_string(), Value::String(fresh_id.clone()));
                    }
                }
            }
            copied
        })
        .collect()
}

/// Duplicate an existing user (GUI mirror of the CLI `users -i` Copy verb,
/// Ehud #347). Creates a brand-new, password-free, non-admin user that copies
/// the source's display avatar and all of its server profiles, but NO passwords
/// or credentials: those live in the source user's keyring and stay with it.
/// The new user gets fresh crypto material (its own DEK).
///
/// `source_user_id` is the user to copy; `new_name` is an optional explicit
/// name (must be free); when omitted it is auto-derived as `"<source> (copy)"`
/// (then `(copy 2)`, …), identical to the CLI. Admin-gated exactly like
/// [`user_partitions_add_user`]. Returns `USER_LOCKED` when the source is
/// password-protected and not the currently-unlocked user (its servers cannot
/// be read); the caller should ask the user to switch to it first.
#[tauri::command]
pub async fn user_partitions_copy_user(
    app: AppHandle,
    source_user_id: i64,
    new_name: Option<String>,
) -> Result<UserMetadata, String> {
    init_or_migrate(&app)?;
    let mut conn = open_or_init(&app)?;
    let users = list_users(&conn)?;

    // Admin gate (same policy as user_partitions_add_user): only an unlocked
    // admin may create users. A copy always runs against >=1 existing user, so
    // the gate always applies.
    let status = user_unlock_status(&conn)?;
    let actor_id = status
        .unlocked_user_id
        .ok_or_else(|| "VAULT_LOCKED".to_string())?;
    if !is_admin_user(&conn, actor_id)? {
        return Err("NOT_AUTHORIZED".to_string());
    }

    let src = users
        .iter()
        .find(|u| u.id == source_user_id)
        .cloned()
        .ok_or_else(|| "USER_NOT_FOUND".to_string())?;

    // Resolve the target name: explicit (must be free) else "<src> (copy)".
    let target_name = match new_name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => {
            if user_name_is_taken(&users, n) {
                return Err("NAME_TAKEN".to_string());
            }
            n.to_string()
        }
        None => next_user_copy_name(&users, &src.name),
    };

    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();

    // Read the source's servers first (no passwords copied, keyring creds stay
    // with the original). A locked, password-protected source cannot be read
    // and surfaces USER_LOCKED, telling the caller to switch to it first.
    let profiles = match list_server_profiles_for(&conn, &root_key, source_user_id) {
        Ok(p) => p,
        Err(e) => {
            root_key.zeroize();
            return Err(e);
        }
    };

    // Create the new, password-free user (fresh DEK, non-admin), copying only
    // the display avatar (emoji + colour).
    let new_user = match create_user(
        &mut conn,
        &root_key,
        &target_name,
        src.avatar_emoji.as_deref(),
        src.avatar_color.as_deref(),
        None,
    ) {
        Ok(u) => u,
        Err(e) => {
            root_key.zeroize();
            return Err(e);
        }
    };

    // Copy the (credential-free) server profiles into the new user with fresh
    // credential identities, so no lookup can resolve the source user's secrets.
    let profiles = credential_free_profile_copies(&profiles);
    let result = replace_server_profiles_for(&mut conn, &root_key, new_user.id, &profiles);
    root_key.zeroize();
    result.map(|_| new_user)
}

#[tauri::command]
pub async fn user_partitions_unlock_user(
    app: AppHandle,
    user_id: i64,
    mut passphrase: Option<String>,
) -> Result<UserUnlockStatus, String> {
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let conn = open_or_init(&app)?;
    let result = unlock_user(&conn, &root_key, user_id, passphrase.as_deref());
    if let Some(passphrase) = passphrase.as_mut() {
        passphrase.zeroize();
    }
    // MUV-2 lazy: migrate the just-unlocked passphrase user's legacy secrets.
    if result.is_ok() {
        let _ = migrate_credentials_for_user(&conn, &store, &root_key, user_id);
    }
    root_key.zeroize();
    result
}

#[tauri::command]
pub async fn user_partitions_lock_session() -> Result<(), String> {
    clear_user_session();
    Ok(())
}

#[tauri::command]
pub async fn user_partitions_unlock_status(app: AppHandle) -> Result<UserUnlockStatus, String> {
    let conn = open_or_init(&app)?;
    user_unlock_status(&conn)
}

#[tauri::command]
pub async fn user_partitions_change_passphrase(
    app: AppHandle,
    user_id: i64,
    mut old_passphrase: Option<String>,
    mut new_passphrase: Option<String>,
) -> Result<(), String> {
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let conn = open_or_init(&app)?;
    // Self-only: change_user_passphrase unwraps the DEK with the
    // CURRENT passphrase. Admin acting on a peer cannot supply the
    // current passphrase, so the only path admin has on a peer is the
    // destructive admin_reset_user_passphrase. Reject any non-self call
    // here, even from admin, to keep the foot-gun closed.
    let status = user_unlock_status(&conn)?;
    if status.unlocked_user_id != Some(user_id) {
        root_key.zeroize();
        if let Some(passphrase) = old_passphrase.as_mut() {
            passphrase.zeroize();
        }
        if let Some(passphrase) = new_passphrase.as_mut() {
            passphrase.zeroize();
        }
        return Err("NOT_ACTIVE_USER".to_string());
    }
    let result = change_user_passphrase(
        &conn,
        &root_key,
        user_id,
        old_passphrase.as_deref(),
        new_passphrase.as_deref(),
    );
    if let Some(passphrase) = old_passphrase.as_mut() {
        passphrase.zeroize();
    }
    if let Some(passphrase) = new_passphrase.as_mut() {
        passphrase.zeroize();
    }
    root_key.zeroize();
    result
}

#[tauri::command]
pub async fn user_partitions_rename_user(
    app: AppHandle,
    user_id: i64,
    name: String,
) -> Result<(), String> {
    init_or_migrate(&app)?;
    let conn = open_or_init(&app)?;
    // Self-or-admin: admin can rename a peer (CMS-style account
    // management); a non-admin peer cannot rename anyone but self.
    ensure_user_can_modify(&conn, user_id)?;
    rename_user(&conn, user_id, &name)
}

#[tauri::command]
pub async fn user_partitions_set_user_avatar(
    app: AppHandle,
    user_id: i64,
    avatar_emoji: Option<String>,
    avatar_color: Option<String>,
) -> Result<(), String> {
    init_or_migrate(&app)?;
    let conn = open_or_init(&app)?;
    ensure_user_can_modify(&conn, user_id)?;
    set_user_avatar(
        &conn,
        user_id,
        avatar_emoji.as_deref(),
        avatar_color.as_deref(),
    )
}

#[tauri::command]
pub async fn user_partitions_reorder_users(
    app: AppHandle,
    user_ids: Vec<i64>,
) -> Result<(), String> {
    init_or_migrate(&app)?;
    let mut conn = open_or_init(&app)?;
    let status = user_unlock_status(&conn)?;
    let actor_id = status
        .unlocked_user_id
        .ok_or_else(|| "VAULT_LOCKED".to_string())?;
    if !is_admin_user(&conn, actor_id)? {
        return Err("NOT_AUTHORIZED".to_string());
    }
    reorder_users(&mut conn, &user_ids)
}

#[tauri::command]
pub async fn user_partitions_delete_user(app: AppHandle, user_id: i64) -> Result<(), String> {
    init_or_migrate(&app)?;
    let mut conn = open_or_init(&app)?;
    // Self-or-admin: a non-admin peer cannot delete another account; an
    // admin can. Last-admin and last-user guards live inside
    // delete_user() so they apply regardless of caller path.
    ensure_user_can_modify(&conn, user_id)?;
    delete_user(&mut conn, user_id)
}

#[tauri::command]
pub async fn user_partitions_set_admin(
    app: AppHandle,
    user_id: i64,
    is_admin: bool,
) -> Result<(), String> {
    init_or_migrate(&app)?;
    let mut conn = open_or_init(&app)?;
    set_user_admin(&mut conn, user_id, is_admin)
}

/// Mark (or clear) the default / favourite user auto-unlocked on launch. Stored
/// in the shared user-partitions DB so it round-trips with the CLI `users -i`
/// Fav verb (no longer browser-local localStorage). Self-or-admin, matching the
/// other Manage Users mutations. Per Ehud #311 (D1).
#[tauri::command]
pub async fn user_partitions_set_default_user(
    app: AppHandle,
    user_id: i64,
    is_default: bool,
) -> Result<(), String> {
    init_or_migrate(&app)?;
    let mut conn = open_or_init(&app)?;
    ensure_user_can_modify(&conn, user_id)?;
    set_default_user(&mut conn, user_id, is_default)
}

#[tauri::command]
pub async fn user_partitions_admin_reset_passphrase(
    app: AppHandle,
    user_id: i64,
    mut new_passphrase: Option<String>,
) -> Result<(), String> {
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let mut conn = open_or_init(&app)?;
    let result =
        admin_reset_user_passphrase(&mut conn, &root_key, user_id, new_passphrase.as_deref());
    if let Some(passphrase) = new_passphrase.as_mut() {
        passphrase.zeroize();
    }
    root_key.zeroize();
    result
}

#[tauri::command]
pub async fn user_partitions_storage_stats(
    app: AppHandle,
) -> Result<Vec<UserStorageStats>, String> {
    init_or_migrate(&app)?;
    let conn = open_or_init(&app)?;
    user_storage_stats(&conn)
}

/// Serializes the tests that prime, hold or clear the session, or that read
/// what clearing it cancels, here and in the modules whose transactions go
/// through a partition. The session is process-wide: a test that changed it
/// without this lock would take it from another test in the middle of a
/// partition call, so [`clear_user_session`] and [`set_user_session`] refuse
/// to run in a test on a thread that does not hold it.
#[cfg(test)]
pub(crate) fn test_lock() -> TestLock {
    static USER_PARTITION_TEST_LOCK: Mutex<()> = Mutex::new(());
    // Recover from poisoning so a single failing test does not cascade
    // into the rest of the suite as PoisonError.
    let guard = USER_PARTITION_TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    HOLDS_TEST_LOCK.set(true);
    TestLock { _guard: guard }
}

#[cfg(test)]
pub(crate) struct TestLock {
    _guard: MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for TestLock {
    fn drop(&mut self) {
        HOLDS_TEST_LOCK.set(false);
    }
}

#[cfg(test)]
thread_local! {
    static HOLDS_TEST_LOCK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread holds [`test_lock`].
#[cfg(test)]
pub(crate) fn holds_test_lock() -> bool {
    HOLDS_TEST_LOCK.get()
}

#[cfg(test)]
fn assert_test_lock_held() {
    assert!(
        HOLDS_TEST_LOCK.get(),
        "a test changed the user session without holding user_partitions::test_lock()"
    );
}

/// A partition database file whose active account has a passphrase and is
/// unlocked: the kind of account whose partition calls take the session.
/// Returns the file and the account.
/// Lays `profiles` down as `user_id`'s rows the way a build before unique
/// profile names stored them, names as given. The account opens with this
/// machine's key (no passphrase).
#[cfg(test)]
pub(crate) fn store_profiles_as_an_older_build(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    profiles: &[Value],
) {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let row = read_user_key_row(conn, user_id).expect("the account");
    let dek = unwrap_user_dek_with_key(&row, &root_secret).expect("an account without passphrase");
    insert_profile_rows(conn, &root_secret, user_id, &dek, profiles).expect("the rows");
}

#[cfg(test)]
pub(crate) fn passphrase_account_db(dir: &std::path::Path, root_key: &[u8; 32]) -> (PathBuf, i64) {
    let path = dir.join(DB_FILENAME);
    let mut conn = open_or_init_path(path.clone()).expect("open the partition database");
    migrate_legacy_payloads(&mut conn, None, None, root_key).expect("migrate");
    let user = create_user(
        &mut conn,
        root_key,
        "Locked",
        None,
        None,
        Some("passphrase"),
    )
    .expect("create the account");
    unlock_user(&conn, root_key, user.id, Some("passphrase")).expect("unlock the account");
    (path, user.id)
}

/// Runs `write` on its own connection and thread while this thread holds the
/// session of `user_id` ([`passphrase_account_db`]), and checks that `write`
/// does not hold the database's reservation while it waits for that session:
/// another connection writes all along, with no busy timeout to wait one out.
/// Returns what `write` returned once the session is released.
#[cfg(test)]
pub(crate) fn wait_for_the_session_without_reserving<T: Send + 'static>(
    path: &std::path::Path,
    user_id: i64,
    write: impl FnOnce(Connection) -> T + Send + 'static,
) -> T {
    use std::time::{Duration, Instant};
    let conn = open_or_init_path(path.to_path_buf()).expect("open the writer");
    let other = open_or_init_path(path.to_path_buf()).expect("open the other writer");
    other
        .execute_batch("CREATE TABLE IF NOT EXISTS session_order_probe(x INTEGER)")
        .expect("probe table");
    other.busy_timeout(Duration::ZERO).expect("no busy timeout");
    let session = hold_user_session(&other, user_id);
    let held =
        HELD_SESSION.with_borrow(|held| held.as_ref().and_then(Option::as_ref).map(|(id, _)| *id));
    let writer = std::thread::spawn(move || write(conn));
    // The writer reaches the session at once. One that holds the reservation
    // while it waits refuses every write from then on.
    let until = Instant::now() + Duration::from_millis(300);
    let mut refused = None;
    while refused.is_none() && Instant::now() < until {
        refused = other
            .execute("INSERT INTO session_order_probe VALUES (1)", [])
            .err();
        std::thread::sleep(Duration::from_millis(5));
    }
    // Released before any assertion: a panic while holding it would poison
    // the session for the rest of the suite.
    drop(session);
    let written = writer.join().expect("the writer");
    assert_eq!(held, Some(user_id), "the session held is not the account's");
    assert!(
        refused.is_none(),
        "a write was refused while the writer waited for the session: {refused:?}"
    );
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use serde_json::json;

    /// A running deletion is settled as completed once it can never end
    /// (its process gone, its pid another process's, its lease run out on a
    /// machine this one cannot look at, or this process's own that no longer
    /// runs here), and only then: a live process of this machine holds its
    /// key however long it takes.
    #[test]
    fn a_deletion_is_settled_only_once_it_can_never_end() {
        let this = this_process();
        let now = now_ms();
        let stale = now - CHAIN_DELETION_LEASE_MS - 1;
        let running = |process: &str, space: Option<String>, pid: u32, start: Option<u64>, at| {
            RunningDeletion {
                id: hex::encode(crate::crypto::random_bytes(8)),
                process: process.to_string(),
                space,
                pid,
                start,
                at,
            }
        };
        let elsewhere = Some("another machine".to_string());
        let mut cases = vec![
            (
                "another machine, just begun",
                running("other", elsewhere.clone(), 1, Some(1), now),
                false,
            ),
            (
                "another machine, its lease run out",
                running("other", elsewhere, 1, Some(1), stale),
                true,
            ),
            (
                "no pid space, just begun",
                running("other", None, 1, None, now),
                false,
            ),
            (
                "no pid space, its lease run out",
                running("other", None, 1, None, stale),
                true,
            ),
        ];
        #[cfg(target_os = "linux")]
        {
            // This test's own process stands for a live one: its stat is
            // readable wherever the test runs (`hidepid` hides only other
            // users' processes, PID 1 among them), and its token is not this
            // deletion's, so the pid is what decides.
            let live = std::process::id();
            let live_start = process_start(live)
                .expect("this process's own stat is readable")
                .expect("this process runs");
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--list")
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let gone = child.id();
            child.wait().unwrap();
            let here = this.space.clone();
            assert!(here.is_some(), "the pid space of this process is read");
            cases.extend([
                (
                    "a live process here, past any lease",
                    running("other", here.clone(), live, Some(live_start), stale),
                    false,
                ),
                (
                    "its pid now another process's",
                    running("other", here.clone(), live, Some(live_start + 1), now),
                    true,
                ),
                (
                    "a process here that is gone",
                    running("other", here, gone, Some(live_start), now),
                    true,
                ),
            ]);
        }
        let ours = ChainDeletion {
            id: hex::encode(crate::crypto::random_bytes(8)),
        };
        deletions_running_here().insert(ours.id.clone());
        let mut own = running(&this.token, this.space.clone(), this.pid, this.start, stale);
        own.id = ours.id.clone();
        cases.extend([
            ("this process's, running here", own, false),
            (
                "this process's, no longer running",
                running(&this.token, this.space.clone(), this.pid, this.start, now),
                true,
            ),
        ]);
        let left_over = cases.iter().filter(|(_, _, left)| *left).count();
        for (case, deletion, left) in &cases {
            assert_eq!(deletion_left_over(deletion, now), *left, "{case}");
        }
        let mut record = DeletionRecord {
            generation: 3,
            running: cases.into_iter().map(|(_, deletion, _)| deletion).collect(),
        };
        let kept = record.running.len() - left_over;
        assert!(record.settle(now));
        assert_eq!(
            record.view(),
            ChainDeletions {
                generation: 3 + left_over as u64,
                in_progress: kept,
            }
        );
        drop(ours);
    }

    /// A partition call made under the session its thread holds reads the
    /// session from the hold: the lock is not reentrant, and taking it again
    /// would wait on its own thread for good.
    #[test]
    fn a_partition_call_under_a_held_session_does_not_wait_on_its_own_thread() {
        let _guard = test_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, user_id) = passphrase_account_db(dir.path(), &test_root());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let conn = open_or_init_path(path).expect("open");
            let root_secret = user_crypto::secret_key_from_bytes(&test_root());
            let _session = hold_user_session(&conn, user_id);
            let nested = with_user_dek(&conn, &root_secret, user_id, |user_id, _| {
                set_user_setting_for(&conn, &test_root(), user_id, "nested", &json!(1))?;
                get_user_setting_for(&conn, &test_root(), user_id, "nested")
            });
            let _ = sender.send(nested);
        });
        let nested = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the nested calls waited on the session their own thread holds");
        assert_eq!(nested, Ok(Some(json!(1))));
    }

    /// The read-modify-write of a profile list takes the session before the
    /// reservation, like every other partition writer.
    #[test]
    fn a_profile_update_takes_the_session_before_the_reservation() {
        let _guard = test_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, user_id) = passphrase_account_db(dir.path(), &test_root());
        let (profiles, written) =
            wait_for_the_session_without_reserving(&path, user_id, move |mut conn| {
                update_server_profiles_for(&mut conn, &test_root(), user_id, |profiles| {
                    profiles.push(json!({"id": "p", "name": "P", "protocol": "sftp"}));
                    true
                })
            })
            .expect("the update");
        assert!(written);
        assert_eq!(profiles.len(), 1);
    }

    #[test]
    fn gui_account_display_edits_preserve_owner_but_key_changes_do_not() {
        let _guard = test_lock();
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_legacy_payloads(&mut conn, None, None, &test_root()).unwrap();
        let before = gui_account_snapshot_from(&conn).unwrap();
        let user = before.user.unwrap();
        rename_user(&conn, user, "Renamed fixture").unwrap();
        set_user_avatar(&conn, user, Some("X"), Some("#123456")).unwrap();
        reorder_users(&mut conn, &[user]).unwrap();
        set_default_user(&mut conn, user, true).unwrap();
        assert!(before == gui_account_snapshot_from(&conn).unwrap());
        conn.execute(
            "UPDATE users SET dek_verifier = ?1 WHERE id = ?2",
            params![vec![0x55u8; 32], user],
        )
        .unwrap();
        assert!(before != gui_account_snapshot_from(&conn).unwrap());
    }

    #[test]
    fn gui_owner_changed_during_drain_cannot_publish_a_connection() {
        let _guard = test_lock();
        let mut conn = Connection::open_in_memory().unwrap();
        init_db_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO global_state(key,value,updated_at) VALUES(?1,'1',1)",
            [ACTIVE_USER_KEY],
        )
        .unwrap();
        let before = gui_account_snapshot_from(&conn).unwrap();
        // Account selection changed while the asynchronous drain was away.
        conn.execute(
            "UPDATE global_state SET value='2',updated_at=2 WHERE key=?1",
            [ACTIVE_USER_KEY],
        )
        .unwrap();
        let published = std::cell::Cell::new(false);
        assert!(with_gui_account_snapshot_in(&mut conn, &before, || {
            published.set(true);
            Ok(())
        })
        .is_err());
        assert!(!published.get());
        let current = gui_account_snapshot_from(&conn).unwrap();
        with_gui_account_snapshot_in(&mut conn, &current, || {
            published.set(true);
            Ok(())
        })
        .unwrap();
        assert!(published.get());
    }

    #[test]
    fn gui_account_activation_revision_rejects_aba_even_in_one_millisecond() {
        let mut conn = Connection::open_in_memory().unwrap();
        init_db_schema(&conn).unwrap();
        let tx = conn.transaction().unwrap();
        upsert_global_state(&tx, ACTIVE_USER_KEY, "1", 123).unwrap();
        upsert_global_state(&tx, ACTIVE_USER_KEY, "2", 123).unwrap();
        upsert_global_state(&tx, ACTIVE_USER_KEY, "1", 122).unwrap();
        let (value, revision): (String, i64) = tx
            .query_row(
                "SELECT value, updated_at FROM global_state WHERE key = ?1",
                [ACTIVE_USER_KEY],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(value, "1");
        assert_eq!(revision, 125);
    }

    fn test_root() -> [u8; 32] {
        [7u8; 32]
    }

    fn throwaway_vault(dir: &std::path::Path) -> CredentialStore {
        let path = dir.join("vault.db");
        crate::credential_store::CredentialStore::write_test_vault(&path, &[0x24; 32]);
        CredentialStore::from_verified_key(&path, &[0x24; 32])
    }

    /// The account a save or delete mirrors into is read before the vault
    /// write, which may wait for another process's lock: an account switched to
    /// during that wait is not the one that receives (or loses) the secret.
    #[test]
    fn dual_writes_use_the_account_active_before_the_vault_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = throwaway_vault(dir.path());
        let active = std::cell::Cell::new(1i64);
        let mirrored = std::cell::Cell::new(None);
        store_credential_dual_with(
            &store,
            "server_switch",
            "secret",
            || {
                assert!(store.get("server_switch").is_err(), "read after the write");
                let user = active.get();
                // The switch lands while the vault write waits.
                active.set(2);
                Ok(Some(user))
            },
            |user_id| {
                mirrored.set(Some(user_id));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(store.get("server_switch").unwrap(), "secret");
        assert_eq!(mirrored.get(), Some(1));

        active.set(1);
        let unmirrored = std::cell::Cell::new(None);
        delete_credential_dual_with(
            &store,
            "server_switch",
            || {
                assert!(store.get("server_switch").is_ok(), "read after the delete");
                let user = active.get();
                active.set(2);
                Some(user)
            },
            |user_id| {
                unmirrored.set(Some(user_id));
                Ok(())
            },
        )
        .unwrap();
        assert!(store.get("server_switch").is_err());
        assert_eq!(unmirrored.get(), Some(1));
    }

    /// A failed account lookup stops the save before the vault changes.
    #[test]
    fn a_failed_account_lookup_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = throwaway_vault(dir.path());
        let err = store_credential_dual_with(
            &store,
            "server_lookup",
            "secret",
            || Err("lookup failed".to_string()),
            |_| panic!("mirrored without an account"),
        )
        .unwrap_err();
        assert_eq!(err, "lookup failed");
        assert!(store.get("server_lookup").is_err());
    }

    /// #736: the legacy blob follows the active list, and a list that is
    /// already there (same values, different key order) is not rewritten.
    #[test]
    fn gui_account_guard_refuses_foreign_locked_and_aba_sessions() {
        let _guard = test_lock();
        clear_user_session();
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_legacy_payloads(&mut conn, None, None, &test_root()).unwrap();
        let original = active_user_id(&conn).unwrap();
        let generation = session_generation();
        let writes = std::cell::Cell::new(0);
        let run = || {
            writes.set(writes.get() + 1);
            Ok(())
        };
        with_gui_account(&mut conn, original, generation, run).unwrap();
        assert!(with_gui_account(&mut conn, Some(999), generation, run).is_err());
        clear_user_session(); // Same identity after a lock/unlock or switch round trip is still stale.
        assert!(with_gui_account(&mut conn, original, generation, run).is_err());
        let protected = create_user(
            &mut conn,
            &test_root(),
            "Protected fixture",
            None,
            None,
            Some("fixture-password"),
        )
        .unwrap();
        set_active_user(&conn, protected.id).unwrap();
        assert!(
            with_gui_account(&mut conn, Some(protected.id), session_generation(), run).is_err()
        );
        assert_eq!(writes.get(), 1);
    }

    #[test]
    fn legacy_blob_mirror_writes_only_a_different_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = throwaway_vault(dir.path());
        store
            .store(
                LEGACY_PROFILES_BLOB,
                r#"[{"name":"try","id":"srv_try"},{"name":"Drive","id":"srv_drive"}]"#,
            )
            .unwrap();

        let current = vec![json!({ "id": "srv_drive", "name": "Drive 2TB" })];
        assert!(mirror_active_profiles_to_legacy_blob(&store, &current).unwrap());
        let stored: Vec<Value> =
            serde_json::from_str(&store.get(LEGACY_PROFILES_BLOB).unwrap()).unwrap();
        assert_eq!(stored, current);

        store
            .store(
                LEGACY_PROFILES_BLOB,
                r#"[{"name":"Drive 2TB","id":"srv_drive"}]"#,
            )
            .unwrap();
        assert!(!mirror_active_profiles_to_legacy_blob(&store, &current).unwrap());
    }

    /// A Move drops the source secret only after the copy reached somewhere.
    #[test]
    fn a_move_keeps_the_source_secret_when_the_copy_could_not_be_written() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let source = active_user_id(&conn).unwrap().unwrap();
        let locked = create_user(&mut conn, &root, "Locked", None, None, Some("pass")).unwrap();
        let open = create_passphrase_less_user(&mut conn, &root, "Open", None, None).unwrap();
        clear_user_session();
        set_user_credential_for(&conn, &root, source, "server_old", "server", "secret").unwrap();
        let relocation = |target: i64| ProfileRelocation {
            source_profile_id: "old".into(),
            new_profile_id: "new".into(),
            profile_name: "P".into(),
            target_user_id: target,
            moved: true,
            inserted: true,
            already_present: false,
            protocol: "sftp".into(),
            secrets_not_copied: Vec::new(),
        };
        let read = |user: i64, key: &str| {
            get_user_credential_for(&conn, &root, user, key)
                .unwrap()
                .map(|value| value.to_string())
        };
        // Locked target and no vault: nowhere to put it, so the source stays.
        assert!(!relocate_secret_key_dual(
            &conn,
            None,
            &root,
            source,
            &relocation(locked.id),
            None,
            "server_old",
            "server_new",
            "server",
        ));
        assert_eq!(read(source, "server_old").as_deref(), Some("secret"));
        // An open target takes it, and the source goes.
        assert!(relocate_secret_key_dual(
            &conn,
            None,
            &root,
            source,
            &relocation(open.id),
            None,
            "server_old",
            "server_new",
            "server",
        ));
        assert_eq!(read(open.id, "server_new").as_deref(), Some("secret"));
        assert_eq!(read(source, "server_old"), None);
    }

    /// A token relocated between two accounts other than the legacy owner goes
    /// row to row: the vault keys of those names are the owner's, so neither
    /// the copy nor a Move's cleanup touches them. Into the owner's account
    /// the copy still reaches the vault, which that account reads.
    #[test]
    fn a_token_relocated_between_other_accounts_leaves_the_owners_vault_keys() {
        let _guard = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let store = throwaway_vault(dir.path());
        let mut conn = open_or_init_path(dir.path().join("partitions.db")).expect("open");
        let root = store.derive_user_partition_wrapping_key();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
        let source = create_passphrase_less_user(&mut conn, &root, "Source", None, None).unwrap();
        let target = create_passphrase_less_user(&mut conn, &root, "Target", None, None).unwrap();
        clear_user_session();
        for key in ["oauth_dropbox_old", "oauth_dropbox_new"] {
            store.store(key, "owners-token").unwrap();
        }
        set_user_credential_for(
            &conn,
            &root,
            source.id,
            "oauth_dropbox_old",
            "oauth",
            "token",
        )
        .unwrap();
        let relocation = |target: i64, moved: bool| ProfileRelocation {
            source_profile_id: "old".into(),
            new_profile_id: "new".into(),
            profile_name: "P".into(),
            target_user_id: target,
            moved,
            inserted: true,
            already_present: false,
            protocol: "dropbox".into(),
            secrets_not_copied: Vec::new(),
        };
        let read = |user: i64, key: &str| {
            get_user_credential_for(&conn, &root, user, key)
                .unwrap()
                .map(|value| value.to_string())
        };
        assert!(relocate_secret_key_dual(
            &conn,
            Some(&store),
            &root,
            source.id,
            &relocation(target.id, true),
            None,
            "oauth_dropbox_old",
            "oauth_dropbox_new",
            "oauth",
        ));
        assert_eq!(
            read(target.id, "oauth_dropbox_new").as_deref(),
            Some("token")
        );
        assert_eq!(read(source.id, "oauth_dropbox_old"), None);
        assert_eq!(store.get("oauth_dropbox_new").unwrap(), "owners-token");
        assert_eq!(store.get("oauth_dropbox_old").unwrap(), "owners-token");

        store.delete("oauth_dropbox_new").unwrap();
        assert!(relocate_secret_key_dual(
            &conn,
            Some(&store),
            &root,
            target.id,
            &relocation(owner, false),
            None,
            "oauth_dropbox_new",
            "oauth_dropbox_new",
            "oauth",
        ));
        assert_eq!(store.get("oauth_dropbox_new").unwrap(), "token");
        assert_eq!(
            read(target.id, "oauth_dropbox_new").as_deref(),
            Some("token")
        );
    }

    /// The account a relocation compares is read like every other reader reads
    /// it: another account's token from its row alone, never the owner's vault
    /// key of the same name, which would make two different sign-ins match.
    #[test]
    fn a_relocation_fingerprints_another_accounts_token_from_its_row_alone() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None).unwrap();
        clear_user_session();
        let (_keys, read_vault) = fake_vault(&[("oauth_dropbox_p", "owners-token")]);
        let resolve = |user: i64| {
            resolve_relocate_secret_with(&conn, &root, user, None, "dropbox", "p", &read_vault)
                .unwrap()
                .map(|value| value.to_string())
        };
        assert_eq!(resolve(other.id), None);
        assert_eq!(resolve(owner).as_deref(), Some("owners-token"));
        set_user_credential_for(&conn, &root, other.id, "oauth_dropbox_p", "oauth", "own").unwrap();
        assert_eq!(resolve(other.id).as_deref(), Some("own"));
    }

    /// MUV-6 for private tokens: what another account saved in the vault before
    /// its tokens were kept to its partition moves there, its own row winning
    /// over a stale vault copy, and leaves the vault. A profile id the legacy
    /// owner also holds, the owner's singleton and the app settings stay. A
    /// vault that refuses a delete leaves the account unmarked, and the next
    /// pass finishes; a settled account, and the owner, are passed over.
    #[test]
    fn another_accounts_tokens_saved_in_the_vault_move_to_its_partition() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None).unwrap();
        clear_user_session();
        let profile = |id: &str| {
            json!({"id": id, "name": id, "protocol": "dropbox", "host": "", "port": 0,
                   "username": format!("{id}@example.test")})
        };
        replace_server_profiles_for(
            &mut conn,
            &root,
            other.id,
            &[
                profile("mine"),
                profile("held"),
                profile("profile-0"),
                profile("1234_5678"),
            ],
        )
        .unwrap();
        set_user_credential_for(&conn, &root, other.id, "oauth_dropbox_held", "oauth", "row")
            .unwrap();
        let (keys, read) = fake_vault(&[
            ("github_pem_1234_5678", "-----PEM-----"),
            ("oauth_dropbox_mine", "mine-token"),
            ("profile_auth_config_mine", "mine-config"),
            ("oauth_dropbox_held", "stale-token"),
            ("oauth_dropbox_profile-0", "owners-token"),
            ("server_mine", "password"),
            ("oauth_dropbox", "owners-singleton"),
            ("oauth_dropbox_client_id", "app-id"),
        ]);
        let deleted = RefCell::new(Vec::<String>::new());
        let refuse = |_: &str, _: Option<&str>| Err("vault busy".to_string());
        let delete = |key: &str, _: Option<&str>| {
            deleted.borrow_mut().push(key.to_string());
            Ok(true)
        };
        let row = |key: &str| {
            get_user_credential_for(&conn, &root, other.id, key)
                .unwrap()
                .map(|value| value.to_string())
        };

        assert!(align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &refuse
        )
        .is_err());
        assert!(!private_tokens_aligned(&conn, other.id).unwrap());
        assert_eq!(row("oauth_dropbox_mine").as_deref(), Some("mine-token"));

        let moved = align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        let mut gone = deleted.borrow().clone();
        gone.sort();
        assert_eq!(
            gone,
            [
                "oauth_dropbox_held",
                "oauth_dropbox_mine",
                "profile_auth_config_mine"
            ]
        );
        assert_eq!(moved, 3);
        assert_eq!(row("oauth_dropbox_mine").as_deref(), Some("mine-token"));
        assert_eq!(
            row("profile_auth_config_mine").as_deref(),
            Some("mine-config")
        );
        assert_eq!(row("oauth_dropbox_held").as_deref(), Some("row"));
        assert_eq!(row("oauth_dropbox_profile-0"), None);
        assert_eq!(row("server_mine"), None);
        // A GitHub App's PEM is the machine's, whatever profile id reads like
        // its key: it stays in the vault, and every account reads it there.
        assert_eq!(row("github_pem_1234_5678"), None);
        assert_eq!(
            read_credential_with_fallback_inner(
                &conn,
                &root,
                other.id,
                "github_pem_1234_5678",
                &read
            )
            .unwrap()
            .as_deref()
            .map(String::as_str),
            Some("-----PEM-----")
        );

        deleted.borrow_mut().clear();
        assert_eq!(
            align_private_tokens_inner(
                &conn,
                &root,
                other.id,
                &keys,
                &|| Ok(keys.clone()),
                &read,
                &delete
            )
            .unwrap(),
            0
        );
        assert_eq!(
            align_private_tokens_inner(
                &conn,
                &root,
                owner,
                &keys,
                &|| Ok(keys.clone()),
                &read,
                &delete
            )
            .unwrap(),
            0
        );
        assert!(deleted.borrow().is_empty());
        assert!(private_tokens_aligned(&conn, owner).unwrap());
    }

    /// A private-token row the alignment inserted follows the vault chain a
    /// refresh rotated before the vault copy could leave; a row another
    /// writer moved keeps its value and its vault copy.
    #[test]
    fn a_row_the_alignment_inserted_follows_a_rotated_vault_chain() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let _owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
        let first = create_passphrase_less_user(&mut conn, &root, "First", None, None).unwrap();
        let second = create_passphrase_less_user(&mut conn, &root, "Second", None, None).unwrap();
        clear_user_session();
        let profile = |id: &str| {
            json!({"id": id, "name": id, "protocol": "dropbox", "host": "", "port": 0,
                   "username": format!("{id}@example.test")})
        };
        replace_server_profiles_for(&mut conn, &root, first.id, &[profile("mine-a")]).unwrap();
        replace_server_profiles_for(&mut conn, &root, second.id, &[profile("mine-b")]).unwrap();
        let key_a = "oauth_dropbox_mine-a".to_string();
        let key_b = "oauth_dropbox_mine-b".to_string();
        let vault = RefCell::new(HashMap::from([
            (key_a.clone(), "chain-a0".to_string()),
            (key_b.clone(), "chain-b0".to_string()),
        ]));
        let keys = vec![key_a.clone(), key_b.clone()];
        let read = |key: &str| -> Result<Option<Zeroizing<String>>, String> {
            Ok(vault.borrow().get(key).map(|v| Zeroizing::new(v.clone())))
        };
        // The vault copy leaves only while it still holds the value the pass
        // read; the first attempt of each key finds it rotated and refuses.
        let rotated = RefCell::new(HashSet::new());
        let delete = |key: &str, expected: Option<&str>| -> Result<bool, String> {
            if rotated.borrow_mut().insert(key.to_string()) {
                if let Some(slot) = vault.borrow_mut().get_mut(key) {
                    *slot = format!("rotated-{key}");
                }
            }
            let mut map = vault.borrow_mut();
            match (map.get(key), expected) {
                (Some(current), Some(expected)) if current == expected => {
                    map.remove(key);
                    Ok(true)
                }
                _ => Ok(false),
            }
        };
        let list = || Ok(keys.clone());
        let row = |user: i64, key: &str| {
            get_user_credential_for(&conn, &root, user, key)
                .unwrap()
                .map(|value| value.to_string())
        };
        let align = |user: i64| {
            align_private_tokens_inner(&conn, &root, user, &keys, &list, &read, &delete).unwrap()
        };

        // First pass inserts chain-a0, the vault rotates before the delete,
        // the pass stays incomplete; the second moves the row to the rotated
        // chain and the vault copy leaves.
        align(first.id);
        assert_eq!(row(first.id, &key_a).as_deref(), Some("chain-a0"));
        assert_eq!(
            vault.borrow().get(&key_a).map(String::as_str),
            Some("rotated-oauth_dropbox_mine-a")
        );
        align(first.id);
        assert_eq!(
            row(first.id, &key_a).as_deref(),
            Some("rotated-oauth_dropbox_mine-a")
        );
        assert!(!vault.borrow().contains_key(&key_a));

        // Second account: same rotation, then another writer moves the row
        // to a value the alignment never wrote; the row keeps it and the
        // vault keeps the rotated chain.
        align(second.id);
        assert_eq!(row(second.id, &key_b).as_deref(), Some("chain-b0"));
        set_user_credential_for(&conn, &root, second.id, &key_b, "oauth", "writer-c").unwrap();
        align(second.id);
        assert_eq!(row(second.id, &key_b).as_deref(), Some("writer-c"));
        assert_eq!(
            vault.borrow().get(&key_b).map(String::as_str),
            Some("rotated-oauth_dropbox_mine-b")
        );
    }

    /// A profile id the legacy owner and another account both hold: the vault
    /// copy of its tokens is the owner's, so the other account's migration and
    /// alignment leave it, and the other account signs in by itself. Each then
    /// reads its own token. The other account's own profile still reaches its
    /// partition.
    #[test]
    fn a_profile_both_hold_keeps_the_owners_token_from_the_other_account() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None).unwrap();
        clear_user_session();
        let profile = |id: &str| {
            json!({"id": id, "name": id, "protocol": "dropbox", "host": "", "port": 0,
                   "username": format!("{id}@example.test")})
        };
        replace_server_profiles_for(
            &mut conn,
            &root,
            other.id,
            &[profile("profile-0"), profile("mine")],
        )
        .unwrap();
        let (keys, read) = fake_vault(&[
            ("oauth_dropbox_profile-0", "owners-token"),
            ("jottacloud_refresh_profile-0", "owners-chain"),
            ("oauth_dropbox_mine", "mine-token"),
        ]);
        let deleted = RefCell::new(Vec::<String>::new());
        let delete = |key: &str, _: Option<&str>| {
            deleted.borrow_mut().push(key.to_string());
            Ok(true)
        };
        let row = |key: &str| {
            get_user_credential_for(&conn, &root, other.id, key)
                .unwrap()
                .map(|value| value.to_string())
        };

        migrate_user_credentials_inner(&conn, &root, other.id, &keys, &read).unwrap();
        assert_eq!(row("oauth_dropbox_profile-0"), None);
        assert_eq!(row("jottacloud_refresh_profile-0"), None);
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(row("oauth_dropbox_profile-0"), None);
        assert_eq!(row("jottacloud_refresh_profile-0"), None);
        assert_eq!(row("oauth_dropbox_mine").as_deref(), Some("mine-token"));
        assert_eq!(*deleted.borrow(), ["oauth_dropbox_mine"]);

        set_user_credential_for(
            &conn,
            &root,
            other.id,
            "oauth_dropbox_profile-0",
            "oauth",
            "others-token",
        )
        .unwrap();
        let reads = |user: i64| {
            read_credential_with_fallback_inner(
                &conn,
                &root,
                user,
                "oauth_dropbox_profile-0",
                &read,
            )
            .unwrap()
            .map(|value| value.to_string())
        };
        assert_eq!(reads(other.id).as_deref(), Some("others-token"));
        assert_eq!(reads(owner).as_deref(), Some("owners-token"));
    }

    /// A private token's sign-in is saved for one account: the active one when
    /// it holds the profile, or the single holder; with several holders and
    /// none of them active it is refused, never written to all of them.
    #[test]
    fn a_private_sign_in_is_saved_for_one_account_or_refused() {
        assert_eq!(private_token_signer(vec![2, 3], Some(3)).unwrap(), [3]);
        assert_eq!(private_token_signer(vec![2], Some(3)).unwrap(), [2]);
        assert_eq!(private_token_signer(vec![2], None).unwrap(), [2]);
        for active in [Some(4), None] {
            let refused = private_token_signer(vec![2, 3], active).unwrap_err();
            assert!(refused.starts_with(ACCOUNT_CHANGED), "{refused}");
        }
    }

    /// A throwaway vault, outside any partition database.
    /// A write whose mirror fails puts the vault key back to the value that
    /// write replaced, read under its own vault lock: a value another writer
    /// stored before the write (here between its start and its write) is
    /// kept, and so is one stored after it; uncontended, the key goes back to
    /// the value it held.
    #[test]
    fn a_failed_mirror_puts_back_the_value_its_write_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let store = throwaway_vault(dir.path());
        let key = "server_raced";
        let refused = || Err::<(), _>("refused".to_string());

        store.store(key, "a").unwrap();
        let raced = throwaway_vault(dir.path());
        BEFORE_VAULT_WRITE.set(Some(Box::new(move || raced.store(key, "b").unwrap())));
        assert!(store_vault_then_mirror(&store, key, "c", refused).is_err());
        assert_eq!(store.get(key).unwrap(), "b", "a value stored meanwhile");

        let after = |value: &'static str| {
            let store = &store;
            move || {
                store.store(key, value).unwrap();
                Err::<(), _>("refused".to_string())
            }
        };
        assert!(store_vault_then_mirror(&store, key, "c", after("d")).is_err());
        assert_eq!(
            store.get(key).unwrap(),
            "d",
            "a value stored after the write"
        );

        assert!(store_vault_then_mirror(&store, key, "e", refused).is_err());
        assert_eq!(store.get(key).unwrap(), "d", "uncontended");
        store.delete(key).unwrap();
        assert!(store_vault_then_mirror(&store, key, "f", refused).is_err());
        assert!(store.get(key).is_err(), "no value before, none after");
    }

    /// The CLI vault and partition database of an isolated child
    /// ([`crate::provider_commands::isolated_account_fixture`]), opened and
    /// migrated: the legacy owner is the active account.
    fn isolated_cli_store() -> CredentialStore {
        CredentialStore::clear_cache();
        CredentialStore::bootstrap_master_password("synthetic partition fixture").unwrap();
        CredentialStore::unlock_with_master("synthetic partition fixture", None).unwrap();
        let store = CredentialStore::from_cache().unwrap();
        init_or_migrate_cli(&store).unwrap();
        store
    }

    /// The backup a rebuild takes holds a transaction still in the database's
    /// write-ahead log (a reader keeps it from being checkpointed), not only
    /// what the main file held.
    #[test]
    fn a_rebuild_snapshot_keeps_what_the_wal_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DB_FILENAME);
        let writer = open_or_init_path(path.clone()).unwrap();
        writer
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader
            .query_row("SELECT COUNT(*) FROM global_state", [], |row| row.get(0))
            .unwrap();
        writer
            .execute(
                "INSERT INTO global_state(key, value, updated_at) VALUES ('probe', 'kept', 0)",
                [],
            )
            .unwrap();
        let backup = snapshot_before_rebuild(&path).unwrap();
        let alone = tempfile::tempdir().unwrap();
        let copy = alone.path().join("copy.db");
        std::fs::copy(&backup, &copy).unwrap();
        let value: Option<String> = Connection::open(&copy)
            .unwrap()
            .query_row(
                "SELECT value FROM global_state WHERE key = 'probe'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(
            value.as_deref(),
            Some("kept"),
            "the backup lost a committed write"
        );
        drop(reader);
    }

    /// A rebuild holds off a credential writer that takes no list lock from
    /// before its snapshot to the removal: a commit tried after the snapshot
    /// fails busy, so the backup misses no commit of the removed database.
    #[test]
    fn a_credential_commit_after_the_rebuild_snapshot_is_held_off() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DB_FILENAME);
        let mut conn = open_or_init_path(path.clone()).expect("open");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
        clear_user_session();
        set_user_credential_for(&conn, &root, owner, "oauth_box_p", "oauth", "old").unwrap();
        drop(conn);
        let attempt = std::rc::Rc::new(RefCell::new(None));
        let (hook_path, seen) = (path.clone(), attempt.clone());
        AFTER_REBUILD_SNAPSHOT.set(Some(Box::new(move || {
            let writer = Connection::open(&hook_path).unwrap();
            writer.busy_timeout(std::time::Duration::ZERO).unwrap();
            *seen.borrow_mut() = Some(set_user_credential_for(
                &writer,
                &test_root(),
                owner,
                "oauth_box_p",
                "oauth",
                "new",
            ));
        })));
        let backup = remove_partitions_db_for_rebuild(&path).unwrap().unwrap();
        let emptied = Connection::open(&path).unwrap();
        let objects: i64 = emptied
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(objects, 0, "the database is emptied, not removed");
        let attempt = attempt.borrow_mut().take().expect("the hook ran");
        assert!(attempt.is_err(), "a commit after the snapshot: {attempt:?}");
        let alone = tempfile::tempdir().unwrap();
        let copy = alone.path().join("copy.db");
        std::fs::copy(&backup, &copy).unwrap();
        assert_eq!(
            get_user_credential_for(
                &Connection::open(&copy).unwrap(),
                &root,
                owner,
                "oauth_box_p"
            )
            .unwrap()
            .as_deref()
            .map(String::as_str),
            Some("old")
        );
    }

    /// A column another connection added between a migration's check and its
    /// `ALTER TABLE` (the GUI and the CLI opening the database at once) is
    /// taken as added, for each column migration, where the duplicate column
    /// failed the open; the first admin the winner seeded stays the only one.
    #[test]
    fn a_column_another_connection_added_since_the_check_is_taken_as_added() {
        let _guard = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("older.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY);
             INSERT INTO users(id) VALUES (1), (2);
             CREATE TABLE server_profiles (id INTEGER PRIMARY KEY, user_id INTEGER);",
        )
        .unwrap();
        let racing = |path: &std::path::Path, ddl: &'static str| {
            let path = path.to_path_buf();
            BEFORE_COLUMN_ADD.set(Some(Box::new(move || {
                Connection::open(&path).unwrap().execute_batch(ddl).unwrap();
            })));
        };

        racing(
            &path,
            "ALTER TABLE users ADD COLUMN is_admin INTEGER NOT NULL DEFAULT 0;
             UPDATE users SET is_admin = 1 WHERE id = 1;",
        );
        let admin = ensure_users_is_admin_column(&conn);
        racing(
            &path,
            "ALTER TABLE users ADD COLUMN is_default INTEGER NOT NULL DEFAULT 0;",
        );
        let default = ensure_users_is_default_column(&conn);
        racing(
            &path,
            "ALTER TABLE server_profiles ADD COLUMN token_uid TEXT;",
        );
        let token_uid = ensure_server_profiles_token_uid_column(&conn);

        assert_eq!(admin, Ok(()), "is_admin");
        assert_eq!(default, Ok(()), "is_default");
        assert_eq!(token_uid, Ok(()), "token_uid");
        let admins: i64 = conn
            .query_row("SELECT COUNT(*) FROM users WHERE is_admin = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(admins, 1, "one first admin");

        // The v2 -> v3 upgrade checks for the column before its transaction.
        let path = dir.path().join("v2.db");
        let mut conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY);
             INSERT INTO users(id) VALUES (1), (2);
             CREATE TABLE global_state (
                 key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        racing(
            &path,
            "ALTER TABLE users ADD COLUMN is_admin INTEGER NOT NULL DEFAULT 0;",
        );
        assert_eq!(upgrade_v2_to_v3(&mut conn), Ok(()), "v2 -> v3");
        assert_eq!(current_schema_version(&conn).unwrap().as_deref(), Some("3"));
        let admins: Vec<i64> = conn
            .prepare("SELECT id FROM users WHERE is_admin = 1")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(admins, [1], "the upgrade seeds the first admin");
    }

    /// A rebuild takes the session before the database's write lock, the order
    /// a passphrase account's credential writer takes them in: while the
    /// rebuild holds the write lock no other thread can take the session, so a
    /// writer that holds the session is never left waiting on the rebuild's
    /// write lock until its busy timeout fails the write. Right before the
    /// rebuild takes the write lock, the session is already its own and the
    /// write lock still free; the session is gone after the rebuild.
    #[test]
    fn a_rebuild_takes_the_session_before_its_write_lock() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DB_FILENAME);
        let mut conn = open_or_init_path(path.clone()).expect("open");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let locked = create_user(&mut conn, &root, "Locked", None, None, Some("pass")).unwrap();
        unlock_user(&conn, &root, locked.id, Some("pass")).unwrap();
        assert_eq!(session_user_id(), Some(locked.id));
        drop(conn);
        let before = std::rc::Rc::new(RefCell::new(None));
        let (seen_before, lock_path) = (before.clone(), path.clone());
        BEFORE_REBUILD_WRITE_LOCK.set(Some(Box::new(move || {
            // Another thread finds the session taken, and another connection
            // the write lock free: the session came first.
            let session_free = std::thread::spawn(|| USER_SESSION.try_lock().is_ok())
                .join()
                .unwrap();
            let other = Connection::open(&lock_path).unwrap();
            other.busy_timeout(std::time::Duration::ZERO).unwrap();
            let write_lock_free = other.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").is_ok();
            *seen_before.borrow_mut() = Some((session_free, write_lock_free));
        })));
        let free = std::rc::Rc::new(RefCell::new(None));
        let seen = free.clone();
        AFTER_REBUILD_SNAPSHOT.set(Some(Box::new(move || {
            // A passphrase account's writer, on its own thread, taking the
            // session while the rebuild holds the database's write lock.
            let taken = std::thread::spawn(|| USER_SESSION.try_lock().is_ok())
                .join()
                .unwrap();
            *seen.borrow_mut() = Some(taken);
        })));
        remove_partitions_db_for_rebuild(&path).unwrap().unwrap();
        assert_eq!(
            *before.borrow(),
            Some((false, true)),
            "(session free, write lock free) as the rebuild takes the write lock"
        );
        assert_eq!(
            *free.borrow(),
            Some(false),
            "the session is the rebuild's while it holds the write lock"
        );
        assert_eq!(session_user_id(), None, "the rebuild dropped the session");
    }

    /// CodeRabbit 4657339482: on the damaged-database path a writer landing
    /// a commit between the failed `BEGIN IMMEDIATE` and the removal could
    /// not be told apart from the damaged file itself — `PRAGMA
    /// data_version` is per connection, so it proves nothing — so the
    /// rebuild refuses instead of removing: the file a writer replaced
    /// stays, commits and all.
    #[test]
    fn a_commit_in_the_damaged_removal_gap_keeps_the_file() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DB_FILENAME);
        std::fs::write(&path, b"not a sqlite database").unwrap();
        let writer_path = path.clone();
        AFTER_REBUILD_SNAPSHOT.set(Some(Box::new(move || {
            // A writer in the gap the removal would have opened: it
            // replaces the damaged file with a database of its own and
            // commits to it. The rebuild's guard still holds the file
            // open, so the image is written over it, not renamed in.
            let image = writer_path.with_extension("raced-image.db");
            let conn = Connection::open(&image).unwrap();
            conn.execute_batch(
                "CREATE TABLE raced (id INTEGER PRIMARY KEY);
                 INSERT INTO raced(id) VALUES (1);",
            )
            .unwrap();
            drop(conn);
            std::fs::copy(&image, &writer_path).unwrap();
            let _ = std::fs::remove_file(&image);
        })));
        let refused = remove_partitions_db_for_rebuild(&path);
        let error = refused.expect_err("a commit in the gap was not seen");
        assert!(error.contains("being written"), "{error}");
        assert!(path.exists(), "the file a writer touched stays");
        let raced: i64 = Connection::open(&path)
            .unwrap()
            .query_row("SELECT id FROM raced", [], |row| row.get(0))
            .unwrap();
        assert_eq!(raced, 1, "the writer's commit is still there");
    }

    /// A connection that began with no account reads the legacy blob while no
    /// account exists, and the legacy owner's list once one does: the blob
    /// then mirrors whichever account is active, here another one.
    #[test]
    fn a_legacy_only_list_is_the_legacy_owners_once_accounts_exist() {
        if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
            "user_partitions::tests::a_legacy_only_list_is_the_legacy_owners_once_accounts_exist",
        ) {
            return;
        }
        let _guard = test_lock();
        CredentialStore::clear_cache();
        CredentialStore::bootstrap_master_password("synthetic partition fixture").unwrap();
        CredentialStore::unlock_with_master("synthetic partition fixture", None).unwrap();
        let store = CredentialStore::from_cache().unwrap();
        let list = |profiles: &[&str]| {
            Value::Array(
                profiles
                    .iter()
                    .map(|id| json!({"id": id, "name": id, "protocol": "sftp", "host": "h"}))
                    .collect(),
            )
            .to_string()
        };
        let ids = |store: &CredentialStore| -> Vec<String> {
            mcp_list_server_profiles(store, Some(LEGACY_ONLY_ACCOUNT))
                .unwrap()
                .iter()
                .filter_map(|p| p["id"].as_str().map(str::to_string))
                .collect()
        };
        store
            .store("config_server_profiles", &list(&["p"]))
            .unwrap();
        assert_eq!(ids(&store), vec!["p".to_string()], "no account: the blob");
        init_or_migrate_cli(&store).unwrap();
        let mut conn = open_or_init_cli().unwrap();
        let root = Zeroizing::new(store.derive_user_partition_wrapping_key());
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None).unwrap();
        replace_server_profiles_for(
            &mut conn,
            &root,
            other.id,
            &[json!({
                "id": "q", "name": "q", "protocol": "sftp", "host": "h"
            })],
        )
        .unwrap();
        set_active_user(&conn, other.id).unwrap();
        store
            .store("config_server_profiles", &list(&["q"]))
            .unwrap();
        assert_eq!(
            ids(&store),
            vec!["p".to_string()],
            "the legacy-only list followed the active account's mirror"
        );
    }

    /// An active typed save reads the account once: the legacy owner's token
    /// goes to its vault key and its own row, never to the row of an account
    /// switched to between the vault write and the mirror.
    #[test]
    fn an_active_typed_save_mirrors_into_the_account_it_began_with() {
        if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
            "user_partitions::tests::an_active_typed_save_mirrors_into_the_account_it_began_with",
        ) {
            return;
        }
        let _guard = test_lock();
        let store = isolated_cli_store();
        let mut conn = open_or_init_cli().unwrap();
        let root = Zeroizing::new(store.derive_user_partition_wrapping_key());
        let owner = active_user_id(&conn).unwrap().unwrap();
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None)
            .unwrap()
            .id;
        set_active_user(&conn, owner).unwrap();
        let row = |user: i64, key: &str| {
            get_user_credential_for(&open_or_init_cli().unwrap(), &root, user, key)
                .unwrap()
                .map(|value| value.to_string())
        };

        store_active_credential_typed_dual(&store, "oauth_box_steady", "oauth", "o-steady")
            .unwrap();
        assert_eq!(row(owner, "oauth_box_steady").as_deref(), Some("o-steady"));
        assert_eq!(store.get("oauth_box_steady").unwrap(), "o-steady");

        BEFORE_MIRROR.set(Some(Box::new(move || {
            set_active_user(&open_or_init_cli().unwrap(), other).unwrap();
        })));
        store_active_credential_typed_dual(&store, "oauth_box_x", "oauth", "o-token").unwrap();
        assert_eq!(row(owner, "oauth_box_x").as_deref(), Some("o-token"));
        assert_eq!(row(other, "oauth_box_x"), None, "the account switched to");
    }

    /// An unstamped copy of the vault's value in an account other than the
    /// legacy owner, readable until profile tags turn pending for a locked
    /// account: the fixture of the refresh tests below. Returns the store,
    /// the account and its partition root key.
    fn a_row_withheld_at_write_time(
        key: &str,
        kind: &str,
        value: &str,
    ) -> (CredentialStore, i64, Zeroizing<[u8; 32]>) {
        let store = isolated_cli_store();
        let mut conn = open_or_init_cli().unwrap();
        let root = Zeroizing::new(store.derive_user_partition_wrapping_key());
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None)
            .unwrap()
            .id;
        let locked = create_user(&mut conn, &root, "Locked", None, None, Some("pass"))
            .unwrap()
            .id;
        clear_user_session();
        store.store(key, value).unwrap();
        set_user_credential_for(&conn, &root, other, key, kind, value).unwrap();
        assert!(
            matches!(
                read_checked_row(&conn, &store, &root, other, key).unwrap(),
                CheckedRow::Value(_)
            ),
            "readable before the tags turn pending"
        );
        conn.execute(
            "INSERT INTO global_state(key, value, updated_at) VALUES (?1, '1', 0)",
            params![profile_tags_pending_key(locked)],
        )
        .unwrap();
        assert!(
            matches!(
                read_checked_row(&conn, &store, &root, other, key).unwrap(),
                CheckedRow::Withheld
            ),
            "withheld once they are"
        );
        (store, other, root)
    }

    /// A Jottacloud refresh over `key` of `other`, compared with `expected`
    /// ([`replace_owner_aware_if_unchanged`], `signed_in` false), and the row
    /// as stored afterwards.
    fn jotta_refresh_over(
        store: &CredentialStore,
        other: i64,
        root: &[u8; 32],
        key: &str,
        expected: Option<&str>,
    ) -> (Result<bool, String>, Option<String>) {
        let refreshed = replace_owner_aware_if_unchanged(
            store,
            Some(other),
            key,
            "jottacloud_refresh",
            expected,
            r#"{"refresh_token":"rotated","username":"jotta-user"}"#,
            &|_, _| false,
            None,
            &[],
            false,
        );
        let row = get_user_credential_for(&open_or_init_cli().unwrap(), root, other, key)
            .unwrap()
            .map(|value| value.to_string());
        (refreshed, row)
    }

    /// A refresh compared with the bytes it read while the row was readable
    /// does not write over the row once its readers withhold it: the withheld
    /// check runs in the write's own transaction for a refresh too, and the
    /// row keeps its bytes. Red when the check is a sign-in's only again
    /// (`signed_in &&` back in its guard): the row is compared raw and written.
    #[test]
    fn a_refresh_does_not_write_over_a_row_withheld_since_its_read() {
        if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
            "user_partitions::tests::a_refresh_does_not_write_over_a_row_withheld_since_its_read",
        ) {
            return;
        }
        let _guard = test_lock();
        let key = "jottacloud_refresh_w";
        let owners = r#"{"refresh_token":"owners-chain","username":"jotta-user"}"#;
        let (store, other, root) = a_row_withheld_at_write_time(key, "jottacloud_refresh", owners);
        let (over_its_read, row) = jotta_refresh_over(&store, other, &root, key, Some(owners));
        assert_eq!(over_its_read, Ok(false), "a refresh over a withheld row");
        assert_eq!(row.as_deref(), Some(owners), "the row keeps its bytes");
    }

    /// A refresh that found no row (`expected` `None`: a key it read empty,
    /// filled since) does not fill a row its readers withhold: the withheld
    /// check refuses a refresh outright, and does not read the row as absent
    /// the way a sign-in's does. Before the check ran for a refresh, the raw
    /// row already refused this write (`Some` against `None`), so what this
    /// pins is the refusal itself: red when the check reads a refresh's row as
    /// absent too (its `return Ok(false)` gone), which matches `None` and
    /// writes the rotation over the withheld row.
    #[test]
    fn a_refresh_that_found_no_row_does_not_fill_a_withheld_row() {
        if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
            "user_partitions::tests::a_refresh_that_found_no_row_does_not_fill_a_withheld_row",
        ) {
            return;
        }
        let _guard = test_lock();
        let key = "jottacloud_refresh_w";
        let owners = r#"{"refresh_token":"owners-chain","username":"jotta-user"}"#;
        let (store, other, root) = a_row_withheld_at_write_time(key, "jottacloud_refresh", owners);
        let (over_no_row, row) = jotta_refresh_over(&store, other, &root, key, None);
        assert_eq!(over_no_row, Ok(false), "a refresh that found no row");
        assert_eq!(row.as_deref(), Some(owners), "the row keeps its bytes");
    }

    /// The same for a refresh written through [`replace_user_credential_where`]
    /// (an OAuth refresh, a native Proton session's): compared with the
    /// bytes read while the row was readable, it is not written once the
    /// readers withhold the row.
    #[test]
    fn a_conditional_refresh_does_not_write_over_a_row_withheld_since_its_read() {
        if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
            "user_partitions::tests::a_conditional_refresh_does_not_write_over_a_row_withheld_since_its_read",
        ) {
            return;
        }
        let _guard = test_lock();
        let key = "oauth_dropbox_w";
        let (_store, other, root) = a_row_withheld_at_write_time(key, "oauth", "owners-token");
        let conn = open_or_init_cli().unwrap();
        let written = replace_user_credential_if_unchanged(
            &conn,
            &root,
            other,
            key,
            "oauth",
            ("owners-token", "refreshed-token"),
            false,
        );
        let row = get_user_credential_for(&conn, &root, other, key)
            .unwrap()
            .map(|value| value.to_string());
        assert_eq!(written, Ok(false), "a refresh over a withheld row");
        assert_eq!(
            row.as_deref(),
            Some("owners-token"),
            "the row keeps its bytes"
        );
    }

    /// The two refresh writers over a partition row, the OAuth refresh's
    /// ([`replace_user_credential_if_unchanged_outcome`]) and the native
    /// Proton session's ([`refresh_user_credential`]), tell a row withheld
    /// since their read from one that changed the same way: withheld, the
    /// result is kept (OAuth files it, Proton keeps it in the live session and
    /// logs it); changed, it is stale. Neither writes the withheld row.
    #[cfg(feature = "proton-native")]
    #[test]
    fn both_refresh_writers_tell_a_withheld_row_from_a_changed_one() {
        if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
            "user_partitions::tests::both_refresh_writers_tell_a_withheld_row_from_a_changed_one",
        ) {
            return;
        }
        let _guard = test_lock();
        let oauth_key = "oauth_dropbox_w";
        let (store, other, root) = a_row_withheld_at_write_time(oauth_key, "oauth", "owners-token");
        let proton_key = "proton_native_session_w";
        let session = r#"{"uid":"owners-session","access_token":"before"}"#;
        store.store(proton_key, session).unwrap();
        let conn = open_or_init_cli().unwrap();
        set_user_credential_for(
            &conn,
            &root,
            other,
            proton_key,
            "proton_native_session",
            session,
        )
        .unwrap();
        let oauth = |read: &str| {
            replace_user_credential_if_unchanged_outcome(
                &conn,
                &root,
                other,
                oauth_key,
                "oauth",
                (read, "refreshed-token"),
                false,
            )
        };
        let same_session =
            |current: &str| proton_session_uid(current).as_deref() == Some("owners-session");
        let proton = || {
            refresh_user_credential(
                &store,
                other,
                proton_key,
                "proton_native_session",
                &same_session,
                r#"{"uid":"owners-session","access_token":"after"}"#,
            )
        };
        assert_eq!(oauth("owners-token"), Ok(RefreshOutcome::Withheld), "OAuth");
        assert_eq!(proton(), Ok(RefreshOutcome::Withheld), "Proton");
        assert_eq!(
            oauth("another-token"),
            Ok(RefreshOutcome::Changed),
            "OAuth over a row that holds another token"
        );
        let row = |key: &str| {
            get_user_credential_for(&conn, &root, other, key)
                .unwrap()
                .map(|value| value.to_string())
        };
        assert_eq!(row(oauth_key).as_deref(), Some("owners-token"));
        assert_eq!(row(proton_key).as_deref(), Some(session));
    }

    /// While profile tags are pending, an unstamped row of an account other
    /// than the legacy owner reads as absent, and the stamp is checked on the
    /// ciphertext the reader decrypted: a sign-in that rewrites and stamps the
    /// row between the read and the check never lets the old bytes through.
    #[test]
    fn a_rows_stamp_is_checked_on_the_bytes_the_reader_returns() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partitions.db");
        let mut conn = open_or_init_path(path.clone()).expect("open");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None)
            .unwrap()
            .id;
        clear_user_session();
        let key = "oauth_dropbox_p";
        set_user_credential_for(&conn, &root, other, key, "oauth", "owners-token").unwrap();
        conn.execute(
            "INSERT INTO global_state(key, value, updated_at) VALUES (?1, '1', 0)",
            params![profile_tags_pending_key(owner)],
        )
        .unwrap();
        let (_, read) = fake_vault(&[(key, "owners-token")]);
        let reads = || {
            read_credential_owner_aware_inner(&conn, &root, other, key, &read)
                .unwrap()
                .map(|value| value.to_string())
        };
        assert_eq!(reads(), None, "an unstamped copy while tags are pending");

        AFTER_ROW_READ.set(Some(Box::new(move || {
            let conn = Connection::open(&path).unwrap();
            set_signed_in_credential_for(&conn, &test_root(), other, key, "oauth", "n-new")
                .unwrap();
        })));
        let raced = reads();
        assert_ne!(
            raced.as_deref(),
            Some("owners-token"),
            "the bytes read before the sign-in are not its own"
        );
        assert_eq!(reads().as_deref(), Some("n-new"), "the stamped row");
    }

    /// The row and the alignment state are read in one snapshot: an alignment
    /// that drops the unstamped copy and marks the account after the reader
    /// decrypted it does not vouch for those bytes, which stay withheld. The
    /// account's own stamped sign-in, raced the same way, stays readable.
    #[test]
    fn an_alignment_after_the_row_read_does_not_vouch_for_its_bytes() {
        let _guard = test_lock();
        for stamped in [false, true] {
            clear_user_session();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("partitions.db");
            let mut conn = open_or_init_path(path.clone()).expect("open");
            let root = test_root();
            migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
            let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
            let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None)
                .unwrap()
                .id;
            clear_user_session();
            let key = "oauth_dropbox_p";
            if stamped {
                set_signed_in_credential_for(&conn, &root, other, key, "oauth", "own-sign-in")
                    .unwrap();
            } else {
                set_user_credential_for(&conn, &root, other, key, "oauth", "owners-token").unwrap();
            }
            conn.execute(
                "INSERT INTO global_state(key, value, updated_at) VALUES (?1, '1', 0)",
                params![profile_tags_pending_key(owner)],
            )
            .unwrap();
            let (_, read) = fake_vault(&[(key, "owners-token")]);
            AFTER_ROW_READ.set(Some(Box::new(move || {
                // Another process's alignment completes: tags rebuilt, the
                // copy dropped, the account marked aligned.
                let conn = Connection::open(&path).unwrap();
                conn.execute(
                    "DELETE FROM global_state WHERE key = ?1",
                    params![profile_tags_pending_key(owner)],
                )
                .unwrap();
                if !stamped {
                    delete_user_credential_for(&conn, other, key).unwrap();
                }
                mark_private_tokens_aligned(&conn, other).unwrap();
            })));
            let raced = read_checked_row_with(&conn, &root, other, key, &read).unwrap();
            if stamped {
                assert!(
                    matches!(&raced, CheckedRow::Value(value) if value.as_str() == "own-sign-in"),
                    "the account's own sign-in"
                );
            } else {
                assert!(
                    matches!(raced, CheckedRow::Withheld),
                    "the copy's bytes were vouched for by an alignment after the read"
                );
                assert!(matches!(
                    read_checked_row_with(&conn, &root, other, key, &read).unwrap(),
                    CheckedRow::Absent
                ));
            }
        }
    }

    /// With the tags already current, a writer holding the profile list lock
    /// (a restore landing rows before its realignment) leaves an unaligned
    /// non-owner row just as unverified: the unstamped copy reads as absent,
    /// the account's own stamped sign-in stays readable.
    #[test]
    fn a_busy_profile_list_lock_withholds_an_unaligned_unstamped_row() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partitions.db");
        let mut conn = open_or_init_path(path.clone()).expect("open");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None)
            .unwrap()
            .id;
        clear_user_session();
        let copy = "oauth_dropbox_p";
        let own = "oauth_dropbox_q";
        set_user_credential_for(&conn, &root, other, copy, "oauth", "owners-token").unwrap();
        set_signed_in_credential_for(&conn, &root, other, own, "oauth", "own-sign-in").unwrap();
        assert!(!private_tokens_aligned(&conn, other).unwrap());
        let (_, read) = fake_vault(&[(copy, "owners-token"), (own, "own-sign-in")]);
        let reads = |key: &str| {
            read_credential_owner_aware_inner(&conn, &root, other, key, &read)
                .unwrap()
                .map(|value| value.to_string())
        };
        assert_eq!(reads(copy).as_deref(), Some("owners-token"));
        assert_eq!(reads(own).as_deref(), Some("own-sign-in"));

        let writer = ProfileListLock::acquire(&conn).unwrap();
        assert!(writer.is_some());
        assert_eq!(
            reads(copy),
            None,
            "an unstamped copy while a writer holds the lock"
        );
        assert_eq!(
            reads(own).as_deref(),
            Some("own-sign-in"),
            "the stamped sign-in stays readable"
        );
        drop(writer);
        assert_eq!(
            reads(copy).as_deref(),
            Some("owners-token"),
            "readable again once the lock is free"
        );
    }

    /// The alignment does not run while a profile writer holds the list lock,
    /// which also covers a caller holding it: the account stays unmarked, and
    /// the next pass sees the holder the writer added meanwhile, so the token
    /// stays in the vault for the legacy owner who now holds the profile.
    #[test]
    fn a_holder_added_during_the_alignment_keeps_its_token() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_or_init_path(dir.path().join("partitions.db")).expect("open");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None).unwrap();
        clear_user_session();
        let profile = json!({"id": "p", "name": "p", "protocol": "dropbox", "host": "",
                             "port": 0, "username": "p@example.test"});
        replace_server_profiles_for(&mut conn, &root, other.id, std::slice::from_ref(&profile))
            .unwrap();
        let (keys, read) = fake_vault(&[("oauth_dropbox_p", "owners-token")]);
        let deleted = RefCell::new(Vec::<String>::new());
        let delete = |key: &str, _: Option<&str>| {
            deleted.borrow_mut().push(key.to_string());
            Ok(true)
        };

        let writer = ProfileListLock::acquire(&conn).unwrap();
        assert_eq!(
            align_private_tokens_inner(
                &conn,
                &root,
                other.id,
                &keys,
                &|| Ok(keys.clone()),
                &read,
                &delete
            )
            .unwrap(),
            0
        );
        assert!(!private_tokens_aligned(&conn, other.id).unwrap());
        replace_server_profiles_under_lock(&mut conn, &root, owner, &[profile]).unwrap();
        drop(writer);

        assert_eq!(
            align_private_tokens_inner(
                &conn,
                &root,
                other.id,
                &keys,
                &|| Ok(keys.clone()),
                &read,
                &delete
            )
            .unwrap(),
            0
        );
        assert!(private_tokens_aligned(&conn, other.id).unwrap());
        assert!(deleted.borrow().is_empty());
        assert_eq!(
            get_user_credential_for(&conn, &root, other.id, "oauth_dropbox_p").unwrap(),
            None
        );
    }

    /// The targeted pass after a restore that cannot run now (the list lock
    /// busy, profile tags pending) is left to the account's next ordinary
    /// pass, over the same keys alone: the account's own token then reaches
    /// its row and leaves the vault, while a profile only the restore brought
    /// gets nothing.
    #[test]
    fn a_targeted_realignment_that_cannot_run_is_run_by_the_next_pass() {
        let _guard = test_lock();
        for blocked_by_tags in [false, true] {
            clear_user_session();
            let dir = tempfile::tempdir().unwrap();
            let store = throwaway_vault(dir.path());
            let root = store.derive_user_partition_wrapping_key();
            let db = dir.path().join("partitions.db");
            let mut conn = open_or_init_path(db.clone()).expect("open");
            migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
            let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
            let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None)
                .unwrap()
                .id;
            clear_user_session();
            set_active_user(&conn, other).unwrap();
            let profile = |id: &str| {
                json!({"id": id, "name": id, "protocol": "dropbox", "host": "", "port": 0,
                       "username": format!("{id}@example.test")})
            };
            replace_server_profiles_for(&mut conn, &root, other, &[profile("p"), profile("q")])
                .unwrap();
            mark_creds_migrated(&conn, other).unwrap();
            mark_private_tokens_aligned(&conn, other).unwrap();
            store.store("oauth_dropbox_p", "n-token").unwrap();
            store.store("oauth_dropbox_q", "backup-q").unwrap();
            let row = |key: &str| {
                get_user_credential_for(&conn, &root, other, key)
                    .unwrap()
                    .map(|value| value.to_string())
            };

            let writer = (!blocked_by_tags).then(|| ProfileListLock::acquire(&conn).unwrap());
            if blocked_by_tags {
                conn.execute(
                    "INSERT INTO global_state(key, value, updated_at) VALUES (?1, '1', 0)",
                    params![profile_tags_pending_key(owner)],
                )
                .unwrap();
            }
            let (ids, restored) = (["p".to_string()], ["oauth_dropbox_p".to_string()]);
            let realigned = realign_private_tokens_after_restore(
                &db,
                Some(ActiveBeforeRestore {
                    store: &store,
                    user_id: other,
                    profile_ids: &ids,
                    restored_keys: &restored,
                }),
            )
            .unwrap();
            assert_eq!(realigned, 0, "tags pending: {blocked_by_tags}");
            assert_eq!(row("oauth_dropbox_p"), None);
            assert!(deferred_realign_tags(&conn, other).unwrap().is_some());
            drop(writer);
            conn.execute(
                "DELETE FROM global_state WHERE key = ?1",
                params![profile_tags_pending_key(owner)],
            )
            .unwrap();

            // Another restore defers a key of its own while the pass runs; it
            // wrote that key into the vault before.
            store.store("oauth_dropbox_r", "backup-r").unwrap();
            let (later_db, later_root) = (db.clone(), root);
            BEFORE_REALIGN_CLEAR.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    let later = open_or_init_path(later_db).expect("second connection");
                    defer_realign(&later, &later_root, other, &["oauth_dropbox_r".to_string()])
                        .expect("the later deferral");
                }))
            });
            migrate_credentials_eager_all(&conn, &store, &root).unwrap();
            assert_eq!(
                row("oauth_dropbox_p").as_deref(),
                Some("n-token"),
                "tags pending: {blocked_by_tags}"
            );
            assert!(store.get("oauth_dropbox_p").is_err(), "the vault let go");
            assert!(
                deferred_realign_tags(&conn, other).unwrap().is_some(),
                "the later deferral waits for the next pass"
            );
            migrate_credentials_for_user(&conn, &store, &root, other).unwrap();
            assert!(deferred_realign_tags(&conn, other).unwrap().is_none());
            assert_eq!(
                row("oauth_dropbox_q"),
                None,
                "a profile the restore brought"
            );
            assert_eq!(store.get("oauth_dropbox_q").unwrap(), "backup-q");
        }
    }

    /// A deferred pass clears only the tags of the keys it ran: a tag a
    /// restore deferred for a key the pass's vault list did not hold yet stays
    /// for the next pass, which moves that key, and a tag whose key the vault
    /// no longer lists goes.
    #[test]
    fn a_deferred_pass_keeps_the_tags_it_did_not_run() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_or_init_path(dir.path().join("partitions.db")).expect("open");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None)
            .unwrap()
            .id;
        clear_user_session();
        let profile = |id: &str| {
            json!({"id": id, "name": id, "protocol": "dropbox", "host": "", "port": 0,
                   "username": format!("{id}@example.test")})
        };
        replace_server_profiles_for(&mut conn, &root, other, &[profile("p"), profile("q")])
            .unwrap();
        mark_creds_migrated(&conn, other).unwrap();
        mark_private_tokens_aligned(&conn, other).unwrap();
        let (first, later, gone) = ("oauth_dropbox_p", "oauth_dropbox_q", "oauth_dropbox_gone");
        defer_realign(
            &conn,
            &root,
            other,
            &[first, later, gone].map(str::to_string),
        )
        .unwrap();
        let (all, read) = fake_vault(&[(first, "p-token"), (later, "q-token")]);
        let deleted = RefCell::new(Vec::<String>::new());
        let delete = |key: &str, _: Option<&str>| {
            deleted.borrow_mut().push(key.to_string());
            Ok(true)
        };
        let row = |key: &str| {
            get_user_credential_for(&conn, &root, other, key)
                .unwrap()
                .map(|value| value.to_string())
        };
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        let tag = |key: &str| user_crypto::metadata_tag(&root_secret, b"realign-key", key).unwrap();

        // The pass's list was taken before the later key was restored.
        let listed = vec![first.to_string()];
        align_private_tokens_inner(
            &conn,
            &root,
            other,
            &listed,
            &|| Ok(all.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(row(first).as_deref(), Some("p-token"));
        assert_eq!(row(later), None);
        assert_eq!(*deleted.borrow(), [first.to_string()]);
        assert_eq!(
            deferred_realign_tags(&conn, other).unwrap(),
            Some(vec![tag(later)]),
            "only the tag the pass did not run stays"
        );

        align_private_tokens_inner(
            &conn,
            &root,
            other,
            &all,
            &|| Ok(all.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(row(later).as_deref(), Some("q-token"));
        assert!(deleted.borrow().contains(&later.to_string()));
        assert_eq!(deferred_realign_tags(&conn, other).unwrap(), None);
    }

    /// A restore that defers a key again while a deferred pass runs, after
    /// the pass ran that key and let the list lock go, keeps that key's tag:
    /// the pass cannot tell the new deferral from the one it ran, so the tag
    /// stays for the next pass, with the tag of the restore's other key.
    #[test]
    fn a_key_deferred_again_during_a_deferred_pass_keeps_its_tag() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partitions.db");
        let mut conn = open_or_init_path(path.clone()).expect("open");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None)
            .unwrap()
            .id;
        clear_user_session();
        let profile = |id: &str| {
            json!({"id": id, "name": id, "protocol": "dropbox", "host": "", "port": 0,
                   "username": format!("{id}@example.test")})
        };
        replace_server_profiles_for(&mut conn, &root, other, &[profile("p"), profile("q")])
            .unwrap();
        mark_creds_migrated(&conn, other).unwrap();
        mark_private_tokens_aligned(&conn, other).unwrap();
        let (again, new) = ("oauth_dropbox_p", "oauth_dropbox_q");
        defer_realign(&conn, &root, other, &[again.to_string()]).unwrap();
        let (all, read) = fake_vault(&[(again, "p-token"), (new, "q-token")]);
        let delete = |_: &str, _: Option<&str>| Ok(true);
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        let tag = |key: &str| user_crypto::metadata_tag(&root_secret, b"realign-key", key).unwrap();
        // The restore puts both keys back in the vault and defers them, once
        // the pass ran the first one.
        BEFORE_REALIGN_CLEAR.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                let later = open_or_init_path(path).expect("second connection");
                defer_realign(&later, &root, other, &[again, new].map(str::to_string))
                    .expect("the later deferral");
            }))
        });

        align_private_tokens_inner(
            &conn,
            &root,
            other,
            &[again.to_string()],
            &|| Ok(all.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(
            get_user_credential_for(&conn, &root, other, again)
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("p-token"),
            "the pass ran the first deferral"
        );
        let mut expected = vec![tag(again), tag(new)];
        expected.sort();
        assert_eq!(
            deferred_realign_tags(&conn, other).unwrap(),
            Some(expected),
            "the key deferred again keeps its tag"
        );
    }

    /// On a filesystem without file locks a deferred pass copies the key but
    /// the vault keeps it, so the pass is not done: its marker stays, and the
    /// next pass that can exclude the profile writers removes the vault copy
    /// and the marker, while the row stays.
    #[test]
    fn without_file_locks_a_deferred_pass_keeps_its_marker() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_or_init_path(dir.path().join("partitions.db")).expect("open");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None)
            .unwrap()
            .id;
        clear_user_session();
        let profile = json!({"id": "p", "name": "p", "protocol": "dropbox", "host": "",
                             "port": 0, "username": "p@example.test"});
        replace_server_profiles_for(&mut conn, &root, other, &[profile]).unwrap();
        mark_creds_migrated(&conn, other).unwrap();
        mark_private_tokens_aligned(&conn, other).unwrap();
        let key = "oauth_dropbox_p";
        defer_realign(&conn, &root, other, &[key.to_string()]).unwrap();
        let (keys, read) = fake_vault(&[(key, "mine-token")]);
        let deleted = RefCell::new(Vec::<String>::new());
        let delete = |key: &str, _: Option<&str>| {
            deleted.borrow_mut().push(key.to_string());
            Ok(true)
        };
        let row = || {
            get_user_credential_for(&conn, &root, other, key)
                .unwrap()
                .map(|value| value.to_string())
        };

        NO_FILE_LOCKS_IN_TEST.set(true);
        let first = align_private_tokens_inner(
            &conn,
            &root,
            other,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        );
        NO_FILE_LOCKS_IN_TEST.set(false);
        first.unwrap();
        assert_eq!(row().as_deref(), Some("mine-token"));
        assert!(deleted.borrow().is_empty(), "the vault kept its key");
        assert!(
            deferred_realign_tags(&conn, other).unwrap().is_some(),
            "the marker stays while the vault holds the copy"
        );

        align_private_tokens_inner(
            &conn,
            &root,
            other,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(row().as_deref(), Some("mine-token"));
        assert_eq!(*deleted.borrow(), [key.to_string()]);
        assert!(deferred_realign_tags(&conn, other).unwrap().is_none());
    }

    /// An install migrated before the tokens were kept to their account: on a
    /// profile id the legacy owner also holds, the other account's row holding
    /// the vault's very value is the copy that migration made of the owner's
    /// token, and is dropped; a row of its own sign-in (here a chain of another
    /// Jottacloud user, a session of another UID) stays. The vault keeps the
    /// owner's.
    #[test]
    fn a_copy_an_earlier_migration_made_of_a_shared_token_is_dropped() {
        let _guard = test_lock();
        let mut conn = migrated_conn(2);
        let root = test_root();
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None).unwrap();
        clear_user_session();
        let profile = |id: &str| {
            json!({"id": id, "name": id, "protocol": "dropbox", "host": "", "port": 0,
                   "username": format!("{id}@example.test")})
        };
        replace_server_profiles_for(
            &mut conn,
            &root,
            other.id,
            &[profile("profile-0"), profile("profile-1")],
        )
        .unwrap();
        mark_creds_migrated(&conn, other.id).unwrap();
        let session = |uid: &str, pair: &str| {
            json!({"uid": uid, "access_token": pair, "refresh_token": pair, "key_password": "k"})
                .to_string()
        };
        let (rotated, copied) = (session("owners-uid", "r2"), session("owners-uid", "r1"));
        let (owners_other, own) = (session("owners-other-uid", "r1"), session("own-uid", "r1"));
        let chain = |refresh: &str, user: &str| {
            json!({"refresh_token": refresh, "token_endpoint": "https://example.test/token",
                   "username": user})
            .to_string()
        };
        let (others_chain, owners_chain) = (chain("r-other", "other"), chain("r-owner", "owner"));
        for (key, value) in [
            ("oauth_dropbox_profile-0", "owners-token"),
            ("jottacloud_refresh_profile-0", others_chain.as_str()),
            ("proton_native_session_profile-0", copied.as_str()),
            ("proton_native_session_profile-1", own.as_str()),
        ] {
            let kind = private_token_type(key).unwrap();
            set_user_credential_for(&conn, &root, other.id, key, kind, value).unwrap();
        }
        let (keys, read) = fake_vault(&[
            ("oauth_dropbox_profile-0", "owners-token"),
            ("jottacloud_refresh_profile-0", &owners_chain),
            ("proton_native_session_profile-0", &rotated),
            ("proton_native_session_profile-1", &owners_other),
        ]);
        let deleted = RefCell::new(Vec::<String>::new());
        let delete = |key: &str, _: Option<&str>| {
            deleted.borrow_mut().push(key.to_string());
            Ok(true)
        };
        let row = |key: &str| {
            get_user_credential_for(&conn, &root, other.id, key)
                .unwrap()
                .map(|value| value.to_string())
        };
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(row("oauth_dropbox_profile-0"), None);
        // An unstamped Jottacloud row is a copy like any other: a changed
        // username or refresh token proves nothing, since a copied chain
        // drifts in both once it is refreshed. Only the sign-in stamp does.
        assert_eq!(row("jottacloud_refresh_profile-0"), None);
        // The owner's session, rotated since the copy, is the same session.
        assert_eq!(row("proton_native_session_profile-0"), None);
        assert_eq!(
            row("proton_native_session_profile-1").as_deref(),
            Some(own.as_str())
        );
        assert!(deleted.borrow().is_empty());
        assert!(private_tokens_aligned(&conn, other.id).unwrap());
    }

    /// A partition restored from another machine: its profile tags are keyed
    /// by that machine's root. The accounts this machine opens get their lists
    /// tagged again; a passphrase account is pending until it is unlocked
    /// here, and until then another account's alignment waits, since the
    /// holders it reads would miss the pending account. Once that account is
    /// tagged, the profile id both hold keeps its vault copy.
    #[test]
    fn a_restored_partitions_tags_are_rebuilt_before_the_alignment_reads_them() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let (source, here) = (test_root(), [9u8; 32]);
        let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
        let profile = json!({"id": "p", "name": "p", "protocol": "dropbox", "host": "",
                             "port": 0, "username": "p@example.test"});
        let locked = create_user(&mut conn, &source, "Locked", None, None, Some("pass")).unwrap();
        unlock_user(&conn, &source, locked.id, Some("pass")).unwrap();
        replace_server_profiles_for(
            &mut conn,
            &source,
            locked.id,
            std::slice::from_ref(&profile),
        )
        .unwrap();
        let other = create_passphrase_less_user(&mut conn, &source, "Other", None, None).unwrap();
        replace_server_profiles_for(&mut conn, &source, other.id, std::slice::from_ref(&profile))
            .unwrap();
        clear_user_session();
        let transport = export_transport_deks(&conn, &source, "backup password")
            .unwrap()
            .unwrap();
        let rekey = rekey_transport_deks(
            &conn,
            &here,
            "backup password",
            &transport.salt,
            &transport.kdf_params,
            &transport.wrapped_deks,
        )
        .unwrap();
        assert_eq!(rekey.rekeyed, 2);
        assert_eq!(retag_restored_profiles(&conn, &here, &rekey).unwrap(), 1);
        mark_creds_migrated(&conn, owner).unwrap();
        mark_private_tokens_aligned(&conn, owner).unwrap();
        let (keys, read) = fake_vault(&[("oauth_dropbox_p", "token")]);
        let deleted = RefCell::new(Vec::<String>::new());
        let delete = |key: &str, _: Option<&str>| {
            deleted.borrow_mut().push(key.to_string());
            Ok(true)
        };

        assert_eq!(
            align_private_tokens_inner(
                &conn,
                &here,
                other.id,
                &keys,
                &|| Ok(keys.clone()),
                &read,
                &delete
            )
            .unwrap(),
            0
        );
        assert!(!private_tokens_aligned(&conn, other.id).unwrap());
        assert!(!has_eager_pending_users(&conn).unwrap());

        unlock_user(&conn, &here, locked.id, Some("pass")).unwrap();
        retag_own_profiles_if_pending(&conn, &here, locked.id).unwrap();
        clear_user_session();
        assert!(has_eager_pending_users(&conn).unwrap());
        align_private_tokens_inner(
            &conn,
            &here,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert!(private_tokens_aligned(&conn, other.id).unwrap());
        assert!(deleted.borrow().is_empty(), "{:?}", deleted.borrow());
        assert_eq!(
            get_user_credential_for(&conn, &here, other.id, "oauth_dropbox_p").unwrap(),
            None
        );
    }

    /// The SQL that selects native Proton session rows takes the three key
    /// shapes the Rust filter accepts and nothing else: no other secret's
    /// ciphertext is loaded to be filtered out, an `_` is not a wildcard,
    /// and a prefix with no tail is no session.
    #[cfg(feature = "proton-native")]
    #[test]
    fn the_session_row_query_selects_session_keys_alone() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE k(credential_id TEXT)")
            .unwrap();
        let keys = [
            "proton_native_session",
            "proton_native_session_p1",
            "proton_native_unbound_user_3",
            "proton_native_session_",
            "proton_native_unbound_user_",
            "protonXnativeXsessionXp1",
            "proton_native_sessionXp1",
            "proton_native_unbound_userX3",
            "server_proton_native_session_p1",
            "server_p1",
        ];
        for key in keys {
            conn.execute("INSERT INTO k VALUES (?1)", params![key])
                .unwrap();
        }
        let selected: Vec<String> = conn
            .prepare(&format!(
                "SELECT credential_id FROM k WHERE {} ORDER BY rowid",
                proton_session_key_sql("credential_id")
            ))
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let expected: Vec<String> = keys
            .iter()
            .filter(|key| is_proton_session_key(key))
            .map(|key| key.to_string())
            .collect();
        assert_eq!(expected.len(), 3);
        assert_eq!(selected, expected);
    }

    /// A partition restored from another machine with a passphrase account
    /// still locked: its tags are pending, so the alignment cannot yet drop
    /// the row an earlier migration copied from the legacy owner's token into
    /// another account. Until it can, every reader of that account takes the
    /// row for absent, so it never signs in to the owner's cloud; the owner
    /// reads its token, a row the account's own sign-in stamped reads as
    /// before, and so does every row once the account is aligned.
    #[test]
    fn an_unverified_copy_reads_as_absent_while_profile_tags_wait() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let (source, here) = (test_root(), [9u8; 32]);
        let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
        let profile = |id: &str| {
            json!({"id": id, "name": id, "protocol": "dropbox", "host": "", "port": 0,
                   "username": format!("{id}@example.test")})
        };
        let locked = create_user(&mut conn, &source, "Locked", None, None, Some("pass")).unwrap();
        unlock_user(&conn, &source, locked.id, Some("pass")).unwrap();
        replace_server_profiles_for(&mut conn, &source, locked.id, &[profile("elsewhere")])
            .unwrap();
        let other = create_passphrase_less_user(&mut conn, &source, "Other", None, None).unwrap();
        replace_server_profiles_for(&mut conn, &source, other.id, &[profile("p"), profile("q")])
            .unwrap();
        clear_user_session();
        let transport = export_transport_deks(&conn, &source, "backup password")
            .unwrap()
            .unwrap();
        let rekey = rekey_transport_deks(
            &conn,
            &here,
            "backup password",
            &transport.salt,
            &transport.kdf_params,
            &transport.wrapped_deks,
        )
        .unwrap();
        assert_eq!(retag_restored_profiles(&conn, &here, &rekey).unwrap(), 1);
        set_user_credential_for(
            &conn,
            &here,
            other.id,
            "oauth_dropbox_p",
            "oauth",
            "owners-p",
        )
        .unwrap();
        set_signed_in_credential_for(&conn, &here, other.id, "oauth_dropbox_q", "oauth", "own-q")
            .unwrap();
        let (_, read) = fake_vault(&[
            ("oauth_dropbox_p", "owners-p"),
            ("oauth_dropbox_q", "owners-q"),
        ]);
        let owner_aware = |user: i64, key: &str| {
            read_credential_owner_aware_inner(&conn, &here, user, key, &read)
                .unwrap()
                .map(|value| value.to_string())
        };
        let with_fallback = |user: i64, key: &str| {
            read_credential_with_fallback_inner(&conn, &here, user, key, &read)
                .unwrap()
                .map(|value| value.to_string())
        };
        assert_eq!(owner_aware(other.id, "oauth_dropbox_p"), None);
        assert_eq!(with_fallback(other.id, "oauth_dropbox_p"), None);
        assert_eq!(
            resolve_relocate_secret_with(&conn, &here, other.id, None, "dropbox", "p", &read)
                .unwrap(),
            None,
            "a relocation carried the unverified row"
        );
        assert_eq!(
            owner_aware(other.id, "oauth_dropbox_q").as_deref(),
            Some("own-q")
        );
        assert_eq!(
            owner_aware(owner, "oauth_dropbox_p").as_deref(),
            Some("owners-p")
        );
        // The owner signs out while the tags still wait: its vault key is
        // gone, and the copy it leaves in the other account still reads as
        // absent; the row the account's own sign-in stamped reads as before.
        let (_, signed_out) = fake_vault(&[("oauth_dropbox_q", "owners-q")]);
        for user_key in ["oauth_dropbox_p", "oauth_dropbox_q"] {
            let read = |key: &str| {
                read_credential_owner_aware_inner(&conn, &here, other.id, key, &signed_out)
                    .unwrap()
                    .map(|value| value.to_string())
            };
            let expected = (user_key == "oauth_dropbox_q").then(|| "own-q".to_string());
            assert_eq!(
                read(user_key),
                expected,
                "{user_key} after the owner's sign-out"
            );
        }
        assert_eq!(
            read_credential_with_fallback_inner(
                &conn,
                &here,
                other.id,
                "oauth_dropbox_p",
                &signed_out
            )
            .unwrap(),
            None
        );

        mark_private_tokens_aligned(&conn, other.id).unwrap();
        assert_eq!(
            owner_aware(other.id, "oauth_dropbox_p").as_deref(),
            Some("owners-p")
        );
    }

    /// On a profile id the legacy owner also holds, another account's row stays
    /// only when it shows a sign-in of its own. A copy of the owner's record
    /// that the owner's refreshes have rewritten since holds other bytes but
    /// the same sign-in family and refresh token, and is dropped; so is a
    /// record with no family, which says nothing of where it came from, and a
    /// Jottacloud chain of the vault's Jottacloud user. A record its own
    /// sign-in stamped, and a chain of another Jottacloud user, stay.
    #[test]
    fn a_shared_profiles_row_stays_only_for_a_sign_in_of_its_own() {
        let _guard = test_lock();
        let mut conn = migrated_conn(3);
        let root = test_root();
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None).unwrap();
        clear_user_session();
        let profile = |id: &str| {
            json!({"id": id, "name": id, "protocol": "dropbox", "host": "", "port": 0,
                   "username": format!("{id}@example.test")})
        };
        replace_server_profiles_for(
            &mut conn,
            &root,
            other.id,
            &[
                profile("profile-0"),
                profile("profile-1"),
                profile("profile-2"),
            ],
        )
        .unwrap();
        mark_creds_migrated(&conn, other.id).unwrap();
        let record = |access: &str, refresh: &str, family: Option<&str>| {
            let mut record = json!({"access_token": access, "refresh_token": refresh,
                                    "expires_at": 1, "token_type": "Bearer", "scopes": []});
            if let Some(family) = family {
                record["owner_family"] = json!(family);
            }
            record.to_string()
        };
        let chain = |refresh: &str, user: &str| {
            json!({"refresh_token": refresh, "token_endpoint": "https://example.test/token",
                   "username": user})
            .to_string()
        };
        let owners = Some("v1:owners");
        let rows = [
            (
                "oauth_dropbox_profile-0",
                record("copied-access", "owners-refresh", owners),
                record("refreshed-access", "owners-refresh", owners),
            ),
            (
                "oauth_dropbox_profile-1",
                record("own-access", "own-refresh", Some("v1:own")),
                record("owners-access", "owners-refresh", owners),
            ),
            (
                "oauth_dropbox_profile-2",
                record("old-access", "old-refresh", None),
                record("owners-access", "owners-refresh", owners),
            ),
            (
                "jottacloud_refresh_profile-0",
                chain("consumed", "owner"),
                chain("rotated", "owner"),
            ),
            (
                "jottacloud_refresh_profile-1",
                chain("own", "other"),
                chain("owners", "owner"),
            ),
        ];
        for (key, row, _) in &rows {
            let kind = private_token_type(key).unwrap();
            set_user_credential_for(&conn, &root, other.id, key, kind, row).unwrap();
        }
        // The account's own sign-ins, stamped as its sign-in writer stamps
        // them: only the stamp proves a shared row is this account's own.
        set_signed_in_credential_for(&conn, &root, other.id, rows[1].0, "oauth", &rows[1].1)
            .unwrap();
        set_signed_in_credential_for(
            &conn,
            &root,
            other.id,
            rows[4].0,
            "jottacloud_refresh",
            &rows[4].1,
        )
        .unwrap();
        let held: Vec<(&str, &str)> = rows.iter().map(|(k, _, v)| (*k, v.as_str())).collect();
        let (keys, read) = fake_vault(&held);
        let delete = |_: &str, _: Option<&str>| Ok(true);
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        let row = |key: &str| {
            get_user_credential_for(&conn, &root, other.id, key)
                .unwrap()
                .map(|value| value.to_string())
        };
        assert_eq!(
            row("oauth_dropbox_profile-0"),
            None,
            "a copy of the owner's sign-in, refreshed since, was kept"
        );
        assert_eq!(row("oauth_dropbox_profile-1"), Some(rows[1].1.clone()));
        assert_eq!(row("oauth_dropbox_profile-2"), None);
        assert_eq!(row("jottacloud_refresh_profile-0"), None);
        assert_eq!(row("jottacloud_refresh_profile-1"), Some(rows[4].1.clone()));
    }

    /// The alignment marker of `user` reset, as a restore that wrote private
    /// token keys resets it ([`realign_private_tokens_after_restore`]).
    fn reset_alignment(conn: &Connection, user: i64) {
        conn.execute(
            "DELETE FROM global_state WHERE key = ?1",
            params![format!("{PRIVATE_TOKENS_ALIGNED_PREFIX}{user}")],
        )
        .unwrap();
    }

    /// An account other than the legacy owner that holds `profile-0`, a
    /// profile id the owner of `conn` (`migrated_conn(1)`) also holds.
    fn shares_the_owners_profile(conn: &mut Connection, root: &[u8; 32]) -> UserMetadata {
        let other = create_passphrase_less_user(conn, root, "Other", None, None).unwrap();
        clear_user_session();
        let profile = json!({"id": "profile-0", "name": "p", "protocol": "dropbox",
                             "host": "", "port": 0, "username": "p@example.test"});
        replace_server_profiles_for(conn, root, other.id, &[profile]).unwrap();
        mark_creds_migrated(conn, other.id).unwrap();
        other
    }

    fn oauth_record(access: &str, refresh: &str, family: &str) -> String {
        json!({"access_token": access, "refresh_token": refresh, "expires_at": 1,
               "token_type": "Bearer", "scopes": [], "owner_family": family})
        .to_string()
    }

    fn row_of(conn: &Connection, root: &[u8; 32], user: i64, key: &str) -> Option<String> {
        get_user_credential_for(conn, root, user, key)
            .unwrap()
            .map(|value| value.to_string())
    }

    /// A non-owner signs in to GitHub again on a profile id the owner also
    /// holds, with a token other than the owner's: its row is its own sign-in,
    /// kept by the alignment, and kept again when a restore resets the marker.
    #[test]
    fn a_non_owners_own_github_sign_in_survives_the_alignment_and_a_restore() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let other = shares_the_owners_profile(&mut conn, &root);
        let key = "github_oauth_profile-0";
        set_signed_in_credential_for(&conn, &root, other.id, key, "oauth", "gho_other").unwrap();
        let (keys, read) = fake_vault(&[(key, "gho_owner")]);
        let delete = |_: &str, _: Option<&str>| Ok(true);
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(
            row_of(&conn, &root, other.id, key).as_deref(),
            Some("gho_other"),
            "the non-owner's own GitHub sign-in was dropped"
        );
        reset_alignment(&conn, other.id);
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(
            row_of(&conn, &root, other.id, key).as_deref(),
            Some("gho_other"),
            "the non-owner's own GitHub sign-in was dropped after a restore"
        );
    }

    /// A row written before the stamp, on a profile id the owner also holds,
    /// that differs from the vault (an OAuth record with a `v1:` family of
    /// its own, a GitHub token) proves no sign-in: it is dropped once, the
    /// owner's row and vault copy are left as they are, and a sign-in of the
    /// account's own afterwards stays.
    #[test]
    fn an_unstamped_shared_row_is_dropped_once_and_the_owners_is_untouched() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
        let other = shares_the_owners_profile(&mut conn, &root);
        let (oauth, github) = ("oauth_dropbox_profile-0", "github_oauth_profile-0");
        let owners = oauth_record("owners-access", "owners-refresh", "v1:owners");
        let others = oauth_record("other-access", "other-refresh", "v1:other");
        set_user_credential_for(&conn, &root, other.id, oauth, "oauth", &others).unwrap();
        set_user_credential_for(&conn, &root, other.id, github, "oauth", "gho_other").unwrap();
        set_user_credential_for(&conn, &root, owner, oauth, "oauth", &owners).unwrap();
        let (keys, read) = fake_vault(&[(oauth, owners.as_str()), (github, "gho_owner")]);
        let deleted = RefCell::new(Vec::<String>::new());
        let delete = |key: &str, _: Option<&str>| {
            deleted.borrow_mut().push(key.to_string());
            Ok(true)
        };
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(
            row_of(&conn, &root, other.id, oauth),
            None,
            "an unstamped record with a family of its own was kept"
        );
        assert_eq!(row_of(&conn, &root, other.id, github), None);
        assert_eq!(row_of(&conn, &root, owner, oauth), Some(owners.clone()));
        assert!(deleted.borrow().is_empty(), "{:?}", deleted.borrow());

        set_signed_in_credential_for(&conn, &root, other.id, oauth, "oauth", &others).unwrap();
        reset_alignment(&conn, other.id);
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(row_of(&conn, &root, other.id, oauth), Some(others));
        assert_eq!(row_of(&conn, &root, owner, oauth), Some(owners));
        assert!(deleted.borrow().is_empty(), "{:?}", deleted.borrow());
    }

    /// An earlier migration copied the owner's OAuth record, family and all,
    /// into another account's row; the owner then signed in again, which
    /// minted a new family, while the copied token may still be valid. The
    /// copy differs from the vault in family and tokens alike and is still
    /// dropped; the account's own sign-in, stamped, stays.
    #[test]
    fn a_copied_v1_record_is_dropped_after_the_owner_signs_in_again() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let other = shares_the_owners_profile(&mut conn, &root);
        let key = "oauth_dropbox_profile-0";
        let copied = oauth_record("old-access", "old-refresh", "v1:owners-first");
        set_user_credential_for(&conn, &root, other.id, key, "oauth", &copied).unwrap();
        let signed_in_again = oauth_record("new-access", "new-refresh", "v1:owners-second");
        let (keys, read) = fake_vault(&[(key, signed_in_again.as_str())]);
        let delete = |_: &str, _: Option<&str>| Ok(true);
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(
            row_of(&conn, &root, other.id, key),
            None,
            "a copy of the owner's earlier sign-in was kept"
        );

        let own = oauth_record("own-access", "own-refresh", "v1:own");
        set_signed_in_credential_for(&conn, &root, other.id, key, "oauth", &own).unwrap();
        reset_alignment(&conn, other.id);
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(row_of(&conn, &root, other.id, key), Some(own));
    }

    /// An earlier migration copied the owner's token into another account's
    /// row on a profile id both hold, and the owner signed out before that
    /// account's first alignment, which took the vault key away: the
    /// unstamped copy is dropped all the same and the account is marked
    /// aligned. A row its own sign-in stamped stays, and so does a native
    /// Proton session, which the owner's sign-out ended.
    #[test]
    fn a_shared_copy_whose_vault_key_is_gone_is_dropped() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let other = shares_the_owners_profile(&mut conn, &root);
        let (oauth, proton) = ("oauth_dropbox_profile-0", "proton_native_session_profile-0");
        let copied = oauth_record("owners-access", "owners-refresh", "v1:owners");
        set_user_credential_for(&conn, &root, other.id, oauth, "oauth", &copied).unwrap();
        set_user_credential_for(
            &conn,
            &root,
            other.id,
            proton,
            "proton_native_session",
            "{}",
        )
        .unwrap();
        let (keys, read) = fake_vault(&[]);
        let delete = |_: &str, _: Option<&str>| Ok(true);
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(
            row_of(&conn, &root, other.id, oauth),
            None,
            "a copy of the owner's token outlived the owner's sign-out"
        );
        assert_eq!(
            row_of(&conn, &root, other.id, proton).as_deref(),
            Some("{}")
        );
        assert!(private_tokens_aligned(&conn, other.id).unwrap());

        let own = oauth_record("own-access", "own-refresh", "v1:own");
        set_signed_in_credential_for(&conn, &root, other.id, oauth, "oauth", &own).unwrap();
        reset_alignment(&conn, other.id);
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(row_of(&conn, &root, other.id, oauth), Some(own));
    }

    /// A stamp is never carried: a row a restore (or any writer other than
    /// the account's sign-in) writes over a stamped one is unstamped, and a
    /// restored partition database comes back with no stamp at all. Either
    /// row is dropped on a profile id the owner also holds.
    #[test]
    fn a_restored_row_carries_no_sign_in_stamp() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partitions.db");
        let mut conn = open_or_init_path(path.clone()).expect("open");
        let root = test_root();
        let owners_list = json!([{"id": "profile-0", "name": "p", "protocol": "dropbox",
                                  "host": "", "port": 0, "username": "p@example.test"}]);
        migrate_legacy_payloads(&mut conn, Some(&owners_list.to_string()), None, &root)
            .expect("seed");
        let other = shares_the_owners_profile(&mut conn, &root);
        let (oauth, github) = ("oauth_dropbox_profile-0", "github_oauth_profile-0");
        let own = oauth_record("own-access", "own-refresh", "v1:own");
        set_signed_in_credential_for(&conn, &root, other.id, oauth, "oauth", &own).unwrap();
        set_signed_in_credential_for(&conn, &root, other.id, github, "oauth", "gho_other").unwrap();
        // The backup's copy of the row, written back by a restore.
        set_user_credential_for(&conn, &root, other.id, oauth, "oauth", &own).unwrap();
        // A restored partition database.
        forget_sign_in_stamps_in_db(&path).unwrap();
        let owners = oauth_record("owners-access", "owners-refresh", "v1:owners");
        let (keys, read) = fake_vault(&[(oauth, owners.as_str()), (github, "gho_owner")]);
        let delete = |_: &str, _: Option<&str>| Ok(true);
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(
            row_of(&conn, &root, other.id, oauth),
            None,
            "a row a restore wrote kept its sign-in stamp"
        );
        assert_eq!(
            row_of(&conn, &root, other.id, github),
            None,
            "a restored database kept its sign-in stamps"
        );
    }

    /// The owner's own rows on a profile id another account also holds are
    /// never touched: not by the owner's alignment, nor by the other's.
    #[test]
    fn the_alignment_never_touches_the_owners_rows() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
        let other = shares_the_owners_profile(&mut conn, &root);
        let (oauth, github) = ("oauth_dropbox_profile-0", "github_oauth_profile-0");
        let owners = oauth_record("owners-access", "owners-refresh", "v1:owners");
        set_user_credential_for(&conn, &root, owner, oauth, "oauth", &owners).unwrap();
        set_user_credential_for(&conn, &root, owner, github, "oauth", "gho_owner_row").unwrap();
        let vault = oauth_record("vault-access", "vault-refresh", "v1:vault");
        let (keys, read) = fake_vault(&[(oauth, vault.as_str()), (github, "gho_vault")]);
        let delete = |_: &str, _: Option<&str>| Ok(true);
        for user in [owner, other.id, owner] {
            reset_alignment(&conn, user);
            align_private_tokens_inner(
                &conn,
                &root,
                user,
                &keys,
                &|| Ok(keys.clone()),
                &read,
                &delete,
            )
            .unwrap();
        }
        assert_eq!(row_of(&conn, &root, owner, oauth), Some(owners));
        assert_eq!(
            row_of(&conn, &root, owner, github).as_deref(),
            Some("gho_owner_row")
        );
    }

    /// A sign-in stamped by its writer and refreshed since (a record with no
    /// family, as the headless OAuth finish writes it) is still the account's
    /// own sign-in, and stays on a profile id the owner also holds.
    #[test]
    fn a_refreshed_sign_in_keeps_its_stamp() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let other = shares_the_owners_profile(&mut conn, &root);
        let key = "oauth_dropbox_profile-0";
        let record = |access: &str| {
            json!({"access_token": access, "refresh_token": "own-refresh", "expires_at": 1,
                   "token_type": "Bearer", "scopes": []})
            .to_string()
        };
        set_signed_in_credential_for(&conn, &root, other.id, key, "oauth", &record("first"))
            .unwrap();
        assert!(replace_user_credential_if_unchanged(
            &conn,
            &root,
            other.id,
            key,
            "oauth",
            (&record("first"), &record("refreshed")),
            false,
        )
        .unwrap());
        let owners = oauth_record("owners-access", "owners-refresh", "v1:owners");
        let (keys, read) = fake_vault(&[(key, owners.as_str())]);
        let delete = |_: &str, _: Option<&str>| Ok(true);
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(
            row_of(&conn, &root, other.id, key),
            Some(record("refreshed")),
            "a refresh of the account's own sign-in lost its stamp"
        );
    }

    /// The migration of an account other than the legacy owner takes the
    /// `server_<id>` of its own profiles, and not the one of a profile id the
    /// legacy owner also holds, whose vault entry may be the owner's. That
    /// account still reads the vault entry through the fallback, as before.
    #[test]
    fn a_shared_profile_ids_password_is_not_copied_to_another_account() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None).unwrap();
        clear_user_session();
        let profile = |id: &str| {
            json!({"id": id, "name": id, "protocol": "sftp", "host": "example.com",
                   "port": 22, "username": "u"})
        };
        replace_server_profiles_for(
            &mut conn,
            &root,
            other.id,
            &[profile("profile-0"), profile("own")],
        )
        .unwrap();
        let (keys, read) = fake_vault(&[
            ("server_profile-0", "owners-password"),
            ("server_own", "own-password"),
        ]);
        migrate_user_credentials_inner(&conn, &root, other.id, &keys, &read).unwrap();
        let row = |key: &str| {
            get_user_credential_for(&conn, &root, other.id, key)
                .unwrap()
                .map(|value| value.to_string())
        };
        assert_eq!(row("server_own").as_deref(), Some("own-password"));
        assert_eq!(row("server_profile-0"), None);
        assert_eq!(
            read_credential_with_fallback_inner(&conn, &root, other.id, "server_profile-0", &read)
                .unwrap()
                .map(|value| value.to_string())
                .as_deref(),
            Some("owners-password")
        );
    }

    /// On a filesystem without file locks the list lock excludes nobody: the
    /// alignment still copies an account's own tokens into its partition, but
    /// the vault keeps its keys, which a holder added after the commit would
    /// read, and the account is not done: the next pass that excludes the
    /// writers keeps the row and removes the vault copy.
    #[test]
    fn without_file_locks_the_alignment_copies_and_a_later_pass_clears_the_vault() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_or_init_path(dir.path().join("partitions.db")).expect("open");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None).unwrap();
        clear_user_session();
        let profile = json!({"id": "p", "name": "p", "protocol": "dropbox", "host": "",
                             "port": 0, "username": "p@example.test"});
        replace_server_profiles_for(&mut conn, &root, other.id, &[profile]).unwrap();
        let (keys, read) = fake_vault(&[("oauth_dropbox_p", "mine-token")]);
        let deleted = RefCell::new(Vec::<String>::new());
        let delete = |key: &str, _: Option<&str>| {
            deleted.borrow_mut().push(key.to_string());
            Ok(true)
        };
        NO_FILE_LOCKS_IN_TEST.set(true);
        let aligned = align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        );
        NO_FILE_LOCKS_IN_TEST.set(false);
        aligned.unwrap();
        assert_eq!(
            get_user_credential_for(&conn, &root, other.id, "oauth_dropbox_p")
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("mine-token")
        );
        assert!(deleted.borrow().is_empty());
        assert!(!private_tokens_aligned(&conn, other.id).unwrap());

        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        assert_eq!(
            get_user_credential_for(&conn, &root, other.id, "oauth_dropbox_p")
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("mine-token")
        );
        assert_eq!(*deleted.borrow(), ["oauth_dropbox_p"]);
        assert!(private_tokens_aligned(&conn, other.id).unwrap());
    }

    /// A profile id with capitals: the alignment names its OAuth token as the
    /// OAuth runtime does (`oauth_dropbox_team` for `Team`). On a profile id
    /// of this account alone the token moves into its row and leaves the
    /// vault; on one the legacy owner also holds, the unstamped copy is
    /// dropped while the account's own stamped sign-in stays readable.
    #[test]
    fn a_mixed_case_profile_id_is_aligned_under_the_runtime_key() {
        let _guard = test_lock();
        let profile = |id: &str| {
            json!({"id": id, "name": id, "protocol": "dropbox", "host": "", "port": 0,
                   "username": format!("{id}@example.test")})
        };
        let key = |id: &str| crate::providers::oauth2::oauth_token_key("Dropbox", id);
        assert_eq!(key("Team"), "oauth_dropbox_team");
        let (team, crew) = (key("Team"), key("Crew"));
        for shared in [false, true] {
            clear_user_session();
            let dir = tempfile::tempdir().unwrap();
            let mut conn = open_or_init_path(dir.path().join("partitions.db")).expect("open");
            let root = test_root();
            migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
            let owner = nonprofile_secret_owner(&conn).unwrap().unwrap();
            let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None)
                .unwrap()
                .id;
            clear_user_session();
            let list = [profile("Team"), profile("Crew")];
            replace_server_profiles_for(&mut conn, &root, other, &list).unwrap();
            if shared {
                replace_server_profiles_for(&mut conn, &root, owner, &list).unwrap();
            }
            let deleted = RefCell::new(Vec::<String>::new());
            let delete = |key: &str, _: Option<&str>| {
                deleted.borrow_mut().push(key.to_string());
                Ok(true)
            };
            let row = |key: &str| {
                get_user_credential_for(&conn, &root, other, key)
                    .unwrap()
                    .map(|value| value.to_string())
            };
            if !shared {
                let (keys, read) = fake_vault(&[(team.as_str(), "mine-token")]);
                align_private_tokens_inner(
                    &conn,
                    &root,
                    other,
                    &keys,
                    &|| Ok(keys.clone()),
                    &read,
                    &delete,
                )
                .unwrap();
                assert_eq!(row(&team).as_deref(), Some("mine-token"), "moved");
                assert_eq!(
                    *deleted.borrow(),
                    std::slice::from_ref(&team),
                    "the vault let go"
                );
                assert!(private_tokens_aligned(&conn, other).unwrap());
                continue;
            }
            set_user_credential_for(&conn, &root, other, &team, "oauth", "owners-token").unwrap();
            set_signed_in_credential_for(&conn, &root, other, &crew, "oauth", "own-sign-in")
                .unwrap();
            let (keys, read) = fake_vault(&[
                (team.as_str(), "owners-token"),
                (crew.as_str(), "owners-crew"),
            ]);
            align_private_tokens_inner(
                &conn,
                &root,
                other,
                &keys,
                &|| Ok(keys.clone()),
                &read,
                &delete,
            )
            .unwrap();
            assert_eq!(row(&team), None, "the unstamped copy is dropped");
            assert_eq!(
                read_credential_owner_aware_inner(&conn, &root, other, &crew, &read)
                    .unwrap()
                    .map(|value| value.to_string())
                    .as_deref(),
                Some("own-sign-in"),
                "the stamped sign-in stays readable"
            );
            assert!(
                deleted.borrow().is_empty(),
                "the owner keeps its vault keys"
            );
            assert!(private_tokens_aligned(&conn, other).unwrap());
        }
    }

    /// Two accounts whose profile ids differ only in case (`team` of the
    /// legacy owner, `Team` of another account) name one OAuth token key,
    /// `oauth_dropbox_team`, which the vault holds for the owner. However the
    /// other account's row stands (none, a copy of the owner's token, or its
    /// own sign-in), its alignment copies nothing into it and the vault keeps
    /// the owner's token; then each account's connect read (the OAuth
    /// runtime's load) gets its own token or none, never the other's. While
    /// the owner's list has no token tags yet (a list written by an older
    /// build), the alignment waits for them and changes nothing.
    #[test]
    fn accounts_whose_profile_ids_differ_in_case_never_read_each_others_token() {
        use crate::providers::oauth2::{OAuth2Manager, OAuthProvider};
        if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
            "user_partitions::tests::accounts_whose_profile_ids_differ_in_case_never_read_each_others_token",
        ) {
            return;
        }
        let _env = crate::test_env::lock();
        let _guard = test_lock();
        std::env::remove_var("AEROFTP_USER_PASSPHRASE");
        CredentialStore::clear_cache();
        CredentialStore::bootstrap_master_password("case-collision-pass").expect("bootstrap");
        CredentialStore::unlock_with_master("case-collision-pass", None).expect("unlock");
        let store = CredentialStore::from_cache().expect("cached vault");
        init_or_migrate_cli(&store).expect("init partitions");
        let mut conn = open_or_init_cli().expect("partition DB");
        let root_key = store.derive_user_partition_wrapping_key();
        let owner = active_user_id(&conn).unwrap().expect("legacy owner");
        let other = create_user(&mut conn, &root_key, "Other", None, None, None)
            .unwrap()
            .id;
        let profile = |id: &str| {
            json!({"id": id, "name": id, "protocol": "dropbox", "host": "", "port": 0,
                   "username": format!("{id}@example.test")})
        };
        replace_server_profiles_for(&mut conn, &root_key, owner, &[profile("team")]).unwrap();
        replace_server_profiles_for(&mut conn, &root_key, other, &[profile("Team")]).unwrap();
        let key = crate::providers::oauth2::oauth_token_key("dropbox", "Team");
        assert_eq!(
            key,
            crate::providers::oauth2::oauth_token_key("dropbox", "team")
        );
        let record = |token: &str| {
            json!({"access_token": token, "refresh_token": null, "expires_at": null,
                   "token_type": "Bearer", "scopes": []})
            .to_string()
        };
        let align = || {
            let vault_keys = list_vault_keys(&store).unwrap();
            align_private_tokens_inner(
                &conn,
                &root_key,
                other,
                &vault_keys,
                &|| list_vault_keys(&store),
                &|key: &str| get_optional_secret(&store, key),
                &|key: &str, expected: Option<&str>| match expected {
                    Some(expected) => store
                        .delete_secret_if_unchanged(key, expected)
                        .map_err(|e| format!("Delete legacy secret: {e}")),
                    None => delete_optional_secret(&store, key).map(|_| true),
                },
            )
            .unwrap()
        };
        let load = |user: i64, id: &str| {
            OAuth2Manager::new()
                .with_credential_user(Some(user))
                .load_tokens(OAuthProvider::Dropbox, id)
                .map(|tokens| tokens.access_token.clone())
                .ok()
        };
        let row = |user: i64| {
            get_user_credential_for(&conn, &root_key, user, &key)
                .unwrap()
                .map(|value| value.to_string())
        };
        let owners = record("owners-token");
        // The owner's sign-in from before tokens were kept to their account:
        // the vault key alone.
        store.store(&key, &owners).unwrap();

        // The owner's list as an older build wrote it: no token tags.
        conn.execute(
            "UPDATE server_profiles SET token_uid = NULL WHERE user_id = ?1",
            params![owner],
        )
        .unwrap();
        align();
        assert!(!private_tokens_aligned(&conn, other).unwrap(), "it waits");
        assert_eq!(row(other), None, "nothing copied while it waits");
        assert_eq!(store.get(&key).ok().as_deref(), Some(owners.as_str()));
        fill_profile_token_tags(&conn, &root_key, owner).unwrap();

        for case in ["no row", "a copy of the owner's token", "its own sign-in"] {
            delete_user_credential_for(&conn, other, &key).unwrap();
            conn.execute(
                "DELETE FROM global_state WHERE key = ?1",
                params![format!("{PRIVATE_TOKENS_ALIGNED_PREFIX}{other}")],
            )
            .unwrap();
            let expected = match case {
                "no row" => None,
                "a copy of the owner's token" => {
                    set_user_credential_for(&conn, &root_key, other, &key, "oauth", &owners)
                        .unwrap();
                    None
                }
                _ => {
                    set_signed_in_credential_for(
                        &conn,
                        &root_key,
                        other,
                        &key,
                        "oauth",
                        &record("others-token"),
                    )
                    .unwrap();
                    Some("others-token")
                }
            };
            align();
            assert!(private_tokens_aligned(&conn, other).unwrap(), "{case}");
            assert_eq!(
                store.get(&key).ok().as_deref(),
                Some(owners.as_str()),
                "{case}: the vault keeps the owner's token"
            );
            assert_eq!(
                load(other, "Team").as_deref(),
                expected,
                "{case}: the other account reads its own token or none"
            );
            assert_eq!(
                load(owner, "team").as_deref(),
                Some("owners-token"),
                "{case}: the owner reads its own token"
            );
        }
    }

    /// The accounts of [`a_locked_account_holds_up_no_other_accounts_alignment`]
    /// and [`a_key_that_waited_for_token_tags_aligns_once_they_are_filled`]:
    /// the legacy owner; `Locked`, a passphrase account whose list an older
    /// build wrote (no token tags) and which stays locked, holding `team` and
    /// `ops` (Dropbox); and `Other`, holding `Team` and `Ops` (Dropbox, keys
    /// that lowercase the id) and `w` (Jottacloud, a key that keeps it). The
    /// vault holds a pre-partition save under `oauth_dropbox_team` and
    /// `jottacloud_refresh_w`; `Other` holds an unstamped row of
    /// `oauth_dropbox_ops`, and the vault none.
    struct TokenTagWait {
        store: CredentialStore,
        conn: Connection,
        root_key: [u8; 32],
        locked: i64,
        other: i64,
    }

    const TOKEN_TAG_WAIT_CHAIN: &str =
        r#"{"refresh_token":"others-chain","username":"jotta-user"}"#;

    fn token_tag_wait_fixture(pass: &str) -> TokenTagWait {
        std::env::remove_var("AEROFTP_USER_PASSPHRASE");
        CredentialStore::clear_cache();
        CredentialStore::bootstrap_master_password(pass).expect("bootstrap");
        CredentialStore::unlock_with_master(pass, None).expect("unlock");
        let store = CredentialStore::from_cache().expect("cached vault");
        init_or_migrate_cli(&store).expect("init partitions");
        let mut conn = open_or_init_cli().expect("partition DB");
        let root_key = store.derive_user_partition_wrapping_key();
        let profile = |id: &str, protocol: &str| {
            json!({"id": id, "name": id, "protocol": protocol, "host": "", "port": 0,
                   "username": format!("{id}@example.test")})
        };
        let locked = create_user(&mut conn, &root_key, "Locked", None, None, Some("pass"))
            .unwrap()
            .id;
        unlock_user(&conn, &root_key, locked, Some("pass")).unwrap();
        replace_server_profiles_for(
            &mut conn,
            &root_key,
            locked,
            &[profile("team", "dropbox"), profile("ops", "dropbox")],
        )
        .unwrap();
        let other = create_user(&mut conn, &root_key, "Other", None, None, None)
            .unwrap()
            .id;
        replace_server_profiles_for(
            &mut conn,
            &root_key,
            other,
            &[
                profile("Team", "dropbox"),
                profile("Ops", "dropbox"),
                profile("w", "jottacloud"),
            ],
        )
        .unwrap();
        // The locked account's list as an older build wrote it, and the
        // account locked: nothing can fill its tags.
        conn.execute(
            "UPDATE server_profiles SET token_uid = NULL WHERE user_id = ?1",
            params![locked],
        )
        .unwrap();
        clear_user_session();
        fill_profile_token_tags(&conn, &root_key, locked).unwrap();
        let pending: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM server_profiles
                 WHERE user_id = ?1 AND token_uid IS NULL)",
                params![locked],
                |row| row.get(0),
            )
            .unwrap();
        assert!(pending, "a locked account keeps its tags unfilled");
        store.store("oauth_dropbox_team", "owners-token").unwrap();
        store
            .store("jottacloud_refresh_w", TOKEN_TAG_WAIT_CHAIN)
            .unwrap();
        set_user_credential_for(
            &conn,
            &root_key,
            other,
            "oauth_dropbox_ops",
            "oauth",
            "a-copy",
        )
        .unwrap();
        TokenTagWait {
            store,
            conn,
            root_key,
            locked,
            other,
        }
    }

    impl TokenTagWait {
        fn align(&self) {
            let vault_keys = list_vault_keys(&self.store).unwrap();
            align_private_tokens_inner(
                &self.conn,
                &self.root_key,
                self.other,
                &vault_keys,
                &|| list_vault_keys(&self.store),
                &|key: &str| get_optional_secret(&self.store, key),
                &|key: &str, expected: Option<&str>| match expected {
                    Some(expected) => self
                        .store
                        .delete_secret_if_unchanged(key, expected)
                        .map_err(|e| format!("Delete legacy secret: {e}")),
                    None => delete_optional_secret(&self.store, key).map(|_| true),
                },
            )
            .unwrap();
        }

        fn row(&self, key: &str) -> Option<String> {
            get_user_credential_for(&self.conn, &self.root_key, self.other, key)
                .unwrap()
                .map(|value| value.to_string())
        }

        fn assert_waiting(&self, pass: &str) {
            assert_eq!(
                self.row("jottacloud_refresh_w").as_deref(),
                Some(TOKEN_TAG_WAIT_CHAIN),
                "{pass}: the key no other account can name moves into the partition"
            );
            assert!(
                self.store.get("jottacloud_refresh_w").is_err(),
                "{pass}: the vault let go of it"
            );
            assert_eq!(
                self.row("oauth_dropbox_team"),
                None,
                "{pass}: nothing copied for the key that waits"
            );
            assert_eq!(
                self.store.get("oauth_dropbox_team").ok().as_deref(),
                Some("owners-token"),
                "{pass}: the vault keeps the key that waits"
            );
            assert_eq!(
                self.row("oauth_dropbox_ops").as_deref(),
                Some("a-copy"),
                "{pass}: the row that waits stays as it is"
            );
            assert!(
                !private_tokens_aligned(&self.conn, self.other).unwrap(),
                "{pass}: the account stays unmarked for the pass that runs it"
            );
        }
    }

    /// An account locked behind its passphrase, whose list has no token tags
    /// yet, holds up no other account's alignment but the keys one of its
    /// profiles may name (a key that lowercases the id, as a vault key or as
    /// a row): the rest moves now, and a pass while it stays locked moves
    /// nothing twice and drops nothing.
    #[test]
    fn a_locked_account_holds_up_no_other_accounts_alignment() {
        if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
            "user_partitions::tests::a_locked_account_holds_up_no_other_accounts_alignment",
        ) {
            return;
        }
        let _env = crate::test_env::lock();
        let _guard = test_lock();
        let wait = token_tag_wait_fixture("locked-token-tag-wait-pass");
        wait.align();
        wait.assert_waiting("first pass");
        wait.align();
        wait.assert_waiting("a pass while the account stays locked");
    }

    /// Once the locked account is unlocked and its tags are filled, the next
    /// pass runs the keys that waited by the same rule: a key another account
    /// names is that account's (the vault keeps its token, the unstamped row
    /// is dropped as a copy), and the account is marked aligned.
    #[test]
    fn a_key_that_waited_for_token_tags_aligns_once_they_are_filled() {
        if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
            "user_partitions::tests::a_key_that_waited_for_token_tags_aligns_once_they_are_filled",
        ) {
            return;
        }
        let _env = crate::test_env::lock();
        let _guard = test_lock();
        let wait = token_tag_wait_fixture("filled-token-tag-wait-pass");
        wait.align();
        wait.assert_waiting("while the account is locked");
        unlock_user(&wait.conn, &wait.root_key, wait.locked, Some("pass")).unwrap();
        fill_profile_token_tags(&wait.conn, &wait.root_key, wait.locked).unwrap();
        clear_user_session();
        wait.align();
        assert!(
            private_tokens_aligned(&wait.conn, wait.other).unwrap(),
            "the pass that ran the keys that waited marks the account"
        );
        assert_eq!(
            wait.row("oauth_dropbox_team"),
            None,
            "the locked account names it"
        );
        assert_eq!(
            wait.store.get("oauth_dropbox_team").ok().as_deref(),
            Some("owners-token"),
            "the vault keeps the token the locked account names"
        );
        assert_eq!(
            wait.row("oauth_dropbox_ops"),
            None,
            "the unstamped row of a key the locked account names is dropped as a copy"
        );
        assert_eq!(
            wait.row("jottacloud_refresh_w").as_deref(),
            Some(TOKEN_TAG_WAIT_CHAIN),
            "the key moved earlier stays"
        );
    }

    /// A native Proton session belongs to one account. A copy of a session
    /// that another account also holds in the vault (same UID) is dropped from
    /// this one, row and vault copy, locally: the other account's copy is not
    /// touched, so it stays signed in. A session of this account alone moves
    /// into its partition, also when two of its own profiles hold it, and so
    /// does the session it signed in to before saving a profile.
    #[test]
    fn a_native_session_two_accounts_hold_stays_with_the_other_one() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None).unwrap();
        clear_user_session();
        let profile = |id: &str| {
            json!({"id": id, "name": id, "protocol": "proton", "host": "drive.proton.me",
                   "port": 443, "username": format!("{id}@example.test")})
        };
        replace_server_profiles_for(
            &mut conn,
            &root,
            other.id,
            &[profile("copied"), profile("own-a"), profile("own-b")],
        )
        .unwrap();
        let saved = |uid: &str| {
            json!({"uid": uid, "access_token": "a", "refresh_token": "r", "key_password": "k"})
                .to_string()
        };
        let (shared, own, pending) = (saved("shared"), saved("own"), saved("pending"));
        set_user_credential_for(
            &conn,
            &root,
            other.id,
            "proton_native_session_copied",
            "proton_native_session",
            &shared,
        )
        .unwrap();
        let unbound = format!("proton_native_unbound_user_{}", other.id);
        let (keys, read) = fake_vault(&[
            ("proton_native_session_profile-0", &shared),
            ("proton_native_session_copied", &shared),
            ("proton_native_session_own-a", &own),
            ("proton_native_session_own-b", &own),
            (&unbound, &pending),
        ]);
        let deleted = RefCell::new(Vec::<String>::new());
        let delete = |key: &str, _: Option<&str>| {
            deleted.borrow_mut().push(key.to_string());
            Ok(true)
        };
        align_private_tokens_inner(
            &conn,
            &root,
            other.id,
            &keys,
            &|| Ok(keys.clone()),
            &read,
            &delete,
        )
        .unwrap();
        let mut gone = deleted.borrow().clone();
        gone.sort();
        let mut expected = vec![
            "proton_native_session_copied".to_string(),
            "proton_native_session_own-a".to_string(),
            "proton_native_session_own-b".to_string(),
            unbound.clone(),
        ];
        expected.sort();
        assert_eq!(gone, expected);
        let row = |key: &str| {
            get_user_credential_for(&conn, &root, other.id, key)
                .unwrap()
                .and_then(|raw| proton_session_uid(&raw))
        };
        assert_eq!(row("proton_native_session_copied"), None);
        assert_eq!(row("proton_native_session_own-a").as_deref(), Some("own"));
        assert_eq!(row("proton_native_session_own-b").as_deref(), Some("own"));
        assert_eq!(row(&unbound).as_deref(), Some("pending"));
    }

    /// An account whose private tokens are not aligned yet is work for the
    /// boot pass, even when its secrets were copied long ago.
    #[test]
    fn an_account_not_aligned_yet_is_pending_for_the_boot_pass() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        create_passphrase_less_user(&mut conn, &root, "Other", None, None).unwrap();
        clear_user_session();
        for user in list_users(&conn).unwrap() {
            mark_creds_migrated(&conn, user.id).unwrap();
        }
        assert!(has_eager_pending_users(&conn).unwrap());
        for user in list_users(&conn).unwrap() {
            mark_private_tokens_aligned(&conn, user.id).unwrap();
        }
        assert!(!has_eager_pending_users(&conn).unwrap());
    }

    /// Whose a private token's vault key is, asked on a read-only connection
    /// before any owner is stored: answered, and nothing written.
    #[test]
    fn whose_vault_copy_it_is_is_answered_on_a_read_only_connection() {
        let _guard = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partitions.db");
        let mut conn = open_or_init_path(path.clone()).expect("open");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let other = create_passphrase_less_user(&mut conn, &root, "Other", None, None).unwrap();
        conn.execute(
            "DELETE FROM global_state WHERE key = ?1",
            params![LEGACY_OWNER_KEY],
        )
        .unwrap();
        drop(conn);
        let read_only =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert!(vault_copy_is_another_accounts(&read_only, other.id, "oauth_dropbox_p").unwrap());
        assert!(!vault_copy_is_another_accounts(&read_only, other.id, "server_p").unwrap());
        let stored: Option<String> = read_only
            .query_row(
                "SELECT value FROM global_state WHERE key = ?1",
                params![LEGACY_OWNER_KEY],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(stored, None);
    }

    /// A locked account that is not the legacy owner cannot take a save: its readers
    /// use only its partition, with or without a row there.
    #[test]
    fn a_locked_account_that_reads_only_its_partition_refuses_a_save() {
        let _guard = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let store = throwaway_vault(dir.path());
        let mut conn = open_or_init_path(dir.path().join("partitions.db")).expect("open");
        let root = store.derive_user_partition_wrapping_key();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let locked = create_user(&mut conn, &root, "Locked", None, None, Some("pass")).unwrap();
        unlock_user(&conn, &root, locked.id, Some("pass")).unwrap();
        set_user_credential_for(&conn, &root, locked.id, "server_with_row", "server", "old")
            .unwrap();
        clear_user_session();

        // Not the legacy owner: it reads only its partition, so a vault copy would be lost.
        assert!(
            mirror_into_partition(&conn, &store, locked.id, "server_no_row", "server", "new")
                .is_err()
        );
        let refused = mirror_into_partition(
            &conn,
            &store,
            locked.id,
            "server_with_row",
            "server",
            "new-value",
        )
        .unwrap_err();
        assert!(refused.contains("server_with_row"), "{refused}");
        assert!(refused.contains("USER_LOCKED"), "{refused}");
        assert!(!refused.contains("new-value"), "{refused}");
    }

    /// A load does not wait for a writer holding the lock, and does not touch the
    /// blob while it does.
    #[test]
    fn a_load_leaves_the_blob_to_a_writer_holding_the_list_lock() {
        let _guard = test_lock();
        let dir = tempfile::tempdir().unwrap();
        let store = throwaway_vault(dir.path());
        let mut conn = open_or_init_path(dir.path().join("partitions.db")).expect("open");
        let root = test_root();
        let seed = json!([{ "id": "p", "name": "P", "protocol": "sftp", "host": "h", "port": 22, "username": "u" }]);
        migrate_legacy_payloads(&mut conn, Some(&seed.to_string()), None, &root).expect("seed");
        let user = active_user_id(&conn).unwrap().unwrap();
        store.store(LEGACY_PROFILES_BLOB, "[]").unwrap();

        let held = ProfileListLock::acquire(&conn).unwrap().unwrap();
        mirror_after_load(&conn, &store, &root, user);
        assert_eq!(store.get(LEGACY_PROFILES_BLOB).unwrap(), "[]");
        drop(held);

        mirror_after_load(&conn, &store, &root, user);
        let mirrored: Vec<Value> =
            serde_json::from_str(&store.get(LEGACY_PROFILES_BLOB).unwrap()).unwrap();
        assert_eq!(mirrored[0]["id"], "p");
    }

    /// No partition database means nothing to mirror, and the blob is left as
    /// it is (a VaultOnly machine that never ran the partition migration).
    #[test]
    fn legacy_blob_refresh_without_a_partition_db_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let store = throwaway_vault(dir.path());
        store.store(LEGACY_PROFILES_BLOB, "[]").unwrap();
        let missing = dir.path().join("user_partitions.db");
        assert!(!refresh_legacy_profiles_blob_from_db(&store, &missing).unwrap());
        assert!(
            !missing.exists(),
            "a read-only refresh must not create the DB"
        );
        assert_eq!(store.get(LEGACY_PROFILES_BLOB).unwrap(), "[]");
    }

    #[test]
    fn reconcile_overlay_flags_selfheals_legacy_profile() {
        // Legacy profile (BUG-LT2a): enabled overlay, flag persisted false, yet
        // the secret IS in the vault. Recompute must flip the flags to reality.
        let mut profiles = vec![json!({
            "id": "srv_legacy",
            "name": "Legacy crypt",
            "aeroCryptOverlay": { "enabled": true, "kind": "rclone-crypt" },
            "hasStoredAeroCryptPassword": false,
            "hasStoredAeroCryptSalt": false,
        })];
        let present: std::collections::HashSet<&str> = [
            "aerocrypt_overlay_pw_srv_legacy",
            "aerocrypt_overlay_salt_srv_legacy",
        ]
        .into_iter()
        .collect();
        reconcile_overlay_secret_flags(|k| present.contains(k), &mut profiles);
        assert_eq!(profiles[0]["hasStoredAeroCryptPassword"], json!(true));
        assert_eq!(profiles[0]["hasStoredAeroCryptSalt"], json!(true));
        assert_eq!(profiles[0]["hasStoredAeroCryptKeyfilePath"], json!(false));
        assert_eq!(profiles[0]["hasStoredAeroCryptConfig"], json!(false));
    }

    #[test]
    fn reconcile_overlay_flags_respects_vault_truth() {
        let mut profiles = vec![
            // Disabled overlay: never touched, even with a stale true flag.
            json!({
                "id": "srv_off",
                "aeroCryptOverlay": { "enabled": false },
                "hasStoredAeroCryptPassword": true,
            }),
            // Enabled but the secret is genuinely gone: stale true is cleared to
            // false so the flag matches what the unlock would actually retrieve.
            json!({
                "id": "srv_gone",
                "aeroCryptOverlay": { "enabled": true },
                "hasStoredAeroCryptPassword": true,
            }),
            // No overlay binding at all: untouched.
            json!({ "id": "srv_plain", "protocol": "sftp" }),
        ];
        reconcile_overlay_secret_flags(|_| false, &mut profiles);
        assert_eq!(profiles[0]["hasStoredAeroCryptPassword"], json!(true));
        assert_eq!(profiles[1]["hasStoredAeroCryptPassword"], json!(false));
        assert!(profiles[2].get("hasStoredAeroCryptPassword").is_none());
    }

    fn migrated_conn(profile_count: usize) -> Connection {
        clear_user_session();
        let mut conn = Connection::open_in_memory().expect("memory db");
        let profiles: Vec<Value> = (0..profile_count)
            .map(|idx| {
                json!({
                    "id": format!("profile-{idx}"),
                    "name": format!("Profile {idx}"),
                    "protocol": "sftp",
                    "host": "example.com",
                    "port": 22,
                    "username": format!("user{idx}")
                })
            })
            .collect();
        let profiles_json = serde_json::to_string(&profiles).expect("profiles json");
        migrate_legacy_payloads(
            &mut conn,
            Some(&profiles_json),
            Some(r#"{"theme":"dark","sync":{"conflictStrategy":"newer"}}"#),
            &test_root(),
        )
        .expect("migrate");
        conn
    }

    /// A bridge scan with no account yet writes its ledger into a database
    /// that has its schema and no schema version. The move of the legacy list
    /// that follows adopts the OAuth apps its profiles use, as every other
    /// list writer does, so the scan's settle cannot take back an app a moved
    /// profile uses; the record of an app no moved profile uses stays.
    #[test]
    fn the_move_of_a_legacy_list_adopts_the_oauth_apps_its_profiles_use() {
        let _guard = test_lock();
        clear_user_session();
        let mut conn = Connection::open_in_memory().expect("memory db");
        init_db_schema(&conn).unwrap();
        for (entry, slot) in [
            (
                "bridge_import_entry_scan-a_0000000000",
                "oauth_dropbox_client_id",
            ),
            (
                "bridge_import_entry_scan-a_0000000001",
                "oauth_box_client_id",
            ),
        ] {
            let written = json!({"profile": "scanned", "key": slot, "kind": "oauth_app",
                                 "mac": "m", "holders": ["scanned"]});
            conn.execute(
                "INSERT INTO global_state(key, value, updated_at) VALUES (?1, ?2, 0)",
                params![entry, written.to_string()],
            )
            .unwrap();
        }
        let profiles = json!([{"id": "legacy-dropbox", "name": "D", "protocol": "dropbox",
                               "host": "", "port": 0}])
        .to_string();
        migrate_legacy_payloads(&mut conn, Some(&profiles), None, &test_root()).unwrap();
        let left: Vec<String> = conn
            .prepare(
                "SELECT key FROM global_state
                 WHERE substr(key, 1, 20) = 'bridge_import_entry_' ORDER BY key",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            left,
            vec!["bridge_import_entry_scan-a_0000000001".to_string()],
            "the moved Dropbox profile adopts its app; the Box record stays"
        );
    }

    /// R7-2: a database the move of the legacy list ran on counts as moved at
    /// any schema version from the move's on: one the upgrades have not
    /// brought to this build's yet, or one a newer build stamped before a
    /// downgrade. No version, one before the move, or one that is not a
    /// number does not. Red while the move is read from an exact match with
    /// this build's version: a bridge settle there
    /// ([`crate::bridge_import_scan`]) settles nothing for as long as the
    /// version differs, and a never-confirmed scan's secrets stay.
    #[test]
    fn a_database_the_move_ran_on_counts_as_moved_at_any_later_schema_version() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        assert!(
            legacy_payloads_moved(&conn).unwrap(),
            "this build's version"
        );
        let stamp = |version: &str| {
            conn.execute(
                "INSERT OR REPLACE INTO global_state(key, value, updated_at) VALUES (?1, ?2, 0)",
                params![SCHEMA_VERSION_KEY, version],
            )
            .unwrap();
        };
        for version in ["2", "3", "4", "999"] {
            stamp(version);
            assert!(legacy_payloads_moved(&conn).unwrap(), "version {version}");
        }
        for version in ["1", "0", "", "v5", "-5"] {
            stamp(version);
            assert!(
                !legacy_payloads_moved(&conn).unwrap(),
                "version {version:?}: not moved"
            );
        }
        conn.execute(
            "DELETE FROM global_state WHERE key = ?1",
            params![SCHEMA_VERSION_KEY],
        )
        .unwrap();
        assert!(
            !legacy_payloads_moved(&conn).unwrap(),
            "no version: the move has not run"
        );
    }

    #[test]
    fn conditional_owned_refresh_distinguishes_gui_switch_from_explicit_cli_owner() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let a = get_active_user(&conn).unwrap().unwrap().id;
        let b = create_user(&mut conn, &root, "Other user", None, None, None)
            .unwrap()
            .id;
        set_user_credential_for(&conn, &root, a, "oauth_fixture", "oauth", "A").unwrap();
        set_active_user(&conn, b).unwrap();
        assert!(replace_user_credential_if_unchanged(
            &conn,
            &root,
            a,
            "oauth_fixture",
            "oauth",
            ("A", "rotated-A"),
            true
        )
        .is_err());
        assert_eq!(
            get_user_credential_for(&conn, &root, a, "oauth_fixture")
                .unwrap()
                .unwrap()
                .as_str(),
            "A"
        );
        assert!(replace_user_credential_if_unchanged(
            &conn,
            &root,
            a,
            "oauth_fixture",
            "oauth",
            ("A", "rotated-A"),
            false
        )
        .unwrap());
        assert!(get_user_credential_for(&conn, &root, b, "oauth_fixture")
            .unwrap()
            .is_none());
    }

    #[test]
    fn conditional_owned_refresh_preserves_new_login_and_signed_out_row() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root = test_root();
        let user = get_active_user(&conn).unwrap().unwrap().id;
        set_user_credential_for(&conn, &root, user, "oauth_fixture", "oauth", "A").unwrap();
        set_user_credential_for(&conn, &root, user, "oauth_fixture", "oauth", "B").unwrap();
        assert!(!replace_user_credential_if_unchanged(
            &conn,
            &root,
            user,
            "oauth_fixture",
            "oauth",
            ("A", "refresh-A"),
            true,
        )
        .unwrap());
        assert_eq!(
            get_user_credential_for(&conn, &root, user, "oauth_fixture")
                .unwrap()
                .unwrap()
                .as_str(),
            "B"
        );
        assert!(replace_user_credential_if_unchanged(
            &conn,
            &root,
            user,
            "oauth_fixture",
            "oauth",
            ("B", "refresh-B"),
            true,
        )
        .unwrap());
        delete_user_credential_for(&conn, user, "oauth_fixture").unwrap();
        assert!(!replace_user_credential_if_unchanged(
            &conn,
            &root,
            user,
            "oauth_fixture",
            "oauth",
            ("refresh-B", "resurrected"),
            true,
        )
        .unwrap());
    }

    /// A vault from before the naming rule migrates with two profiles of one
    /// name, and the first write numbers the second. An import onto that list
    /// reports the names stored: a name claimed before the write ("Lab (1)")
    /// is the one the second old profile gets, and would select it.
    #[test]
    fn an_import_onto_two_profiles_of_one_name_reports_names_that_select_it() {
        use crate::util::profile_name::renamed_by_write;

        let _guard = test_lock();
        clear_user_session();
        let mut conn = Connection::open_in_memory().expect("memory db");
        migrate_legacy_payloads(&mut conn, None, None, &test_root()).expect("migrate");
        let user_id = active_user_id(&conn).unwrap().expect("active user");
        store_profiles_as_an_older_build(
            &conn,
            &test_root(),
            user_id,
            &[
                json!({"id": "a", "name": "Lab", "protocol": "sftp", "host": "one.example"}),
                json!({"id": "b", "name": "Lab", "protocol": "sftp", "host": "two.example"}),
            ],
        );
        let mut sent = list_server_profiles_for(&conn, &test_root(), user_id).expect("list");
        assert_eq!(sent.len(), 2, "an older build kept both: {sent:?}");
        sent.push(json!({"id": "c", "name": "Lab", "protocol": "sftp", "host": "three.example"}));

        let stored =
            replace_server_profiles_for(&mut conn, &test_root(), user_id, &sent).expect("write");
        let renamed = renamed_by_write(&sent, &stored);
        let read_back = list_server_profiles_for(&conn, &test_root(), user_id).expect("re-read");
        assert_eq!(read_back, stored, "the list returned is the list stored");
        assert_eq!(
            renamed
                .iter()
                .map(|r| (r.id.as_str(), r.to.as_str()))
                .collect::<Vec<_>>(),
            [("b", "Lab (1)"), ("c", "Lab (2)")]
        );
        for r in &renamed {
            // `--profile NAME` takes the first profile of that name,
            // case-insensitive.
            let selected = read_back.iter().find(|p| {
                p["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&r.to))
            });
            assert_eq!(selected.and_then(|p| p["id"].as_str()), Some(r.id.as_str()));
        }
    }

    /// The legacy store let two profiles share a name; the migration into the
    /// partition stores them as every write of a list does.
    #[test]
    fn the_migration_numbers_a_legacy_list_with_two_profiles_of_one_name() {
        let _guard = test_lock();
        clear_user_session();
        let mut conn = Connection::open_in_memory().expect("memory db");
        let legacy = json!([
            {"id": "a", "name": "Lab", "protocol": "sftp", "host": "one.example"},
            {"id": "b", "name": "lab", "protocol": "sftp", "host": "two.example"},
            {"id": "c", "name": "Prod", "protocol": "sftp", "host": "three.example"},
        ]);
        migrate_legacy_payloads(&mut conn, Some(&legacy.to_string()), None, &test_root())
            .expect("migrate");
        let user_id = active_user_id(&conn).unwrap().expect("active user");
        let stored = list_server_profiles_for(&conn, &test_root(), user_id).expect("list");
        let names: Vec<&str> = stored.iter().map(|p| p["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Lab", "lab (1)", "Prod"]);
    }

    #[test]
    fn a_restore_reset_covers_other_accounts_and_continues_after_a_corrupt_one() {
        let _guard = test_lock();
        clear_user_session();
        let mut conn = Connection::open_in_memory().expect("memory db");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, None, None, &root).expect("migrate");
        let default = active_user_id(&conn).unwrap().expect("active user");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", None, None).expect("bob");
        let relaxed = |id: &str, name: &str| {
            vec![json!({
                "id": id, "name": name, "protocol": "sftp", "host": "sftp.example.com",
                "options": {"trustUnknownHosts": true}
            })]
        };
        store_profiles_as_an_older_build(&conn, &root, default, &relaxed("a", "Default"));
        store_profiles_as_an_older_build(&conn, &root, bob.id, &relaxed("b", "Bob"));

        let report = reset_profile_relaxations(&mut conn, &root, Some(default)).unwrap();
        assert_eq!(report.reset.len(), 1);
        assert_eq!(report.reset[0].name, "Bob");
        assert_eq!(
            list_server_profiles_for(&conn, &root, default).unwrap()[0]["options"]
                ["trustUnknownHosts"],
            true
        );
        assert_eq!(
            list_server_profiles_for(&conn, &root, bob.id).unwrap()[0]["options"]
                ["trustUnknownHosts"],
            false
        );

        store_profiles_as_an_older_build(&conn, &root, bob.id, &relaxed("b2", "Bob again"));
        conn.execute(
            "UPDATE server_profiles SET encrypted_blob = X'00' WHERE user_id = ?1",
            params![default],
        )
        .unwrap();
        let report = reset_profile_relaxations(&mut conn, &root, None).unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(report.reset.len(), 1, "the later account is still reset");
        assert_eq!(report.reset[0].name, "Bob again");
        assert_eq!(report.left, 0);
    }

    #[test]
    fn a_passphrase_account_is_reset_before_its_first_use_after_restore() {
        let _guard = test_lock();
        clear_user_session();
        let mut conn = Connection::open_in_memory().expect("memory db");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, None, None, &root).expect("migrate");
        let locked = create_user(&mut conn, &root, "Locked", None, None, Some("passphrase"))
            .expect("locked");
        let row = read_user_key_row(&conn, locked.id).unwrap();
        let dek = unwrap_user_dek_with_passphrase(&row, "passphrase").unwrap();
        insert_profile_rows(
            &conn,
            &user_crypto::secret_key_from_bytes(&root),
            locked.id,
            &dek,
            &[json!({
                "id": "locked-profile", "name": "Locked profile", "protocol": "sftp",
                "host": "sftp.example.com", "options": {"trustUnknownHosts": true}
            })],
        )
        .unwrap();
        let report = reset_profile_relaxations(&mut conn, &root, None).unwrap();
        assert_eq!(report.left, 1);
        assert!(security_reset_pending(&conn, locked.id).unwrap());

        clear_user_session();
        let status = unlock_user(&conn, &root, locked.id, Some("passphrase")).unwrap();
        assert_eq!(status.security_reset.len(), 1);
        assert!(!security_reset_pending(&conn, locked.id).unwrap());
        assert_eq!(
            list_server_profiles_for(&conn, &root, locked.id).unwrap()[0]["options"]
                ["trustUnknownHosts"],
            false
        );
    }

    #[test]
    fn a_file_backed_switch_enforces_and_resets_a_pending_account_with_a_live_session() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DB_FILENAME);
        let mut conn = open_or_init_path(path).unwrap();
        let root = test_root();
        migrate_legacy_payloads(&mut conn, None, None, &root).unwrap();
        let default = active_user_id(&conn).unwrap().unwrap();
        let locked =
            create_user(&mut conn, &root, "Locked", None, None, Some("passphrase")).unwrap();
        let row = read_user_key_row(&conn, locked.id).unwrap();
        let dek = unwrap_user_dek_with_passphrase(&row, "passphrase").unwrap();
        insert_profile_rows(
            &conn,
            &user_crypto::secret_key_from_bytes(&root),
            locked.id,
            &dek,
            &[json!({
                "id": "live", "name": "Live", "protocol": "sftp", "host": "sftp.example.com",
                "options": {"trustUnknownHosts": true}
            })],
        )
        .unwrap();
        unlock_user(&conn, &root, locked.id, Some("passphrase")).unwrap();
        let report = reset_profile_relaxations(&mut conn, &root, None).unwrap();
        assert_eq!(report.left, 1);
        assert_eq!(
            list_server_profiles_for(&conn, &root, locked.id).unwrap_err(),
            "SECURITY_RESET_PENDING"
        );
        unlock_user(&conn, &root, default, None).unwrap();
        assert_eq!(active_user_id(&conn).unwrap(), Some(default));
        let status = unlock_user(&conn, &root, locked.id, Some("passphrase")).unwrap();
        assert_eq!(status.security_reset.len(), 1);
        assert_eq!(
            list_server_profiles_for(&conn, &root, locked.id).unwrap()[0]["options"]
                ["trustUnknownHosts"],
            false
        );
    }

    #[test]
    fn a_failed_unlock_reset_rolls_back_profiles_and_keeps_the_marker() {
        let _guard = test_lock();
        clear_user_session();
        let mut conn = Connection::open_in_memory().unwrap();
        let root = test_root();
        migrate_legacy_payloads(&mut conn, None, None, &root).unwrap();
        let locked =
            create_user(&mut conn, &root, "Locked", None, None, Some("passphrase")).unwrap();
        let row = read_user_key_row(&conn, locked.id).unwrap();
        let dek = unwrap_user_dek_with_passphrase(&row, "passphrase").unwrap();
        insert_profile_rows(
            &conn,
            &user_crypto::secret_key_from_bytes(&root),
            locked.id,
            &dek,
            &[json!({
                "id": "safe-from-loss", "name": "Still here", "protocol": "sftp",
                "host": "sftp.example.com", "options": {"trustUnknownHosts": true}
            })],
        )
        .unwrap();
        reset_profile_relaxations(&mut conn, &root, None).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_security_reset BEFORE INSERT ON server_profiles BEGIN SELECT RAISE(FAIL, 'forced insert failure'); END;",
        )
        .unwrap();
        assert!(unlock_user(&conn, &root, locked.id, Some("passphrase")).is_err());
        conn.execute_batch("DROP TRIGGER fail_security_reset")
            .unwrap();
        assert!(security_reset_pending(&conn, locked.id).unwrap());
        let profiles = read_profiles_with_dek(&conn, locked.id, &dek).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0]["name"], "Still here");
        assert_eq!(profiles[0]["options"]["trustUnknownHosts"], true);
        assert!(unlock_user(&conn, &root, locked.id, Some("passphrase")).is_ok());
    }

    #[test]
    fn memory_databases_do_not_share_persistent_reset_markers() {
        let _guard = test_lock();
        let first = Connection::open_in_memory().unwrap();
        let second = Connection::open_in_memory().unwrap();
        assert!(security_reset_marker_path(&first, 1).is_none());
        assert!(security_reset_marker_path(&second, 1).is_none());
        block_security_reset(&first, 1);
        assert!(security_reset_blocked(&first, 1));
        assert!(!security_reset_blocked(&second, 1));
        unblock_security_reset(&first, 1);
        assert!(!security_reset_blocked(&first, 1));
    }

    #[test]
    fn a_failed_reset_transaction_start_blocks_until_a_successful_unlock() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let store = throwaway_vault(dir.path());
        let root = store.derive_user_partition_wrapping_key();
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        let path = dir.path().join(DB_FILENAME);
        let mut conn = open_or_init_path(path.clone()).unwrap();
        conn.busy_timeout(std::time::Duration::from_millis(1))
            .unwrap();
        migrate_legacy_payloads(&mut conn, None, None, &root).unwrap();
        let user = active_user_id(&conn).unwrap().unwrap();
        let row = read_user_key_row(&conn, user).unwrap();
        let dek = unwrap_user_dek_with_key(&row, &root_secret).unwrap();
        insert_profile_rows(
            &conn,
            &root_secret,
            user,
            &dek,
            &[json!({
                "id": "relaxed", "name": "Relaxed", "protocol": "sftp",
                "host": "sftp.example.com", "options": {"trustUnknownHosts": true}
            })],
        )
        .unwrap();
        let locker = Connection::open(&path).unwrap();
        locker.execute_batch("BEGIN EXCLUSIVE").unwrap();

        let report = reset_profile_relaxations(&mut conn, &root, None).unwrap();
        assert_eq!(report.failed, 1);
        assert!(report.persistent_failure);
        assert!(security_reset_blocked(&conn, user));
        assert_eq!(
            list_server_profiles_for(&conn, &root, user).unwrap_err(),
            "SECURITY_RESET_PENDING"
        );

        drop(locker);
        let status = unlock_user(&conn, &root, user, None).unwrap();
        assert_eq!(status.security_reset.len(), 1);
        assert!(!security_reset_blocked(&conn, user));
    }

    #[test]
    fn a_failed_reset_write_marks_the_account_pending_before_a_retry() {
        let _guard = test_lock();
        clear_user_session();
        let mut conn = Connection::open_in_memory().unwrap();
        let root = test_root();
        migrate_legacy_payloads(&mut conn, None, None, &root).unwrap();
        let user = active_user_id(&conn).unwrap().unwrap();
        let row = read_user_key_row(&conn, user).unwrap();
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        let dek = unwrap_user_dek_with_key(&row, &root_secret).unwrap();
        insert_profile_rows(
            &conn,
            &root_secret,
            user,
            &dek,
            &[json!({
                "id": "relaxed", "name": "Relaxed", "protocol": "sftp",
                "host": "sftp.example.com", "options": {"trustUnknownHosts": true}
            })],
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_restore_reset BEFORE INSERT ON server_profiles BEGIN SELECT RAISE(FAIL, 'forced insert failure'); END;",
        )
        .unwrap();
        let report = reset_profile_relaxations(&mut conn, &root, None).unwrap();
        assert_eq!(report.failed, 1);
        assert!(security_reset_pending(&conn, user).unwrap());
        assert_eq!(
            list_server_profiles_for(&conn, &root, user).unwrap_err(),
            "SECURITY_RESET_PENDING"
        );
        conn.execute_batch("DROP TRIGGER fail_restore_reset")
            .unwrap();
        let retry = reset_profile_relaxations(&mut conn, &root, None).unwrap();
        assert_eq!(retry.reset.len(), 1);
        assert!(!security_reset_pending(&conn, user).unwrap());
        assert_eq!(
            list_server_profiles_for(&conn, &root, user).unwrap()[0]["options"]
                ["trustUnknownHosts"],
            false
        );
    }

    #[test]
    fn a_double_write_failure_blocks_a_passphrase_free_account_until_reset() {
        let _guard = test_lock();
        clear_user_session();
        let mut conn = Connection::open_in_memory().unwrap();
        let root = test_root();
        migrate_legacy_payloads(&mut conn, None, None, &root).unwrap();
        let user = active_user_id(&conn).unwrap().unwrap();
        let row = read_user_key_row(&conn, user).unwrap();
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        let dek = unwrap_user_dek_with_key(&row, &root_secret).unwrap();
        insert_profile_rows(
            &conn,
            &root_secret,
            user,
            &dek,
            &[json!({
                "id": "relaxed", "name": "Relaxed", "protocol": "sftp",
                "host": "sftp.example.com", "options": {"trustUnknownHosts": true}
            })],
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_restore_reset BEFORE INSERT ON server_profiles BEGIN SELECT RAISE(FAIL, 'forced reset failure'); END;
             CREATE TRIGGER fail_pending_marker BEFORE INSERT ON global_state BEGIN SELECT RAISE(FAIL, 'forced marker failure'); END;",
        )
        .unwrap();
        let report = reset_profile_relaxations(&mut conn, &root, None).unwrap();
        assert_eq!(report.failed, 1);
        assert!(!security_reset_pending(&conn, user).unwrap());
        assert!(security_reset_blocked(&conn, user));
        assert_eq!(
            list_server_profiles_for(&conn, &root, user).unwrap_err(),
            "SECURITY_RESET_PENDING"
        );
        conn.execute_batch("DROP TRIGGER fail_restore_reset; DROP TRIGGER fail_pending_marker;")
            .unwrap();
        let status = unlock_user(&conn, &root, user, None).unwrap();
        assert_eq!(status.security_reset.len(), 1);
        assert!(!security_reset_blocked(&conn, user));
        assert_eq!(
            list_server_profiles_for(&conn, &root, user).unwrap()[0]["options"]
                ["trustUnknownHosts"],
            false
        );
    }

    #[test]
    fn a_double_write_failure_quarantines_the_database_before_a_restart() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let store = throwaway_vault(dir.path());
        let root = store.derive_user_partition_wrapping_key();
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        let path = dir.path().join(DB_FILENAME);
        let mut conn = open_or_init_path(path.clone()).unwrap();
        migrate_legacy_payloads(&mut conn, None, None, &root).unwrap();
        let user = active_user_id(&conn).unwrap().unwrap();
        let row = read_user_key_row(&conn, user).unwrap();
        let dek = unwrap_user_dek_with_key(&row, &root_secret).unwrap();
        insert_profile_rows(
            &conn,
            &root_secret,
            user,
            &dek,
            &[json!({
                "id": "relaxed", "name": "Relaxed", "protocol": "sftp",
                "host": "sftp.example.com", "options": {"trustUnknownHosts": true}
            })],
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_restore_reset BEFORE INSERT ON server_profiles BEGIN SELECT RAISE(FAIL, 'forced reset failure'); END;
             CREATE TRIGGER fail_pending_marker BEFORE INSERT ON global_state BEGIN SELECT RAISE(FAIL, 'forced marker failure'); END;",
        )
        .unwrap();
        drop(conn);

        let report = reset_profile_relaxations_in_db(&store, &path, None).unwrap();
        assert_eq!(report.failed, 1);
        assert!(report.persistent_failure);
        assert!(!path.exists(), "the unmarked restored DB cannot reopen");
        let preserved = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("user_partitions.db.security-reset-failed-")
                            && !name.ends_with("-wal")
                            && !name.ends_with("-shm")
                    })
            })
            .expect("quarantined database");
        assert_eq!(report.quarantine_path.as_ref().unwrap(), &preserved);
        assert!(preserved.metadata().unwrap().len() > 0);

        let fresh = open_or_init_path(path.clone()).unwrap();
        let profiles = fresh
            .query_row("SELECT COUNT(*) FROM server_profiles", [], |row| {
                row.get::<_, usize>(0)
            })
            .unwrap();
        assert_eq!(profiles, 0, "a restart creates no unsafe restored profile");
    }

    #[test]
    fn a_failed_quarantine_keeps_a_persistent_block_after_restart() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().unwrap();
        let store = throwaway_vault(dir.path());
        let root = store.derive_user_partition_wrapping_key();
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        let path = dir.path().join(DB_FILENAME);
        let mut conn = open_or_init_path(path.clone()).unwrap();
        migrate_legacy_payloads(&mut conn, None, None, &root).unwrap();
        let user = active_user_id(&conn).unwrap().unwrap();
        let row = read_user_key_row(&conn, user).unwrap();
        let dek = unwrap_user_dek_with_key(&row, &root_secret).unwrap();
        insert_profile_rows(
            &conn,
            &root_secret,
            user,
            &dek,
            &[json!({
                "id": "relaxed", "name": "Relaxed", "protocol": "sftp",
                "host": "sftp.example.com", "options": {"trustUnknownHosts": true}
            })],
        )
        .unwrap();
        block_security_reset(&conn, user);
        let error = quarantine_security_reset_db_with(&path, |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "forced rename failure",
            ))
        })
        .unwrap_err();
        assert!(error.contains("forced rename failure"), "{error}");
        drop(conn);

        forget_security_reset_blocks_for_tests();
        let fresh = open_or_init_path(path.clone()).unwrap();
        assert!(security_reset_blocked(&fresh, user));
        assert_eq!(
            list_server_profiles_for(&fresh, &root, user).unwrap_err(),
            "SECURITY_RESET_PENDING"
        );
        unblock_security_reset(&fresh, user);
        assert!(!security_reset_blocked(&fresh, user));
    }

    #[test]
    fn a_failed_pending_marker_invalidates_the_held_session() {
        let _guard = test_lock();
        clear_user_session();
        let mut conn = Connection::open_in_memory().unwrap();
        let root = test_root();
        migrate_legacy_payloads(&mut conn, None, None, &root).unwrap();
        let locked =
            create_user(&mut conn, &root, "Locked", None, None, Some("passphrase")).unwrap();
        let row = read_user_key_row(&conn, locked.id).unwrap();
        let dek = unwrap_user_dek_with_passphrase(&row, "passphrase").unwrap();
        insert_profile_rows(
            &conn,
            &user_crypto::secret_key_from_bytes(&root),
            locked.id,
            &dek,
            &[json!({
                "id": "relaxed", "name": "Relaxed", "protocol": "sftp",
                "host": "sftp.example.com", "options": {"trustUnknownHosts": true}
            })],
        )
        .unwrap();
        unlock_user(&conn, &root, locked.id, Some("passphrase")).unwrap();
        let generation = session_generation();
        conn.execute_batch(
            "CREATE TRIGGER fail_pending_marker BEFORE INSERT ON global_state BEGIN SELECT RAISE(FAIL, 'forced marker failure'); END;",
        )
        .unwrap();
        let report = reset_profile_relaxations(&mut conn, &root, None).unwrap();
        assert_eq!(report.failed, 1);
        assert!(session_generation() > generation);
        assert!(list_server_profiles_for(&conn, &root, locked.id).is_err());
        assert!(security_reset_blocked(&conn, locked.id));
        conn.execute_batch("DROP TRIGGER fail_pending_marker")
            .unwrap();
        let status = unlock_user(&conn, &root, locked.id, Some("passphrase")).unwrap();
        assert_eq!(status.security_reset.len(), 1);
        assert!(!security_reset_pending(&conn, locked.id).unwrap());
        assert!(!security_reset_blocked(&conn, locked.id));
    }

    /// A database written whole (a restored backup from an older build) is put
    /// right as the next write of each list would: every account this machine
    /// opens, once; a passphrase account is left to its first write after
    /// unlock and counted.
    #[test]
    fn a_database_from_an_older_build_is_put_right_for_every_account_it_can_open() {
        let _guard = test_lock();
        clear_user_session();
        let mut conn = Connection::open_in_memory().expect("memory db");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, None, None, &root).expect("migrate");
        let default = active_user_id(&conn).unwrap().expect("active user");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", None, None).expect("bob");
        create_user(&mut conn, &root, "Locked", None, None, Some("passphrase")).expect("locked");
        let old = |name_b: &str| {
            vec![
                json!({"id": "a", "name": "Lab", "protocol": "sftp", "host": "one.example"}),
                json!({"id": "b", "name": name_b, "protocol": "sftp", "host": "two.example"}),
            ]
        };
        store_profiles_as_an_older_build(&conn, &root, default, &old("lab"));
        store_profiles_as_an_older_build(&conn, &root, bob.id, &old("Lab"));

        assert_eq!(put_right_profile_names(&mut conn, &root), Ok((2, 1)));
        for (user, second) in [(default, "lab (1)"), (bob.id, "Lab (1)")] {
            let stored = list_server_profiles_for(&conn, &root, user).expect("list");
            let names: Vec<&str> = stored.iter().map(|p| p["name"].as_str().unwrap()).collect();
            assert_eq!(names, ["Lab", second]);
        }
        assert_eq!(
            put_right_profile_names(&mut conn, &root),
            Ok((0, 1)),
            "a list put right is left as it is"
        );
    }

    /// A save committed by another connection while a profile write reads
    /// the list it replaces cannot make that write fail: the write holds the
    /// database's write reservation from before its first read, so the other
    /// save waits (here, with no busy timeout, it is refused) and lands after.
    /// In a deferred transaction the other save went through and this write's
    /// DELETE failed with SQLITE_BUSY_SNAPSHOT.
    #[test]
    fn a_save_from_another_connection_cannot_break_a_profile_write_mid_read() {
        use std::sync::{Arc, Mutex};

        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(DB_FILENAME);
        let mut a = open_or_init_path(path.clone()).expect("open a");
        // Enough rows that the write's read runs well past the progress
        // handler's interval, which BEGIN alone never reaches.
        let profiles: Vec<Value> = (0..50)
            .map(|idx| json!({"id": format!("p{idx}"), "name": format!("P{idx}"), "protocol": "sftp"}))
            .collect();
        migrate_legacy_payloads(
            &mut a,
            Some(&serde_json::to_string(&profiles).unwrap()),
            None,
            &test_root(),
        )
        .expect("migrate");
        let b = open_or_init_path(path).expect("open b");
        b.execute_batch("CREATE TABLE contention_probe(x INTEGER)")
            .expect("probe table");
        b.busy_timeout(std::time::Duration::ZERO).expect("no wait");

        let other = Arc::new(Mutex::new((Some(b), Vec::<Result<(), String>>::new())));
        let in_handler = Arc::clone(&other);
        let user_id = active_user_id(&a).unwrap().expect("active user");
        let root_secret = user_crypto::secret_key_from_bytes(&test_root());
        let written = with_user_dek(&a, &root_secret, user_id, |user_id, dek| {
            a.progress_handler(
                10,
                Some(move || {
                    let mut other = in_handler.lock().unwrap();
                    if other.1.is_empty() {
                        let attempt = other
                            .0
                            .as_ref()
                            .unwrap()
                            .execute("INSERT INTO contention_probe VALUES (1)", [])
                            .map(|_| ())
                            .map_err(|e| e.to_string());
                        other.1.push(attempt);
                    }
                    false
                }),
            );
            let written = write_profiles_with_dek(&a, &root_secret, user_id, dek, &profiles);
            a.progress_handler(0, None::<fn() -> bool>);
            written
        });

        let mut other = other.lock().unwrap();
        assert_eq!(
            other.1.len(),
            1,
            "the other save never ran during the write"
        );
        assert!(
            other.1[0].as_ref().is_err_and(|e| e.contains("locked")),
            "the other save went through mid-read: {:?}",
            other.1[0]
        );
        assert_eq!(written.expect("the profile write").len(), 50);
        let b = other.0.take().unwrap();
        b.execute("INSERT INTO contention_probe VALUES (1)", [])
            .expect("the other save lands once the write commits");
    }

    #[test]
    fn second_connection_migration_is_idempotent_no_unique_failure() {
        // Regression for the GUI + CLI race that surfaced as
        // `Create default user: UNIQUE constraint failed: users.name_canonical`.
        // Guards the observable end-state contract on a shared on-disk DB: a second
        // independent connection must not fail on the default-user insert and must
        // report already_migrated, leaving exactly one default user. (The truly
        // simultaneous interleaving is prevented by the IMMEDIATE write lock plus
        // the in-transaction schema re-check in migrate_legacy_payloads.)
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("user_partitions.db");
        let profiles_json =
            r#"[{"id":"p0","name":"P0","protocol":"sftp","host":"h","port":22,"username":"u"}]"#;
        let settings_json = r#"{"theme":"dark"}"#;

        let mut a = Connection::open(&db_path).expect("open a");
        let r1 = migrate_legacy_payloads(
            &mut a,
            Some(profiles_json),
            Some(settings_json),
            &test_root(),
        )
        .expect("first migrate");
        assert!(!r1.already_migrated);
        assert!(r1.created_default_user);

        let mut b = Connection::open(&db_path).expect("open b");
        let r2 = migrate_legacy_payloads(
            &mut b,
            Some(profiles_json),
            Some(settings_json),
            &test_root(),
        )
        .expect("second migrate must not UNIQUE-fail");
        assert!(r2.already_migrated);

        let users = list_users(&b).expect("list users");
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].name, DEFAULT_USER_NAME);
    }

    #[test]
    fn legacy_migration_resumes_when_default_user_already_exists() {
        let _guard = test_lock();
        clear_user_session();
        let mut conn = Connection::open_in_memory().expect("memory db");
        init_db_schema(&conn).expect("schema");
        let root = test_root();
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        let tx = conn.transaction().expect("tx");
        let (default_id, _default_dek, created) =
            insert_default_user(&tx, &root_secret, now_ms()).expect("seed default");
        assert!(created);
        tx.commit().expect("commit seed");
        assert_eq!(
            current_schema_version(&conn).expect("schema version"),
            None,
            "precondition: partial migration has no schema marker"
        );

        let profiles_json =
            r#"[{"id":"p0","name":"P0","protocol":"sftp","host":"h","port":22,"username":"u"}]"#;
        let settings_json = r#"{"theme":"dark"}"#;
        let report =
            migrate_legacy_payloads(&mut conn, Some(profiles_json), Some(settings_json), &root)
                .expect("resume migration");
        assert!(!report.created_default_user);
        assert_eq!(report.migrated_profiles, 1);
        assert_eq!(report.migrated_settings_scopes, 1);

        let users = list_users(&conn).expect("list users");
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].id, default_id);
        let profile_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM server_profiles", [], |row| row.get(0))
            .expect("profile count");
        assert_eq!(profile_count, 1);
        assert_eq!(
            current_schema_version(&conn)
                .expect("schema version")
                .as_deref(),
            Some(SCHEMA_VERSION)
        );

        let second =
            migrate_legacy_payloads(&mut conn, Some(profiles_json), Some(settings_json), &root)
                .expect("second migrate");
        assert!(second.already_migrated);
    }

    #[test]
    fn fresh_migration_creates_default_user_and_preserves_legacy_backup() {
        let _guard = test_lock();
        let conn = migrated_conn(2);
        let users = list_users(&conn).expect("list users");
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].name, DEFAULT_USER_NAME);
        assert!(users[0].is_active);

        let profile_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM server_profiles", [], |row| row.get(0))
            .expect("profile count");
        assert_eq!(profile_count, 2);

        let settings_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM user_settings", [], |row| row.get(0))
            .expect("settings count");
        assert_eq!(settings_count, 1);

        let schema_version = current_schema_version(&conn).expect("schema version");
        assert_eq!(schema_version.as_deref(), Some(SCHEMA_VERSION));
        let backup: String = conn
            .query_row(
                "SELECT value FROM global_state WHERE key = ?1",
                params![LEGACY_PROFILES_KEY],
                |row| row.get(0),
            )
            .expect("legacy backup");
        assert!(backup.contains("\"enc\":\"aes-256-gcm\""));
        assert!(!backup.contains("Profile 0"));
        assert!(!backup.contains("example.com"));
    }

    #[test]
    fn migration_handles_thirty_profiles_and_duplicate_legacy_ids() {
        let _guard = test_lock();
        let mut conn = Connection::open_in_memory().expect("memory db");
        let profiles: Vec<Value> = (0..30)
            .map(|idx| {
                json!({
                    "id": "duplicated-id",
                    "name": format!("Duplicate {idx}"),
                    "protocol": "webdav",
                    "host": "dav.example.com",
                    "username": "same"
                })
            })
            .collect();
        let profiles_json = serde_json::to_string(&profiles).expect("profiles json");
        let report = migrate_legacy_payloads(&mut conn, Some(&profiles_json), None, &test_root())
            .expect("migrate");
        assert_eq!(report.migrated_profiles, 30);

        let unique_count: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT profile_uid) FROM server_profiles",
                [],
                |row| row.get(0),
            )
            .expect("unique count");
        assert_eq!(unique_count, 30);
    }

    #[test]
    fn migration_is_idempotent() {
        let _guard = test_lock();
        let mut conn = migrated_conn(3);
        let report = migrate_legacy_payloads(&mut conn, Some("[]"), Some("{}"), &test_root())
            .expect("rerun");
        assert!(report.already_migrated);
        let user_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .expect("user count");
        let profile_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM server_profiles", [], |row| row.get(0))
            .expect("profile count");
        assert_eq!(user_count, 1);
        assert_eq!(profile_count, 3);
    }

    /// The load-time crypt migration writes through this: the list is re-read
    /// and written in one transaction, and a change that `apply` does not
    /// report is not written at all.
    #[test]
    fn update_server_profiles_writes_only_a_reported_change() {
        let _guard = test_lock();
        let mut conn = migrated_conn(2);
        let root = test_root();
        let before = list_active_server_profiles(&conn, &root).expect("load");

        let untouched = update_active_server_profiles(&mut conn, &root, |p| {
            p[0]["name"] = json!("changed but not reported");
            false
        })
        .expect("update");
        // Not written, and what comes back is the list as stored, not the
        // copy `apply` changed.
        assert_eq!(untouched, (before.clone(), false));
        assert_eq!(
            list_active_server_profiles(&conn, &root).expect("reload"),
            before
        );

        let (written, was_written) = update_active_server_profiles(&mut conn, &root, |p| {
            p[1]["name"] = json!("renamed");
            true
        })
        .expect("update");
        assert!(was_written);
        assert_eq!(written[1]["name"], "renamed");
        let after = list_active_server_profiles(&conn, &root).expect("reload");
        assert_eq!(after, written);
        assert_eq!(after[0], before[0]);
    }

    #[test]
    fn active_profiles_round_trip_through_encrypted_rows() {
        let _guard = test_lock();
        let mut conn = migrated_conn(2);
        let root = test_root();
        let before = list_active_server_profiles(&conn, &root).expect("load migrated profiles");
        assert_eq!(before.len(), 2);
        assert_eq!(before[0]["name"], "Profile 0");

        let replacement = vec![
            json!({"id":"new-a","name":"New A","protocol":"sftp","host":"a.example.com"}),
            json!({"id":"new-b","name":"New B","protocol":"webdav","host":"b.example.com"}),
        ];
        replace_active_server_profiles(&mut conn, &root, &replacement).expect("replace profiles");

        let after = list_active_server_profiles(&conn, &root).expect("reload profiles");
        assert_eq!(after, replacement);
        let encrypted_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM server_profiles", [], |row| row.get(0))
            .expect("profile count");
        assert_eq!(encrypted_count, 2);
    }

    /// Every write of the list goes through one place, so no window, command
    /// or import can store two profiles of one name: the one that had the
    /// name keeps it, the one created or renamed onto it gets " (1)", and the
    /// list returned is the list stored.
    #[test]
    fn no_write_stores_two_profiles_of_one_name() {
        let _guard = test_lock();
        let mut conn = migrated_conn(2);
        let root = test_root();
        let before = list_active_server_profiles(&conn, &root).expect("load migrated profiles");

        let mut profiles = before.clone();
        profiles
            .push(json!({"id":"new","name":"profile 0","protocol":"sftp","host":"n.example.com"}));
        let stored =
            replace_active_server_profiles(&mut conn, &root, &profiles).expect("add a profile");
        let names = |list: &[Value]| -> Vec<String> {
            list.iter()
                .map(|p| p["name"].as_str().unwrap_or_default().to_string())
                .collect()
        };
        assert_eq!(names(&stored), ["Profile 0", "Profile 1", "profile 0 (1)"]);
        assert_eq!(
            list_active_server_profiles(&conn, &root).expect("reload"),
            stored
        );

        let (renamed, _) = update_active_server_profiles(&mut conn, &root, |p| {
            p[1]["name"] = json!("PROFILE 0");
            true
        })
        .expect("rename onto a taken name");
        assert_eq!(
            names(&renamed),
            ["Profile 0", "PROFILE 0 (2)", "profile 0 (1)"]
        );
        assert_eq!(
            list_active_server_profiles(&conn, &root).expect("reload"),
            renamed
        );
    }

    #[test]
    fn profile_metadata_and_legacy_backup_do_not_store_plaintext_fields() {
        let _guard = test_lock();
        let conn = migrated_conn(1);
        let (profile_uid, dedup_key, name): (String, String, String) = conn
            .query_row(
                "SELECT profile_uid, dedup_key, name FROM server_profiles LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("profile metadata");
        let metadata = format!("{profile_uid}\n{dedup_key}\n{name}");
        assert!(metadata.contains("hmac-sha256:"));
        assert!(!metadata.contains("Profile 0"));
        assert!(!metadata.contains("example.com"));
        assert!(!metadata.contains("user0"));

        let legacy_backup: String = conn
            .query_row(
                "SELECT value FROM global_state WHERE key = ?1",
                params![LEGACY_PROFILES_KEY],
                |row| row.get(0),
            )
            .expect("legacy backup");
        assert!(legacy_backup.contains("\"enc\":\"aes-256-gcm\""));
        assert!(!legacy_backup.contains("Profile 0"));
        assert!(!legacy_backup.contains("example.com"));
        assert!(!legacy_backup.contains("user0"));
    }

    // Deep scan item 46: a share's drive key goes to the account that started
    // the share, whichever account is active when publication completes.
    #[test]
    fn a_drive_key_filed_for_one_account_stays_with_it_while_another_is_active() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        let alice =
            create_passphrase_less_user(&mut conn, &root, "Alice", None, None).expect("alice");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", None, None).expect("bob");
        set_active_user(&conn, bob.id).expect("switch to bob");

        peer_drive_store_for(
            &conn,
            &root_secret,
            alice.id,
            "ns-1",
            "publisher",
            b"alice key",
        )
        .expect("filed for alice");

        let alice_key = peer_drive_load_for(&conn, &root_secret, alice.id, "ns-1").expect("read");
        assert_eq!(
            alice_key.as_deref().map(Vec::as_slice),
            Some(&b"alice key"[..])
        );
        assert!(peer_drive_load_for(&conn, &root_secret, bob.id, "ns-1")
            .expect("read")
            .is_none());
        let bob_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM peer_drives WHERE user_id = ?1",
                params![bob.id],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(bob_rows, 0);
    }

    #[test]
    fn a_drive_key_for_a_locked_account_is_refused_and_filed_nowhere() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        let alice = create_user(
            &mut conn,
            &root,
            "Alice",
            None,
            None,
            Some("correct horse battery staple"),
        )
        .expect("alice");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", None, None).expect("bob");
        // The switch to Bob locks Alice's partition.
        set_active_user(&conn, bob.id).expect("switch to bob");

        let stored = peer_drive_store_for(&conn, &root_secret, alice.id, "ns-1", "publisher", b"k");
        assert_eq!(stored.expect_err("alice is locked"), "USER_LOCKED");
        let loaded = peer_drive_load_for(&conn, &root_secret, alice.id, "ns-1");
        assert_eq!(loaded.expect_err("alice is locked"), "USER_LOCKED");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM peer_drives", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 0);
    }

    #[test]
    fn active_profile_writes_are_scoped_to_current_user() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let extra =
            create_passphrase_less_user(&mut conn, &root, "Ops", Some("O"), Some("#22c55e"))
                .expect("create extra");

        set_active_user(&conn, extra.id).expect("switch extra");
        let ops_profiles = vec![json!({
            "id": "ops-profile",
            "name": "Ops Profile",
            "protocol": "s3"
        })];
        replace_active_server_profiles(&mut conn, &root, &ops_profiles).expect("save extra");

        let default = list_users(&conn)
            .expect("users")
            .into_iter()
            .find(|user| user.name == DEFAULT_USER_NAME)
            .expect("default user");
        set_active_user(&conn, default.id).expect("switch default");
        let default_profiles = list_active_server_profiles(&conn, &root).expect("load default");
        assert_eq!(default_profiles.len(), 1);
        assert_eq!(default_profiles[0]["name"], "Profile 0");

        set_active_user(&conn, extra.id).expect("switch extra again");
        let reloaded_ops = list_active_server_profiles(&conn, &root).expect("load extra");
        assert_eq!(reloaded_ops, ops_profiles);
    }

    #[test]
    fn only_a_filesystem_without_locks_lets_a_writer_go_on_unlocked() {
        use std::io::{Error, ErrorKind};
        assert!(file_locks_unavailable(&Error::from(ErrorKind::Unsupported)));
        assert!(!file_locks_unavailable(&Error::from(
            ErrorKind::PermissionDenied
        )));
        #[cfg(unix)]
        assert!(file_locks_unavailable(&Error::from_raw_os_error(
            libc::ENOLCK
        )));
    }

    /// Every kind of list write waits for the lock, so a holder that writes secrets
    /// and list together (an import) never has another write land inside it.
    #[test]
    fn every_list_writer_waits_for_the_profile_list_lock() {
        let _guard = test_lock();
        clear_user_session();
        let dir = tempfile::tempdir().expect("tempdir");
        let mut conn = open_or_init_path(dir.path().join("partitions.db")).expect("open");
        let root = test_root();
        let seed = json!([{ "id": "p", "name": "P", "protocol": "sftp", "host": "h", "port": 22, "username": "u" }]);
        migrate_legacy_payloads(&mut conn, Some(&seed.to_string()), None, &root).expect("seed");
        let user = active_user_id(&conn).expect("active").expect("a user");
        let other =
            create_passphrase_less_user(&mut conn, &root, "Ops", Some("O"), Some("#22c55e"))
                .expect("create ops");
        let list = list_server_profiles_for(&conn, &root, user).expect("read");

        let held = ProfileListLock::acquire(&conn)
            .expect("lock")
            .expect("a file db");
        let busy = PROFILE_LIST_BUSY.to_string();
        assert_eq!(
            replace_server_profiles_for(&mut conn, &root, user, &list).err(),
            Some(busy.clone())
        );
        assert_eq!(
            replace_active_server_profiles(&mut conn, &root, &list).err(),
            Some(busy.clone())
        );
        assert_eq!(
            replace_expected_server_profiles(&mut conn, &root, user, None, &list).err(),
            Some(busy.clone())
        );
        assert_eq!(
            update_server_profiles_for(&mut conn, &root, user, |_| true).err(),
            Some(busy.clone())
        );
        assert_eq!(
            relocate_server_profile(&mut conn, &root, user, other.id, "p", "copy", None, false)
                .map(|_| ())
                .err(),
            Some(busy.clone())
        );
        assert_eq!(
            admin_reset_user_passphrase(&mut conn, &root, other.id, None).err(),
            Some(busy.clone())
        );
        assert_eq!(delete_user(&mut conn, other.id).err(), Some(busy.clone()));
        // A switch waits too, by both of its doors, and leaves the account as it was.
        assert_eq!(set_active_user(&conn, other.id).err(), Some(busy.clone()));
        assert_eq!(
            unlock_user(&conn, &root, other.id, None).map(|_| ()).err(),
            Some(busy)
        );
        assert_eq!(active_user_id(&conn).unwrap(), Some(user));
        drop(held);
        replace_server_profiles_for(&mut conn, &root, user, &list).expect("free again");
    }

    /// The same numeric id in a restored database can be another account: a session
    /// whose key does not open it does not count as unlocked.
    #[test]
    fn a_session_that_does_not_open_the_partition_leaves_the_account_locked() {
        let _guard = test_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("partitions.db");
        let mut conn = open_or_init_path(path.clone()).expect("open");
        let root = test_root();
        migrate_legacy_payloads(&mut conn, Some("[]"), None, &root).expect("seed");
        let protected =
            create_user(&mut conn, &root, "Protected", None, None, Some("pass")).expect("create");
        set_active_user(&conn, protected.id).expect("switch");
        assert!(active_user_locked_in_db(&path).unwrap());
        unlock_user(&conn, &root, protected.id, Some("pass")).expect("unlock");
        assert!(!active_user_locked_in_db(&path).unwrap());
        set_user_session(protected.id, user_crypto::secret_key_from_bytes(&[7u8; 32])).unwrap();
        assert!(active_user_locked_in_db(&path).unwrap());
        clear_user_session();
    }

    /// A list another writer saved after the read is refused, in both write paths,
    /// and the writer's rows stay; a fresh read carries a revision that passes.
    #[test]
    fn a_write_from_a_stale_read_is_refused_by_revision() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let user = active_user_id(&conn).expect("active").expect("a user");

        let (read, revision) = list_server_profiles_at_revision(&conn, &root, user).expect("read");
        let (reread, same) = list_server_profiles_at_revision(&conn, &root, user).expect("reread");
        assert_eq!(read, reread);
        assert_eq!(revision, same, "reading changes nothing");

        // Another writer saves the same list: new nonces, a new revision.
        replace_expected_server_profiles(&mut conn, &root, user, Some(&revision), &read)
            .expect("the first writer goes through");
        let mut edited = read.clone();
        edited[0]["name"] = json!("Edited");
        let refused =
            replace_expected_server_profiles(&mut conn, &root, user, Some(&revision), &edited)
                .expect_err("a save from the old read");
        assert_eq!(refused, PROFILES_CHANGED);
        let refused =
            update_server_profiles_at_revision(&mut conn, &root, user, &revision, |list| {
                list.clear()
            })
            .expect_err("an import planned on the old read");
        assert_eq!(refused, PROFILES_CHANGED);
        assert_eq!(
            list_active_server_profiles(&conn, &root).expect("load"),
            read,
            "the refused writes stored nothing"
        );

        let (_, fresh) = list_server_profiles_at_revision(&conn, &root, user).expect("fresh");
        assert_ne!(fresh, revision);
        let (sent, stored) =
            update_server_profiles_at_revision(&mut conn, &root, user, &fresh, |list| {
                list[0]["name"] = json!("Imported");
            })
            .expect("a fresh read goes through");
        assert_eq!(sent[0]["name"], "Imported");
        assert_eq!(stored[0]["name"], "Imported");
    }

    #[test]
    fn expected_user_write_is_refused_after_an_account_switch() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let ops = create_passphrase_less_user(&mut conn, &root, "Ops", Some("O"), Some("#22c55e"))
            .expect("create ops");
        let default = list_users(&conn)
            .expect("users")
            .into_iter()
            .find(|user| user.name == DEFAULT_USER_NAME)
            .expect("default user");

        // The edit form reads the default account's list...
        set_active_user(&conn, default.id).expect("switch default");
        let read_user = active_user_id(&conn).expect("active").expect("a user");
        let mut edited = list_active_server_profiles(&conn, &root).expect("read");
        assert_eq!(read_user, default.id);
        edited[0]["name"] = json!("Edited");

        // ...the account switches before its Save lands...
        set_active_user(&conn, ops.id).expect("switch ops");
        let refused = replace_expected_server_profiles(&mut conn, &root, read_user, None, &edited)
            .expect_err("a write for the previous account");
        assert_eq!(refused, ACCOUNT_CHANGED);

        // ...and neither account received the other's list.
        assert!(list_active_server_profiles(&conn, &root)
            .expect("load ops")
            .is_empty());
        set_active_user(&conn, default.id).expect("switch default again");
        let default_profiles = list_active_server_profiles(&conn, &root).expect("load default");
        assert_eq!(default_profiles[0]["name"], "Profile 0");

        // With the account still active, the bound write goes through.
        replace_expected_server_profiles(&mut conn, &root, read_user, None, &edited)
            .expect("write for the active account");
        assert_eq!(
            list_active_server_profiles(&conn, &root).expect("reload default")[0]["name"],
            "Edited"
        );
    }

    #[test]
    fn passphrase_user_unlocks_into_session_without_exposing_dek() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let bob = create_user(
            &mut conn,
            &root,
            "Bob",
            Some("B"),
            Some("#6366f1"),
            Some("correct horse battery staple"),
        )
        .expect("create locked user");
        assert!(bob.has_passphrase);

        let wrong = unlock_user(&conn, &root, bob.id, Some("wrong")).expect_err("wrong passphrase");
        assert_eq!(wrong, "WRONG_PASSPHRASE");

        let status = unlock_user(&conn, &root, bob.id, Some("correct horse battery staple"))
            .expect("unlock");
        assert_eq!(status.active_user_id, Some(bob.id));
        assert_eq!(status.unlocked_user_id, Some(bob.id));
        assert!(status.is_unlocked);

        let profiles = vec![json!({"id":"bob-profile","name":"Bob Profile","protocol":"sftp"})];
        replace_active_server_profiles(&mut conn, &root, &profiles).expect("write locked profile");
        assert_eq!(
            list_active_server_profiles(&conn, &root).expect("read locked profile"),
            profiles
        );

        clear_user_session();
        let locked = list_active_server_profiles(&conn, &root).expect_err("session locked");
        assert_eq!(locked, "USER_LOCKED");
    }

    #[test]
    fn transient_unlock_does_not_promote_user_to_active() {
        // MU-3 regression guard: CLI `--user <name>` must scope per-invocation
        // without persisting the switch. unlock_user_transient mirrors the
        // `aws --profile X` / `kubectl --context X` semantics.
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active read")
            .expect("default active");

        let alice = create_user(
            &mut conn,
            &root,
            "Alice",
            Some("A"),
            Some("#ec4899"),
            Some("alicepass1"),
        )
        .expect("create alice");
        // Creating alice did not switch active_user_id.
        assert_eq!(
            get_active_user(&conn).expect("active").map(|u| u.id),
            Some(default.id),
        );

        // Transient unlock primes the DEK session AND leaves active_user_id
        // alone. The `UserUnlockStatus` reporting reflects the active user
        // (used by the GUI lock screen); the authoritative check for the
        // per-invocation override is `active_user_id` + scoped reads below.
        let _status = unlock_user_transient(&conn, &root, alice.id, Some("alicepass1"))
            .expect("transient unlock");
        assert_eq!(
            active_user_id(&conn).expect("active id"),
            Some(default.id),
            "transient unlock must not change active_user_id"
        );

        // Per-user scoped writes target alice's partition even while
        // active_user_id still points at default.
        let profiles = vec![json!({"id":"alice-only","name":"Alice Only","protocol":"sftp"})];
        replace_server_profiles_for(&mut conn, &root, alice.id, &profiles).expect("scoped write");
        assert_eq!(
            list_server_profiles_for(&conn, &root, alice.id).expect("scoped read"),
            profiles
        );
        // Default's partition is untouched (no leak).
        assert!(list_server_profiles_for(&conn, &root, default.id)
            .expect("default read")
            .iter()
            .all(|p| p.get("id").and_then(|v| v.as_str()) != Some("alice-only")));

        // And after a transient session, active_user_id still points at default.
        assert_eq!(
            get_active_user(&conn).expect("active").map(|u| u.id),
            Some(default.id),
        );
    }

    #[test]
    fn change_passphrase_rewraps_dek_without_reencrypting_profile_blobs() {
        let _guard = test_lock();
        let conn = migrated_conn(1);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active")
            .expect("default active");
        let before_blob: Vec<u8> = conn
            .query_row(
                "SELECT encrypted_blob FROM server_profiles WHERE user_id = ?1 LIMIT 1",
                params![default.id],
                |row| row.get(0),
            )
            .expect("blob before");

        change_user_passphrase(&conn, &root, default.id, None, Some("new passphrase"))
            .expect("set passphrase");
        let no_pass = unlock_user(&conn, &root, default.id, None).expect_err("pass required");
        assert_eq!(no_pass, "PASSPHRASE_REQUIRED");
        unlock_user(&conn, &root, default.id, Some("new passphrase")).expect("unlock new");

        let after_set_blob: Vec<u8> = conn
            .query_row(
                "SELECT encrypted_blob FROM server_profiles WHERE user_id = ?1 LIMIT 1",
                params![default.id],
                |row| row.get(0),
            )
            .expect("blob after set");
        assert_eq!(before_blob, after_set_blob);

        change_user_passphrase(&conn, &root, default.id, Some("new passphrase"), None)
            .expect("remove passphrase");
        unlock_user(&conn, &root, default.id, None).expect("unlock passphrase-less");
        let after_remove_blob: Vec<u8> = conn
            .query_row(
                "SELECT encrypted_blob FROM server_profiles WHERE user_id = ?1 LIMIT 1",
                params![default.id],
                |row| row.get(0),
            )
            .expect("blob after remove");
        assert_eq!(before_blob, after_remove_blob);
    }

    #[test]
    fn transport_rekey_makes_passphraseless_partition_portable_cross_machine() {
        let _guard = test_lock();
        let conn = migrated_conn(2);
        let root_a = test_root();
        // A different machine's local root_key (HKDF of a different vault_key).
        let root_b = [0x5au8; 32];

        let default = get_active_user(&conn)
            .expect("active")
            .expect("default active");

        // Source machine reads its own profiles fine.
        let before = list_server_profiles_for(&conn, &root_a, default.id).expect("read on A");
        assert_eq!(before.len(), 2);

        // Export transport DEKs under the backup password.
        let transport = export_transport_deks(&conn, &root_a, "backup password 123")
            .expect("export transport")
            .expect("a passphrase-less user exists");
        assert!(transport.wrapped_deks.contains_key(&default.id));

        // Simulate the blind-overwrite import on machine B: identical rows,
        // different local root_key. Before re-keying, B cannot read them.
        assert!(list_server_profiles_for(&conn, &root_b, default.id).is_err());

        // Re-key to machine B's local root_key.
        let report = rekey_transport_deks(
            &conn,
            &root_b,
            "backup password 123",
            &transport.salt,
            &transport.kdf_params,
            &transport.wrapped_deks,
        )
        .expect("rekey");
        assert_eq!(report.rekeyed, 1);
        assert_eq!(report.unreadable, 0);
        assert_eq!(report.passphrase_protected, 0);

        // Machine B now reads the same profiles, byte-for-byte unchanged
        // (the DEK is the same, only its wrapping key changed).
        let after = list_server_profiles_for(&conn, &root_b, default.id).expect("read on B");
        assert_eq!(before, after);
    }

    #[test]
    fn rekey_reports_unreadable_when_no_transport_available() {
        let _guard = test_lock();
        let conn = migrated_conn(1);
        // Old-style backup: no transport section. A foreign machine cannot
        // open the passphrase-less partition, but nothing is destroyed.
        let root_b = [0x5au8; 32];
        let report = rekey_transport_deks(&conn, &root_b, "", &[], "", &HashMap::new())
            .expect("rekey detect-only");
        assert_eq!(report.rekeyed, 0);
        assert_eq!(report.unreadable, 1);
    }

    #[test]
    fn rekey_leaves_same_machine_partitions_untouched() {
        let _guard = test_lock();
        let conn = migrated_conn(1);
        let root_a = test_root();
        let transport = export_transport_deks(&conn, &root_a, "pw")
            .expect("export")
            .expect("some");
        // Re-import on the SAME machine: rows already unwrap with the local
        // root_key, so the re-key pass is a no-op.
        let report = rekey_transport_deks(
            &conn,
            &root_a,
            "pw",
            &transport.salt,
            &transport.kdf_params,
            &transport.wrapped_deks,
        )
        .expect("rekey");
        assert_eq!(report.already_local, 1);
        assert_eq!(report.rekeyed, 0);
        assert_eq!(report.unreadable, 0);
    }

    #[test]
    fn passphrase_protected_partitions_are_excluded_from_transport() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let bob = create_user(
            &mut conn,
            &root,
            "Bob",
            Some("B"),
            Some("#6366f1"),
            Some("correct horse battery staple"),
        )
        .expect("create passphrase user");
        let default = get_active_user(&conn)
            .expect("active")
            .expect("default active");

        let transport = export_transport_deks(&conn, &root, "pw")
            .expect("export")
            .expect("default is passphrase-less");
        // The passphrase-less default is carried; Bob (passphrase) is not:
        // his wrapped_dek is already machine-independent.
        assert!(transport.wrapped_deks.contains_key(&default.id));
        assert!(!transport.wrapped_deks.contains_key(&bob.id));

        // A same-machine re-key counts Bob as passphrase-protected and the
        // default as already-local (no re-keying needed).
        let report = rekey_transport_deks(
            &conn,
            &root,
            "pw",
            &transport.salt,
            &transport.kdf_params,
            &transport.wrapped_deks,
        )
        .expect("rekey");
        assert_eq!(report.passphrase_protected, 1);
        assert_eq!(report.already_local, 1);
        assert_eq!(report.rekeyed, 0);
    }

    #[test]
    fn wrong_passphrase_triggers_persistent_lockout() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let bob = create_user(
            &mut conn,
            &root,
            "Bob",
            Some("B"),
            Some("#6366f1"),
            Some("correct horse battery staple"),
        )
        .expect("create locked user");

        for _ in 0..LOCKOUT_THRESHOLD {
            let err = unlock_user(&conn, &root, bob.id, Some("wrong")).expect_err("wrong");
            assert_eq!(err, "WRONG_PASSPHRASE");
        }
        let state = read_lockout_state(&conn, bob.id).expect("lockout state");
        assert_eq!(state.fail_count, LOCKOUT_THRESHOLD);
        assert!(state.unlock_at_epoch_ms.unwrap_or_default() > now_ms());

        let locked = unlock_user(&conn, &root, bob.id, Some("correct horse battery staple"))
            .expect_err("locked out");
        assert!(locked.starts_with("LOCKED_OUT:"));
    }

    #[test]
    fn delete_user_cascades_profiles_and_moves_active_user() {
        let _guard = test_lock();
        let mut conn = migrated_conn(1);
        let root = test_root();
        let extra =
            create_passphrase_less_user(&mut conn, &root, "Ops", Some("O"), Some("#22c55e"))
                .expect("create extra");
        set_active_user(&conn, extra.id).expect("set active");

        let profiles = vec![json!({"id":"ops-profile","name":"Ops Profile","protocol":"s3"})];
        replace_active_server_profiles(&mut conn, &root, &profiles).expect("insert extra profile");

        delete_user(&mut conn, extra.id).expect("delete extra");
        let remaining_extra_profiles: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM server_profiles WHERE user_id = ?1",
                params![extra.id],
                |row| row.get(0),
            )
            .expect("profile count");
        assert_eq!(remaining_extra_profiles, 0);
        let active = get_active_user(&conn)
            .expect("active")
            .expect("active user");
        assert_eq!(active.name, DEFAULT_USER_NAME);
    }

    #[test]
    fn deleting_non_active_user_keeps_current_active_user() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let extra =
            create_passphrase_less_user(&mut conn, &root, "Ops", Some("O"), Some("#22c55e"))
                .expect("create extra");

        let active_before = get_active_user(&conn)
            .expect("active before")
            .expect("active user before");
        assert_eq!(active_before.name, DEFAULT_USER_NAME);

        delete_user(&mut conn, extra.id).expect("delete non-active");
        let active_after = get_active_user(&conn)
            .expect("active after")
            .expect("active user after");
        assert_eq!(active_after.id, active_before.id);
    }

    // ============ N4 cross-user profile relocation ============

    fn sftp_profile(id: &str) -> Value {
        json!({
            "id": id,
            "name": "My SFTP",
            "protocol": "sftp",
            "host": "example.com",
            "port": 22,
            "username": "alice"
        })
    }

    /// S3 preset with empty access-key in the blob — storage `dedup_key` hashes
    /// the empty key to a constant and false-collides distinct accounts (EF-19).
    fn s3_empty_user_profile(id: &str, name: &str) -> Value {
        json!({
            "id": id,
            "name": name,
            "protocol": "s3",
            "providerId": "wasabi",
            "host": "s3.wasabisys.com",
            "port": 443,
            "username": ""
        })
    }

    /// Preset OAuth (pCloud) with no email materialised in the blob.
    fn pcloud_oauth_empty_user_profile(id: &str, name: &str) -> Value {
        json!({
            "id": id,
            "name": name,
            "protocol": "pcloud",
            "providerId": "pcloud",
            "host": "",
            "port": 0,
            "username": ""
        })
    }

    /// Preset WebDAV with empty / shared placeholder username.
    fn webdav_preset_empty_user_profile(id: &str, name: &str) -> Value {
        json!({
            "id": id,
            "name": name,
            "protocol": "webdav",
            "providerId": "nextcloud",
            "host": "cloud.example.com",
            "port": 443,
            "username": ""
        })
    }

    fn seed_server_secret(
        conn: &Connection,
        root: &[u8; 32],
        user_id: i64,
        profile_id: &str,
        secret: &str,
    ) {
        set_user_credential_for(
            conn,
            root,
            user_id,
            &format!("server_{profile_id}"),
            "server",
            secret,
        )
        .expect("seed server secret");
    }

    fn seed_oauth_secret(
        conn: &Connection,
        root: &[u8; 32],
        user_id: i64,
        profile_id: &str,
        secret: &str,
    ) {
        set_user_credential_for(
            conn,
            root,
            user_id,
            &format!("oauth_pcloud_{profile_id}"),
            "oauth",
            secret,
        )
        .expect("seed oauth secret");
    }

    fn seed_jottacloud_secret(
        conn: &Connection,
        root: &[u8; 32],
        user_id: i64,
        profile_id: &str,
        secret: &str,
    ) {
        set_user_credential_for(
            conn,
            root,
            user_id,
            &format!("jottacloud_refresh_{profile_id}"),
            "jottacloud_refresh",
            secret,
        )
        .expect("seed jottacloud secret");
    }

    fn jottacloud_profile(id: &str, name: &str) -> Value {
        json!({
            "id": id,
            "name": name,
            "protocol": "jottacloud",
            "providerId": "jottacloud",
            "host": "",
            "port": 0,
            "username": "jotta-user@example.com"
        })
    }

    /// Partition-only credential dual (no vault store) — unit-test seam for F4.
    fn relocate_credentials_partition_only(
        conn: &Connection,
        root: &[u8; 32],
        source_user_id: i64,
        relocation: &ProfileRelocation,
        target_passphrase: Option<&str>,
    ) {
        let source_keys =
            relocate_all_secret_key_candidates(&relocation.protocol, &relocation.source_profile_id);
        let new_keys =
            relocate_all_secret_key_candidates(&relocation.protocol, &relocation.new_profile_id);
        for (source_key, new_key) in source_keys.iter().zip(new_keys.iter()) {
            relocate_secret_key_dual(
                conn,
                None,
                root,
                source_user_id,
                relocation,
                target_passphrase,
                source_key,
                new_key,
                relocate_secret_kind(source_key),
            );
        }
    }

    fn seed_crypt_overlay_secrets(
        conn: &Connection,
        root: &[u8; 32],
        user_id: i64,
        profile_id: &str,
    ) {
        set_user_credential_for(
            conn,
            root,
            user_id,
            &format!("aerocrypt_overlay_pw_{profile_id}"),
            "aerocrypt_overlay_pw",
            "overlay-password",
        )
        .expect("seed aerocrypt pw");
        set_user_credential_for(
            conn,
            root,
            user_id,
            &format!("aerocrypt_overlay_salt_{profile_id}"),
            "aerocrypt_overlay_salt",
            "overlay-salt",
        )
        .expect("seed aerocrypt salt");
        set_user_credential_for(
            conn,
            root,
            user_id,
            &format!("aerocrypt_overlay_config_{profile_id}"),
            "aerocrypt_overlay_config",
            r#"{"v":3}"#,
        )
        .expect("seed aerocrypt config");
    }

    fn assert_cred_present(
        conn: &Connection,
        root: &[u8; 32],
        user_id: i64,
        key: &str,
        expected: &str,
    ) {
        let got = get_user_credential_for(conn, root, user_id, key)
            .expect("read cred")
            .unwrap_or_else(|| panic!("expected credential {key} for user {user_id}"));
        assert_eq!(got.as_str(), expected, "credential {key}");
    }

    fn assert_cred_absent(conn: &Connection, root: &[u8; 32], user_id: i64, key: &str) {
        let got = get_user_credential_for(conn, root, user_id, key).expect("read cred");
        assert!(
            got.is_none(),
            "credential {key} must be absent for user {user_id}, got {:?}",
            got.as_ref().map(|s| s.as_str())
        );
    }

    #[test]
    fn relocate_copy_into_passphraseless_target_keeps_source() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        replace_active_server_profiles(&mut conn, &root, &[sftp_profile("srv_src")])
            .expect("seed source profile");

        let report = relocate_server_profile(
            &mut conn, &root, default.id, bob.id, "srv_src", "srv_new", None,
            /*remove_from_source=*/ false,
        )
        .expect("copy");
        assert!(!report.moved);
        assert!(!report.already_present);
        assert!(report.inserted);
        assert_eq!(report.new_profile_id, "srv_new");
        assert_eq!(report.profile_name, "My SFTP");

        // Source untouched.
        let source = list_server_profiles_for(&conn, &root, default.id).expect("source list");
        assert_eq!(source.len(), 1);
        assert_eq!(source[0]["id"], "srv_src");

        // Target received an independent copy under the new id.
        let target = list_server_profiles_for(&conn, &root, bob.id).expect("target list");
        assert_eq!(target.len(), 1);
        assert_eq!(target[0]["id"], "srv_new");
        assert_eq!(target[0]["host"], "example.com");
        assert!(target[0].get("lastConnected").is_none());
    }

    #[test]
    fn relocate_move_removes_source_row() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        replace_active_server_profiles(&mut conn, &root, &[sftp_profile("srv_src")])
            .expect("seed source profile");

        let report = relocate_server_profile(
            &mut conn, &root, default.id, bob.id, "srv_src", "srv_new", None,
            /*remove_from_source=*/ true,
        )
        .expect("move");
        assert!(report.moved);
        assert!(!report.already_present);
        assert!(report.inserted);

        let source = list_server_profiles_for(&conn, &root, default.id).expect("source list");
        assert!(
            source.is_empty(),
            "source profile must be gone after a move"
        );
        let target = list_server_profiles_for(&conn, &root, bob.id).expect("target list");
        assert_eq!(target.len(), 1);
        assert_eq!(target[0]["id"], "srv_new");
    }

    /// The target already holds a profile of the source's name (another
    /// account, so the Copy is not skipped): that profile keeps the name, the
    /// copy is stored as "My SFTP (1)", and the report names the copy by the
    /// name that selects it there, for a Copy and for a Move.
    #[test]
    fn relocate_reports_the_name_the_target_stored() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        let mut bobs = sftp_profile("srv_bob");
        bobs["host"] = json!("other.example.com");
        replace_server_profiles_for(&mut conn, &root, bob.id, &[bobs]).expect("seed bob");

        for (source_id, new_id, remove_from_source, stored_as) in [
            ("srv_src", "srv_copy", false, "My SFTP (1)"),
            ("srv_src", "srv_moved", true, "My SFTP (2)"),
        ] {
            replace_active_server_profiles(&mut conn, &root, &[sftp_profile(source_id)])
                .expect("seed source profile");
            let report = relocate_server_profile(
                &mut conn,
                &root,
                default.id,
                bob.id,
                source_id,
                new_id,
                None,
                remove_from_source,
            )
            .expect("relocate");
            assert!(report.inserted);
            assert_eq!(report.profile_name, stored_as);
            let target = list_server_profiles_for(&conn, &root, bob.id).expect("target list");
            assert_eq!(
                crate::util::profile_name::name_of(&target, new_id),
                Some(stored_as)
            );
            assert_eq!(
                crate::util::profile_name::name_of(&target, "srv_bob"),
                Some("My SFTP"),
                "the target's own profile keeps its name"
            );
        }
    }

    #[test]
    fn relocate_into_protected_target_requires_passphrase() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_user(
            &mut conn,
            &root,
            "Bob",
            Some("B"),
            Some("#6366f1"),
            Some("correct horse battery staple"),
        )
        .expect("create protected bob");
        replace_active_server_profiles(&mut conn, &root, &[sftp_profile("srv_src")])
            .expect("seed source profile");

        // No passphrase -> rejected.
        let err = relocate_server_profile(
            &mut conn, &root, default.id, bob.id, "srv_src", "srv_new", None, false,
        )
        .expect_err("must require passphrase");
        assert_eq!(err, "TARGET_PASSPHRASE_REQUIRED");

        // Correct passphrase -> writes into the protected partition.
        relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_src",
            "srv_new",
            Some("correct horse battery staple"),
            false,
        )
        .expect("copy with passphrase");

        // Prime the target session to read it back.
        unlock_user_transient(&conn, &root, bob.id, Some("correct horse battery staple"))
            .expect("unlock target");
        let target = list_server_profiles_for(&conn, &root, bob.id).expect("target list");
        assert_eq!(target.len(), 1);
        assert_eq!(target[0]["id"], "srv_new");
        clear_user_session();
    }

    #[test]
    fn relocate_rejects_same_user() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        replace_active_server_profiles(&mut conn, &root, &[sftp_profile("srv_src")]).expect("seed");
        let err = relocate_server_profile(
            &mut conn, &root, default.id, default.id, "srv_src", "srv_new", None, false,
        )
        .expect_err("same user");
        assert_eq!(err, "RELOCATE_SAME_USER");
    }

    #[test]
    fn relocate_copy_skips_when_target_already_has_server() {
        // Strong-surface true positive (SFTP host+user) still skips a second Copy.
        // EF-19: weak-surface S3 / pCloud OAuth / preset WebDAV with empty blob
        // usernames and *different* credentials must NOT report "already saved"
        // and must materialise the Copy. Same credentials after a first copy
        // still skip (real identity match).
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");

        // --- SFTP: strong surface, no secrets needed ---
        replace_active_server_profiles(&mut conn, &root, &[sftp_profile("srv_sftp")])
            .expect("seed sftp");
        relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_sftp",
            "srv_sftp_bob",
            None,
            false,
        )
        .expect("first sftp copy");
        let sftp_second = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_sftp",
            "srv_sftp_dup",
            None,
            false,
        )
        .expect("second sftp copy");
        assert!(sftp_second.already_present, "same SFTP account must skip");
        assert!(!sftp_second.inserted, "a Copy identity hit must not insert");

        // --- S3 empty username: distinct secrets must materialise ---
        replace_active_server_profiles(
            &mut conn,
            &root,
            &[
                sftp_profile("srv_sftp"),
                s3_empty_user_profile("srv_s3_a", "Wasabi A"),
            ],
        )
        .expect("seed s3 a on source");
        seed_server_secret(&conn, &root, default.id, "srv_s3_a", "s3-secret-account-a");
        // Bob already has a *different* S3 account (same empty blob surface).
        // Under storage dedup_key both hash to s3:wasabi:<empty-hash> and would
        // false-positive; the identity probe must not.
        replace_server_profiles_for(
            &mut conn,
            &root,
            bob.id,
            &[
                sftp_profile("srv_sftp_bob"),
                s3_empty_user_profile("srv_s3_bob", "Wasabi B"),
            ],
        )
        .expect("seed s3 b on bob");
        seed_server_secret(&conn, &root, bob.id, "srv_s3_bob", "s3-secret-account-b");

        let s3_copy = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_s3_a",
            "srv_s3_copied",
            None,
            false,
        )
        .expect("s3 cross-user copy");
        assert!(
            !s3_copy.already_present,
            "distinct S3 accounts must not false-positive as already saved"
        );
        assert!(s3_copy.inserted, "S3 cross-user Copy must materialise");
        // Mirror the credential the production dual-write would copy, then a
        // second Copy of the same account should skip.
        seed_server_secret(&conn, &root, bob.id, "srv_s3_copied", "s3-secret-account-a");
        let s3_second = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_s3_a",
            "srv_s3_again",
            None,
            false,
        )
        .expect("s3 second copy same secret");
        assert!(
            s3_second.already_present && !s3_second.inserted,
            "same S3 credential fingerprint must skip a second Copy"
        );

        // --- pCloud OAuth empty username: distinct tokens must materialise ---
        replace_active_server_profiles(
            &mut conn,
            &root,
            &[
                sftp_profile("srv_sftp"),
                s3_empty_user_profile("srv_s3_a", "Wasabi A"),
                pcloud_oauth_empty_user_profile("srv_pc_a", "pCloud A"),
            ],
        )
        .expect("seed pcloud a");
        seed_oauth_secret(
            &conn,
            &root,
            default.id,
            "srv_pc_a",
            r#"{"access_token":"tok-a"}"#,
        );
        let bob_profiles = list_server_profiles_for(&conn, &root, bob.id).expect("bob list");
        let mut bob_seed = bob_profiles;
        bob_seed.push(pcloud_oauth_empty_user_profile("srv_pc_bob", "pCloud B"));
        replace_server_profiles_for(&mut conn, &root, bob.id, &bob_seed).expect("seed pcloud b");
        seed_oauth_secret(
            &conn,
            &root,
            bob.id,
            "srv_pc_bob",
            r#"{"access_token":"tok-b"}"#,
        );

        let pc_copy = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_pc_a",
            "srv_pc_copied",
            None,
            false,
        )
        .expect("pcloud cross-user copy");
        assert!(
            !pc_copy.already_present,
            "distinct pCloud OAuth accounts must not false-positive"
        );
        assert!(
            pc_copy.inserted,
            "pCloud OAuth cross-user Copy must materialise"
        );

        // --- preset WebDAV empty username: distinct secrets must materialise ---
        replace_active_server_profiles(
            &mut conn,
            &root,
            &[
                sftp_profile("srv_sftp"),
                webdav_preset_empty_user_profile("srv_dav_a", "Nextcloud A"),
            ],
        )
        .expect("seed webdav a");
        seed_server_secret(&conn, &root, default.id, "srv_dav_a", "dav-password-a");
        let bob_profiles = list_server_profiles_for(&conn, &root, bob.id).expect("bob list");
        let mut bob_seed = bob_profiles;
        bob_seed.push(webdav_preset_empty_user_profile(
            "srv_dav_bob",
            "Nextcloud B",
        ));
        replace_server_profiles_for(&mut conn, &root, bob.id, &bob_seed).expect("seed webdav b");
        seed_server_secret(&conn, &root, bob.id, "srv_dav_bob", "dav-password-b");

        let dav_copy = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_dav_a",
            "srv_dav_copied",
            None,
            false,
        )
        .expect("webdav cross-user copy");
        assert!(
            !dav_copy.already_present,
            "distinct WebDAV preset accounts must not false-positive"
        );
        assert!(
            dav_copy.inserted,
            "WebDAV preset cross-user Copy must materialise"
        );

        let target = list_server_profiles_for(&conn, &root, bob.id).expect("final target");
        assert!(
            target.iter().any(|p| p["id"] == "srv_s3_copied"),
            "S3 copy landed"
        );
        assert!(
            target.iter().any(|p| p["id"] == "srv_pc_copied"),
            "pCloud copy landed"
        );
        assert!(
            target.iter().any(|p| p["id"] == "srv_dav_copied"),
            "WebDAV copy landed"
        );
        assert!(
            !target.iter().any(|p| p["id"] == "srv_sftp_dup"),
            "SFTP true-positive must not insert a second row"
        );
        assert!(
            !target.iter().any(|p| p["id"] == "srv_s3_again"),
            "S3 same-credential true-positive must not insert again"
        );
    }

    #[test]
    fn relocate_move_materialises_even_when_target_has_equivalent() {
        // #366 regression: a Move must never delete the source unless the moved
        // profile is materialised in the target. The previous code skipped the
        // insert on a dedup hit yet still deleted the source, so the only copy
        // was lost. The probe still reports `already_present`, but the move must
        // insert first and only then drop the source.
        //
        // EF-19: also cover weak-surface S3 / OAuth / WebDAV — a Move of a
        // *distinct* account (different credential fingerprint) must report
        // already_present=false yet still materialise; a Move onto a real
        // equivalent still materialises with already_present=true.
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        replace_active_server_profiles(&mut conn, &root, &[sftp_profile("srv_src")])
            .expect("seed source profile");
        // Bob already holds an equivalent server (same host/user, different id).
        relocate_server_profile(
            &mut conn, &root, default.id, bob.id, "srv_src", "srv_dup", None, false,
        )
        .expect("seed equivalent in target");

        let report = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_src",
            "srv_moved",
            None,
            /*remove_from_source=*/ true,
        )
        .expect("move with equivalent present");
        assert!(report.moved);
        assert!(
            report.already_present,
            "an equivalent existed in the target"
        );
        assert!(
            report.inserted,
            "a move must materialise the profile in the target"
        );

        // Source removed (it is a move) and the moved profile now lives in the
        // target: no data loss even though the identity probe matched.
        let source = list_server_profiles_for(&conn, &root, default.id).expect("source list");
        assert!(source.is_empty(), "source removed after move");
        let target = list_server_profiles_for(&conn, &root, bob.id).expect("target list");
        assert!(
            target.iter().any(|p| p["id"] == "srv_moved"),
            "moved profile must exist in the target"
        );

        // --- EF-19 weak-surface Move: distinct S3 account still materialises ---
        replace_active_server_profiles(
            &mut conn,
            &root,
            &[s3_empty_user_profile("srv_s3_move", "Wasabi Move")],
        )
        .expect("seed s3 move source");
        seed_server_secret(
            &conn,
            &root,
            default.id,
            "srv_s3_move",
            "s3-move-secret-src",
        );
        // Target already has another empty-username Wasabi (would collide under
        // storage dedup_key) with a different secret.
        let mut bob_now = list_server_profiles_for(&conn, &root, bob.id).expect("bob now");
        bob_now.push(s3_empty_user_profile("srv_s3_other", "Wasabi Other"));
        replace_server_profiles_for(&mut conn, &root, bob.id, &bob_now).expect("seed other s3");
        seed_server_secret(&conn, &root, bob.id, "srv_s3_other", "s3-move-secret-other");

        let s3_move = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_s3_move",
            "srv_s3_moved",
            None,
            /*remove_from_source=*/ true,
        )
        .expect("s3 move distinct account");
        assert!(s3_move.moved);
        assert!(
            !s3_move.already_present,
            "distinct S3 credentials must not look already present"
        );
        assert!(
            s3_move.inserted,
            "Move must materialise before source delete"
        );
        let source = list_server_profiles_for(&conn, &root, default.id).expect("source after s3");
        assert!(source.is_empty(), "s3 source removed after move");
        let target = list_server_profiles_for(&conn, &root, bob.id).expect("target after s3");
        assert!(
            target.iter().any(|p| p["id"] == "srv_s3_moved"),
            "moved S3 profile must exist in the target"
        );

        // --- EF-19 weak-surface Move onto real equivalent (same fingerprint) ---
        replace_active_server_profiles(
            &mut conn,
            &root,
            &[pcloud_oauth_empty_user_profile(
                "srv_pc_move",
                "pCloud Move",
            )],
        )
        .expect("seed pcloud move source");
        seed_oauth_secret(
            &conn,
            &root,
            default.id,
            "srv_pc_move",
            r#"{"access_token":"tok-same"}"#,
        );
        // First copy lands the account in Bob; credential dual-write is manual
        // in unit tests (relocate_server_credential_dual needs a store).
        let pc_seed = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_pc_move",
            "srv_pc_seed",
            None,
            false,
        )
        .expect("seed pcloud in bob");
        assert!(pc_seed.inserted);
        seed_oauth_secret(
            &conn,
            &root,
            bob.id,
            "srv_pc_seed",
            r#"{"access_token":"tok-same"}"#,
        );

        let pc_move = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_pc_move",
            "srv_pc_moved",
            None,
            /*remove_from_source=*/ true,
        )
        .expect("pcloud move with equivalent present");
        assert!(pc_move.moved);
        assert!(
            pc_move.already_present,
            "same OAuth fingerprint is a real equivalent"
        );
        assert!(
            pc_move.inserted,
            "#366: Move still materialises on identity hit"
        );
        let source = list_server_profiles_for(&conn, &root, default.id).expect("source after pc");
        assert!(source.is_empty(), "pcloud source removed after move");
        let target = list_server_profiles_for(&conn, &root, bob.id).expect("target after pc");
        assert!(
            target.iter().any(|p| p["id"] == "srv_pc_moved"),
            "moved pCloud profile must exist in the target"
        );
    }

    // ============ F4: OAuth / Jottacloud credential dual (#366) ============

    #[test]
    fn relocate_credential_dual_pcloud_copy_keeps_source_oauth() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        replace_active_server_profiles(
            &mut conn,
            &root,
            &[pcloud_oauth_empty_user_profile("srv_pc_c", "pCloud Copy")],
        )
        .expect("seed pcloud");
        let token = r#"{"access_token":"tok-copy-a"}"#;
        seed_oauth_secret(&conn, &root, default.id, "srv_pc_c", token);
        seed_server_secret(&conn, &root, default.id, "srv_pc_c", "server-secret-a");

        let report = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_pc_c",
            "srv_pc_c_new",
            None,
            /*remove_from_source=*/ false,
        )
        .expect("pcloud copy");
        assert!(report.inserted);
        assert!(!report.moved);
        assert_eq!(report.protocol, "pcloud");

        relocate_credentials_partition_only(&conn, &root, default.id, &report, None);

        // Target has both secrets under the new id.
        assert_cred_present(&conn, &root, bob.id, "oauth_pcloud_srv_pc_c_new", token);
        assert_cred_present(
            &conn,
            &root,
            bob.id,
            "server_srv_pc_c_new",
            "server-secret-a",
        );
        // Copy must NEVER delete the source keys (#366 gating).
        assert_cred_present(&conn, &root, default.id, "oauth_pcloud_srv_pc_c", token);
        assert_cred_present(
            &conn,
            &root,
            default.id,
            "server_srv_pc_c",
            "server-secret-a",
        );
    }

    #[test]
    fn relocate_credential_dual_pcloud_move_drops_source_oauth() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        replace_active_server_profiles(
            &mut conn,
            &root,
            &[pcloud_oauth_empty_user_profile("srv_pc_m", "pCloud Move")],
        )
        .expect("seed pcloud");
        let token = r#"{"access_token":"tok-move-a"}"#;
        seed_oauth_secret(&conn, &root, default.id, "srv_pc_m", token);
        seed_server_secret(&conn, &root, default.id, "srv_pc_m", "server-secret-m");

        let report = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_pc_m",
            "srv_pc_m_new",
            None,
            /*remove_from_source=*/ true,
        )
        .expect("pcloud move");
        assert!(report.inserted);
        assert!(report.moved);
        assert_eq!(report.protocol, "pcloud");

        relocate_credentials_partition_only(&conn, &root, default.id, &report, None);

        assert_cred_present(&conn, &root, bob.id, "oauth_pcloud_srv_pc_m_new", token);
        assert_cred_present(
            &conn,
            &root,
            bob.id,
            "server_srv_pc_m_new",
            "server-secret-m",
        );
        assert_cred_absent(&conn, &root, default.id, "oauth_pcloud_srv_pc_m");
        assert_cred_absent(&conn, &root, default.id, "server_srv_pc_m");
    }

    #[test]
    fn relocate_credential_dual_jottacloud_copy_keeps_source_refresh() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        replace_active_server_profiles(
            &mut conn,
            &root,
            &[jottacloud_profile("srv_jt_c", "Jotta Copy")],
        )
        .expect("seed jottacloud");
        let refresh = r#"{"refresh_token":"jrt-copy-a"}"#;
        seed_jottacloud_secret(&conn, &root, default.id, "srv_jt_c", refresh);
        seed_server_secret(&conn, &root, default.id, "srv_jt_c", "server-jt-c");

        let report = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_jt_c",
            "srv_jt_c_new",
            None,
            /*remove_from_source=*/ false,
        )
        .expect("jottacloud copy");
        assert!(report.inserted);
        assert!(!report.moved);
        assert_eq!(report.protocol, "jottacloud");

        relocate_credentials_partition_only(&conn, &root, default.id, &report, None);

        assert_cred_present(
            &conn,
            &root,
            bob.id,
            "jottacloud_refresh_srv_jt_c_new",
            refresh,
        );
        assert_cred_present(
            &conn,
            &root,
            default.id,
            "jottacloud_refresh_srv_jt_c",
            refresh,
        );
        assert_cred_present(&conn, &root, default.id, "server_srv_jt_c", "server-jt-c");
    }

    #[test]
    fn relocate_credential_dual_jottacloud_move_drops_source_refresh() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        replace_active_server_profiles(
            &mut conn,
            &root,
            &[jottacloud_profile("srv_jt_m", "Jotta Move")],
        )
        .expect("seed jottacloud");
        let refresh = r#"{"refresh_token":"jrt-move-a"}"#;
        seed_jottacloud_secret(&conn, &root, default.id, "srv_jt_m", refresh);
        seed_server_secret(&conn, &root, default.id, "srv_jt_m", "server-jt-m");

        let report = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_jt_m",
            "srv_jt_m_new",
            None,
            /*remove_from_source=*/ true,
        )
        .expect("jottacloud move");
        assert!(report.inserted);
        assert!(report.moved);
        assert_eq!(report.protocol, "jottacloud");

        relocate_credentials_partition_only(&conn, &root, default.id, &report, None);

        assert_cred_present(
            &conn,
            &root,
            bob.id,
            "jottacloud_refresh_srv_jt_m_new",
            refresh,
        );
        assert_cred_absent(&conn, &root, default.id, "jottacloud_refresh_srv_jt_m");
        assert_cred_absent(&conn, &root, default.id, "server_srv_jt_m");
    }

    #[test]
    fn relocate_credential_dual_password_protocol_only_server_keys() {
        // sftp: identity resolver returns no oauth base → only server_* is
        // identifying. Dual still walks the full per_profile_vault_keys set but
        // only copies keys that exist, so no phantom oauth_* rows appear.
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        replace_active_server_profiles(&mut conn, &root, &[sftp_profile("srv_pw")])
            .expect("seed sftp");
        seed_server_secret(&conn, &root, default.id, "srv_pw", "sftp-password");

        let report = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_pw",
            "srv_pw_new",
            None,
            /*remove_from_source=*/ true,
        )
        .expect("sftp move");
        assert!(report.inserted);
        assert!(report.moved);
        assert_eq!(report.protocol, "sftp");
        assert_eq!(
            relocate_credential_key_candidates(&report.protocol, &report.source_profile_id),
            vec!["server_srv_pw".to_string()],
            "password protocol must not invent oauth keys"
        );

        relocate_credentials_partition_only(&conn, &root, default.id, &report, None);

        assert_cred_present(&conn, &root, bob.id, "server_srv_pw_new", "sftp-password");
        assert_cred_absent(&conn, &root, default.id, "server_srv_pw");
        // No phantom OAuth/Jottacloud keys on either side.
        assert_cred_absent(&conn, &root, bob.id, "oauth_pcloud_srv_pw_new");
        assert_cred_absent(&conn, &root, bob.id, "jottacloud_refresh_srv_pw_new");
        assert_cred_absent(&conn, &root, default.id, "oauth_pcloud_srv_pw");
        assert_cred_absent(&conn, &root, default.id, "jottacloud_refresh_srv_pw");
    }

    #[test]
    fn relocate_credential_dual_copy_carries_crypt_overlay_secrets() {
        // #366 follow-up: Copy to another user must dual-write Crypt overlay
        // secrets onto the new profile id so badges stay honest and decrypt works.
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        replace_active_server_profiles(&mut conn, &root, &[sftp_profile("srv_crypt")])
            .expect("seed sftp+crypt");
        seed_server_secret(&conn, &root, default.id, "srv_crypt", "sftp-password");
        seed_crypt_overlay_secrets(&conn, &root, default.id, "srv_crypt");

        let report = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_crypt",
            "srv_crypt_new",
            None,
            /*remove_from_source=*/ false,
        )
        .expect("crypt copy");
        assert!(report.inserted);
        assert!(!report.moved);

        relocate_credentials_partition_only(&conn, &root, default.id, &report, None);

        // Target has the full crypt bundle under the new id.
        assert_cred_present(
            &conn,
            &root,
            bob.id,
            "server_srv_crypt_new",
            "sftp-password",
        );
        assert_cred_present(
            &conn,
            &root,
            bob.id,
            "aerocrypt_overlay_pw_srv_crypt_new",
            "overlay-password",
        );
        assert_cred_present(
            &conn,
            &root,
            bob.id,
            "aerocrypt_overlay_salt_srv_crypt_new",
            "overlay-salt",
        );
        assert_cred_present(
            &conn,
            &root,
            bob.id,
            "aerocrypt_overlay_config_srv_crypt_new",
            r#"{"v":3}"#,
        );
        // Copy keeps source secrets intact.
        assert_cred_present(
            &conn,
            &root,
            default.id,
            "aerocrypt_overlay_pw_srv_crypt",
            "overlay-password",
        );
        assert_cred_present(
            &conn,
            &root,
            default.id,
            "server_srv_crypt",
            "sftp-password",
        );
    }

    #[test]
    fn relocate_credential_dual_move_drops_source_crypt_overlay_secrets() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        replace_active_server_profiles(&mut conn, &root, &[sftp_profile("srv_crypt_m")])
            .expect("seed sftp+crypt");
        seed_server_secret(&conn, &root, default.id, "srv_crypt_m", "sftp-password");
        seed_crypt_overlay_secrets(&conn, &root, default.id, "srv_crypt_m");

        let report = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "srv_crypt_m",
            "srv_crypt_m_new",
            None,
            /*remove_from_source=*/ true,
        )
        .expect("crypt move");
        assert!(report.inserted);
        assert!(report.moved);

        relocate_credentials_partition_only(&conn, &root, default.id, &report, None);

        assert_cred_present(
            &conn,
            &root,
            bob.id,
            "aerocrypt_overlay_pw_srv_crypt_m_new",
            "overlay-password",
        );
        assert_cred_present(
            &conn,
            &root,
            bob.id,
            "aerocrypt_overlay_config_srv_crypt_m_new",
            r#"{"v":3}"#,
        );
        assert_cred_absent(&conn, &root, default.id, "aerocrypt_overlay_pw_srv_crypt_m");
        assert_cred_absent(
            &conn,
            &root,
            default.id,
            "aerocrypt_overlay_salt_srv_crypt_m",
        );
        assert_cred_absent(
            &conn,
            &root,
            default.id,
            "aerocrypt_overlay_config_srv_crypt_m",
        );
        assert_cred_absent(&conn, &root, default.id, "server_srv_crypt_m");
    }

    #[test]
    fn relocate_identity_surface_distinguishes_weak_blob_accounts() {
        // Pure helper coverage: empty-username S3/OAuth/WebDAV surfaces match
        // each other within a protocol, so the credential fingerprint is what
        // must break the tie (not storage dedup_key).
        let a = s3_empty_user_profile("a", "A");
        let b = s3_empty_user_profile("b", "B");
        assert_eq!(relocate_identity_surface(&a), relocate_identity_surface(&b));
        assert!(!relocate_surface_has_account_id(&a));
        assert!(relocate_accounts_match(&a, "fp1", &b, "fp1"));
        assert!(!relocate_accounts_match(&a, "fp1", &b, "fp2"));
        assert!(
            !relocate_accounts_match(&a, "", &b, ""),
            "weak surface + empty fingerprints must not match"
        );

        let sftp_a = sftp_profile("x");
        let sftp_b = sftp_profile("y");
        assert!(relocate_surface_has_account_id(&sftp_a));
        assert!(
            relocate_accounts_match(&sftp_a, "", &sftp_b, ""),
            "strong surface may match without secrets"
        );
    }

    #[test]
    fn relocate_unknown_profile_errors() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        let err = relocate_server_profile(
            &mut conn,
            &root,
            default.id,
            bob.id,
            "does_not_exist",
            "srv_new",
            None,
            false,
        )
        .expect_err("missing profile");
        assert!(err.starts_with("PROFILE_NOT_FOUND"));
    }

    #[test]
    fn reorder_users_updates_dropdown_order_without_touching_active_user() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let ops = create_passphrase_less_user(&mut conn, &root, "Ops", Some("O"), Some("#22c55e"))
            .expect("create ops");
        let qa = create_passphrase_less_user(&mut conn, &root, "QA", Some("Q"), Some("#f59e0b"))
            .expect("create qa");
        let default = get_active_user(&conn)
            .expect("active")
            .expect("default active");

        reorder_users(&mut conn, &[qa.id, default.id, ops.id]).expect("reorder");
        let users = list_users(&conn).expect("users after reorder");
        assert_eq!(
            users.iter().map(|user| user.id).collect::<Vec<_>>(),
            vec![qa.id, default.id, ops.id]
        );
        assert!(users
            .iter()
            .any(|user| user.id == default.id && user.is_active));

        let duplicate = reorder_users(&mut conn, &[qa.id, qa.id]).expect_err("duplicate");
        assert_eq!(duplicate, "DUPLICATE_USER_ID");
    }

    #[test]
    fn user_storage_stats_count_encrypted_profiles_and_settings() {
        let _guard = test_lock();
        let mut conn = migrated_conn(2);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active")
            .expect("default active");
        let ops = create_passphrase_less_user(&mut conn, &root, "Ops", Some("O"), Some("#22c55e"))
            .expect("create ops");
        set_active_user(&conn, ops.id).expect("switch ops");
        replace_active_server_profiles(
            &mut conn,
            &root,
            &[json!({"id":"ops-profile","name":"Ops Profile","protocol":"s3"})],
        )
        .expect("save ops profile");

        let stats = user_storage_stats(&conn).expect("stats");
        let default_stats = stats
            .iter()
            .find(|item| item.user_id == default.id)
            .expect("default stats");
        let ops_stats = stats
            .iter()
            .find(|item| item.user_id == ops.id)
            .expect("ops stats");
        assert_eq!(default_stats.profile_count, 2);
        assert_eq!(default_stats.settings_count, 1);
        assert!(default_stats.encrypted_bytes > 0);
        assert_eq!(ops_stats.profile_count, 1);
        assert!(ops_stats.encrypted_bytes > 0);
    }

    // ============ MU-4 user_settings CRUD ============

    #[test]
    fn user_settings_round_trip_active_user() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root = test_root();
        let value = json!({ "enabled": true, "interval_secs": 3600 });
        set_active_user_setting(&conn, &root, "aerosync_schedule", &value).expect("set scheduled");
        let read =
            get_active_user_setting(&conn, &root, "aerosync_schedule").expect("get scheduled");
        assert_eq!(read, Some(value));
    }

    /// Scopes stored for one user, read straight from the table.
    fn setting_scopes_of(conn: &Connection, user_id: i64) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT scope FROM user_settings WHERE user_id = ?1 ORDER BY scope ASC")
            .expect("prepare scopes");
        stmt.query_map(params![user_id], |row| row.get::<_, String>(0))
            .expect("query scopes")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("read scopes")
    }

    #[test]
    fn user_settings_missing_returns_none() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root = test_root();
        let read = get_active_user_setting(&conn, &root, "never_written").expect("get missing");
        assert!(read.is_none());
    }

    #[test]
    fn user_settings_upsert_overwrites_existing_scope() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root = test_root();
        set_active_user_setting(&conn, &root, "aerosync_schedule", &json!({"v": 1}))
            .expect("set v1");
        set_active_user_setting(&conn, &root, "aerosync_schedule", &json!({"v": 2}))
            .expect("set v2");
        let read = get_active_user_setting(&conn, &root, "aerosync_schedule")
            .expect("read")
            .expect("present");
        assert_eq!(read, json!({"v": 2}));
        let active = active_user_id(&conn).expect("active").expect("default");
        let scopes = setting_scopes_of(&conn, active);
        assert_eq!(
            scopes.iter().filter(|s| s == &"aerosync_schedule").count(),
            1,
            "duplicate scope rows must not accumulate",
        );
    }

    #[test]
    fn user_settings_delete_removes_row() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root = test_root();
        set_active_user_setting(&conn, &root, "aerosync_schedule", &json!(42)).expect("set");
        delete_active_user_setting(&conn, "aerosync_schedule").expect("delete");
        let read =
            get_active_user_setting(&conn, &root, "aerosync_schedule").expect("get after delete");
        assert!(read.is_none());
    }

    #[test]
    fn user_settings_are_partitioned_per_user() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active default")
            .expect("default");
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#3b82f6"))
                .expect("create alice");

        // Write on default.
        set_active_user_setting(&conn, &root, "aerosync_schedule", &json!("default-value"))
            .expect("set on default");

        // Switch to alice, write own value, default value remains untouched.
        set_active_user(&conn, alice.id).expect("switch alice");
        set_active_user_setting(&conn, &root, "aerosync_schedule", &json!("alice-value"))
            .expect("set on alice");
        let alice_read = get_active_user_setting(&conn, &root, "aerosync_schedule")
            .expect("alice read")
            .expect("alice value");
        assert_eq!(alice_read, json!("alice-value"));

        // Switch back: default's value is intact (R3 partition integrity for
        // settings, not just profiles).
        set_active_user(&conn, default.id).expect("switch default");
        let default_read = get_active_user_setting(&conn, &root, "aerosync_schedule")
            .expect("default read")
            .expect("default value");
        assert_eq!(default_read, json!("default-value"));

        // The rows themselves are scoped too: alice owns only her scope, with
        // no legacy backup row (she is a fresh user).
        assert_eq!(
            setting_scopes_of(&conn, alice.id),
            vec!["aerosync_schedule".to_string()]
        );
    }

    #[test]
    fn user_settings_empty_scope_is_rejected() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root = test_root();
        let err = set_active_user_setting(&conn, &root, "", &json!(true)).expect_err("empty scope");
        assert_eq!(err, "USER_SETTING_SCOPE_REQUIRED");
    }

    // ============ MU-7 cross-user dedup ============

    #[test]
    fn cross_user_dedup_finds_same_profile_in_other_partition() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#3b82f6"))
                .expect("alice");

        let profile = json!({
            "id": "shared-srv",
            "name": "Shared",
            "protocol": "sftp",
            "host": "host.example.com",
            "port": 22,
            "username": "shared",
        });

        // default writes the profile.
        replace_active_server_profiles(&mut conn, &root, std::slice::from_ref(&profile))
            .expect("save on default");

        // Alice is active; she queries against default's dedup_key.
        set_active_user(&conn, alice.id).expect("switch alice");
        let matches = cross_user_dedup_matches(&conn, &root, alice.id, &profile).expect("query");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].user_id, default.id);
        assert_eq!(matches[0].user_name, DEFAULT_USER_NAME);
    }

    #[test]
    fn cross_user_dedup_excludes_requesting_user() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let mut conn = conn;
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");

        let profile = json!({
            "id": "self-srv",
            "protocol": "ftp",
            "host": "self.example.com",
            "port": 21,
            "username": "self",
        });

        replace_active_server_profiles(&mut conn, &root, std::slice::from_ref(&profile))
            .expect("save on default");

        // Querying as the same user must NOT include itself (otherwise R11
        // intra-user dedup would over-trigger a cross-user warning popup).
        let matches = cross_user_dedup_matches(&conn, &root, default.id, &profile).expect("query");
        assert!(matches.is_empty());
    }

    #[test]
    fn cross_user_dedup_returns_empty_when_no_match() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#3b82f6"))
                .expect("alice");

        let unique = json!({
            "id": "unique-srv",
            "protocol": "webdav",
            "host": "nobody.example.com",
            "username": "only",
        });

        let matches = cross_user_dedup_matches(&conn, &root, alice.id, &unique).expect("query");
        assert!(matches.is_empty());
    }

    #[test]
    fn cross_user_dedup_orders_matches_by_sort_order() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#3b82f6"))
                .expect("alice");
        let bob = create_passphrase_less_user(&mut conn, &root, "bob", Some("B"), Some("#22c55e"))
            .expect("bob");

        let profile = json!({
            "id": "shared",
            "protocol": "s3",
            "host": "s3.example.com",
            "username": "k",
            "port": 443,
        });

        replace_active_server_profiles(&mut conn, &root, std::slice::from_ref(&profile))
            .expect("save on default");
        set_active_user(&conn, alice.id).expect("switch alice");
        replace_active_server_profiles(&mut conn, &root, std::slice::from_ref(&profile))
            .expect("save on alice");
        set_active_user(&conn, bob.id).expect("switch bob");

        // From bob's perspective, both default and alice match. Order follows
        // sort_order (default=0, alice=1).
        let matches = cross_user_dedup_matches(&conn, &root, bob.id, &profile).expect("query");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].user_id, default.id);
        assert_eq!(matches[1].user_id, alice.id);
    }

    #[test]
    fn cross_user_dedup_empty_key_s3_does_not_cross_warn() {
        // EF-19(a): empty S3 access keys share a constant storage dedup_key.
        // The soft-warning path must not fire across distinct keyless accounts.
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#3b82f6"))
                .expect("alice");

        let stored = json!({
            "id": "s3-empty-a",
            "name": "S3 Empty A",
            "protocol": "s3",
            "providerId": "wasabi",
            "host": "s3.wasabisys.com",
            "port": 443,
            "username": "",
        });
        replace_active_server_profiles(&mut conn, &root, std::slice::from_ref(&stored))
            .expect("save empty-key S3 on default");

        // Candidate for alice: also empty-key S3 (different id; could even share host).
        let candidate = json!({
            "id": "s3-empty-b",
            "name": "S3 Empty B",
            "protocol": "s3",
            "providerId": "wasabi",
            "host": "s3.wasabisys.com",
            "port": 443,
            "username": "   ",
        });
        set_active_user(&conn, alice.id).expect("switch alice");
        let matches = cross_user_dedup_matches(&conn, &root, alice.id, &candidate).expect("query");
        assert!(
            matches.is_empty(),
            "empty-key S3 must not cross-warn against user {}; got {:?}",
            default.id,
            matches
        );
    }

    #[test]
    fn cross_user_dedup_genuine_s3_dup_still_warns() {
        // Non-empty access key: same key across users must still soft-warn.
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#3b82f6"))
                .expect("alice");

        let profile = json!({
            "id": "s3-real",
            "name": "S3 Real",
            "protocol": "s3",
            "providerId": "wasabi",
            "host": "s3.wasabisys.com",
            "port": 443,
            "username": "AKIA_REAL_KEY_ONE",
        });
        replace_active_server_profiles(&mut conn, &root, std::slice::from_ref(&profile))
            .expect("save real S3 on default");

        set_active_user(&conn, alice.id).expect("switch alice");
        let matches = cross_user_dedup_matches(&conn, &root, alice.id, &profile).expect("query");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].user_id, default.id);
        assert_eq!(matches[0].user_name, DEFAULT_USER_NAME);
    }

    // ----- MU-FE-P4a: admin role + self-or-admin gate -----

    #[test]
    fn migration_seeds_default_user_as_admin() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let users = list_users(&conn).expect("list users");
        assert_eq!(users.len(), 1);
        assert!(
            users[0].is_admin,
            "the legacy default user must be promoted to admin so the install retains full control",
        );
    }

    #[test]
    fn newly_created_users_are_not_admin() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#10b981"))
                .expect("alice");
        assert!(
            !alice.is_admin,
            "Manage Users add flow must not auto-promote new accounts"
        );
    }

    #[test]
    fn set_user_avatar_updates_public_avatar_fields() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#10b981"))
                .expect("alice");

        set_user_avatar(
            &conn,
            alice.id,
            Some("data:image/svg+xml;base64,PHN2Zy8+"),
            Some("#ef4444"),
        )
        .expect("set avatar");

        let updated = list_users(&conn)
            .expect("list users")
            .into_iter()
            .find(|user| user.id == alice.id)
            .expect("alice row");
        assert_eq!(
            updated.avatar_emoji.as_deref(),
            Some("data:image/svg+xml;base64,PHN2Zy8+")
        );
        assert_eq!(updated.avatar_color.as_deref(), Some("#ef4444"));
    }

    #[test]
    fn set_user_avatar_validates_color_and_size() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#10b981"))
                .expect("alice");

        assert_eq!(
            set_user_avatar(&conn, alice.id, Some("A"), Some("red")),
            Err("INVALID_AVATAR_COLOR".to_string())
        );
        let too_large = "x".repeat(128 * 1024 + 1);
        assert_eq!(
            set_user_avatar(&conn, alice.id, Some(&too_large), Some("#10b981")),
            Err("AVATAR_TOO_LARGE".to_string())
        );
    }

    #[test]
    fn ensure_user_can_modify_allows_self_and_admin_on_peer() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#10b981"))
                .expect("alice");

        // default (admin) acting on self -> self-mode (true)
        assert_eq!(ensure_user_can_modify(&conn, default.id), Ok(true));
        // default (admin) acting on alice -> admin-mode (false)
        assert_eq!(ensure_user_can_modify(&conn, alice.id), Ok(false));
    }

    #[test]
    fn ensure_user_can_modify_blocks_non_admin_peer() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#10b981"))
                .expect("alice");
        // Switch active user to alice (passphrase-less so unlock is implicit).
        set_active_user(&conn, alice.id).expect("switch alice");

        // alice -> alice: self OK
        assert_eq!(ensure_user_can_modify(&conn, alice.id), Ok(true));
        // alice -> default: NOT_AUTHORIZED (alice is not admin)
        assert_eq!(
            ensure_user_can_modify(&conn, default.id),
            Err("NOT_AUTHORIZED".to_string())
        );
    }

    #[test]
    fn set_user_admin_promotes_and_demotes_with_guard() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#10b981"))
                .expect("alice");

        // default (admin) promotes alice
        set_user_admin(&mut conn, alice.id, true).expect("promote alice");
        assert!(is_admin_user(&conn, alice.id).expect("alice is_admin"));

        // default demotes alice back
        set_user_admin(&mut conn, alice.id, false).expect("demote alice");
        assert!(!is_admin_user(&conn, alice.id).expect("alice is_admin"));

        // alice (non-admin) cannot promote herself
        set_active_user(&conn, alice.id).expect("switch alice");
        assert_eq!(
            set_user_admin(&mut conn, alice.id, true),
            Err("NOT_AUTHORIZED".to_string())
        );

        // default cannot revoke itself when it is the last admin
        set_active_user(&conn, default.id).expect("switch back default");
        assert_eq!(
            set_user_admin(&mut conn, default.id, false),
            Err("CANNOT_DEMOTE_LAST_ADMIN".to_string())
        );
    }

    /// The default account opens without a prompt: a passphrase added to it,
    /// by its own change or an admin reset, clears the flag. A change that
    /// leaves an account without a passphrase, and one of another account,
    /// leave it.
    #[test]
    fn a_passphrase_clears_the_default_flag() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let admin = get_active_user(&conn).unwrap().unwrap().id;
        let alice = create_passphrase_less_user(&mut conn, &root, "alice", None, None).unwrap();
        let carol = create_passphrase_less_user(&mut conn, &root, "carol", None, None).unwrap();
        let is_default = |conn: &Connection, id: i64| {
            list_users(conn)
                .unwrap()
                .into_iter()
                .find(|u| u.id == id)
                .unwrap()
                .is_default
        };
        set_default_user(&mut conn, alice.id, true).unwrap();
        change_user_passphrase(&conn, &root, alice.id, None, None).unwrap();
        change_user_passphrase(&conn, &root, carol.id, None, Some("carol's")).unwrap();
        assert!(
            is_default(&conn, alice.id),
            "a change without a passphrase cleared it"
        );
        change_user_passphrase(&conn, &root, alice.id, None, Some("alice's")).unwrap();
        assert!(
            !is_default(&conn, alice.id),
            "a protected account stayed the default"
        );

        change_user_passphrase(&conn, &root, alice.id, Some("alice's"), None).unwrap();
        set_default_user(&mut conn, alice.id, true).unwrap();
        set_active_user(&conn, admin).unwrap();
        admin_reset_user_passphrase(&mut conn, &root, alice.id, Some("recovered")).unwrap();
        assert!(
            !is_default(&conn, alice.id),
            "an admin reset left a protected default"
        );
    }

    /// An admin reset drops the account's credential rows with its DEK: the
    /// new DEK cannot read them, and a row kept would still count, by its
    /// session index entry, as a copy of a Proton session another account
    /// signs out of. An intact account holding the same session still counts.
    #[cfg(feature = "proton-native")]
    #[test]
    fn an_admin_reset_leaves_no_session_copy_in_the_index() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let admin = get_active_user(&conn).unwrap().unwrap().id;
        let alice = create_passphrase_less_user(&mut conn, &root, "alice", None, None)
            .unwrap()
            .id;
        let bob = create_passphrase_less_user(&mut conn, &root, "bob", None, None)
            .unwrap()
            .id;
        clear_user_session();
        let session = json!({"uid": "shared-uid", "access_token": "a", "refresh_token": "r",
                             "key_password": "k"})
        .to_string();
        let key = "proton_native_session_p";
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        for user in [alice, bob] {
            set_user_credential_for(&conn, &root, user, key, PROTON_SESSION_TYPE, &session)
                .unwrap();
            with_user_dek(&conn, &root_secret, user, |id, dek| {
                index_session_row(&conn, &root, id, dek, key)
            })
            .unwrap();
        }
        assert_eq!(
            other_accounts_holding_session_in(&conn, &root, Some(alice), "shared-uid").unwrap(),
            OtherAccounts::HoldIt
        );
        set_active_user(&conn, admin).unwrap();
        admin_reset_user_passphrase(&mut conn, &root, bob, None).unwrap();
        assert_eq!(
            other_accounts_holding_session_in(&conn, &root, Some(alice), "shared-uid").unwrap(),
            OtherAccounts::HoldNone,
            "the reset account's unreadable copy"
        );
        assert_eq!(
            other_accounts_holding_session_in(&conn, &root, Some(bob), "shared-uid").unwrap(),
            OtherAccounts::HoldIt,
            "the intact account"
        );
    }

    #[test]
    fn set_default_user_is_single_winner_and_passphrase_gated() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#10b981"))
                .expect("alice");
        let bob = create_user(
            &mut conn,
            &root,
            "bob",
            Some("B"),
            Some("#6366f1"),
            Some("correct horse battery staple"),
        )
        .expect("bob");

        let is_default = |conn: &Connection, id: i64| -> bool {
            list_users(conn)
                .expect("list")
                .into_iter()
                .find(|u| u.id == id)
                .expect("user")
                .is_default
        };

        // No user is default after migration.
        assert!(!is_default(&conn, default.id));
        assert!(!is_default(&conn, alice.id));

        // Mark the migration default.
        set_default_user(&mut conn, default.id, true).expect("set default");
        assert!(is_default(&conn, default.id));

        // Single-winner: marking alice clears the previous default.
        set_default_user(&mut conn, alice.id, true).expect("set alice default");
        assert!(is_default(&conn, alice.id));
        assert!(!is_default(&conn, default.id));
        assert_eq!(
            list_users(&conn)
                .expect("list")
                .iter()
                .filter(|u| u.is_default)
                .count(),
            1,
            "at most one default at a time"
        );

        // Clearing alice leaves no default.
        set_default_user(&mut conn, alice.id, false).expect("clear alice");
        assert!(!is_default(&conn, alice.id));

        // A passphrase-protected account cannot auto-unlock, so it cannot be default.
        assert_eq!(
            set_default_user(&mut conn, bob.id, true),
            Err("DEFAULT_REQUIRES_NO_PASSPHRASE".to_string())
        );

        // Unknown id is rejected.
        assert_eq!(
            set_default_user(&mut conn, 99_999, true),
            Err("USER_NOT_FOUND".to_string())
        );
    }

    #[test]
    fn delete_user_protects_last_admin() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let _alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#10b981"))
                .expect("alice");
        // alice is NOT admin; deleting the only admin (default) must
        // be rejected even though two users exist.
        assert_eq!(
            delete_user(&mut conn, default.id),
            Err("CANNOT_DELETE_LAST_ADMIN".to_string())
        );
    }

    #[test]
    fn admin_reset_wipes_target_partition_and_issues_new_dek() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        // Create alice with a passphrase + a saved profile so we can
        // verify the destructive wipe.
        let alice = create_user(
            &mut conn,
            &root,
            "alice",
            Some("A"),
            Some("#10b981"),
            Some("alice-pw"),
        )
        .expect("alice");
        set_active_user(&conn, alice.id).expect("switch alice");
        unlock_user(&conn, &root, alice.id, Some("alice-pw")).expect("unlock alice");
        let profile = json!({
            "id": "p1",
            "name": "alice profile",
            "protocol": "sftp",
            "host": "s.example.com",
            "username": "a",
            "port": 22,
        });
        replace_active_server_profiles(&mut conn, &root, &[profile]).expect("save alice profile");
        let count_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM server_profiles WHERE user_id = ?1",
                params![alice.id],
                |row| row.get(0),
            )
            .expect("count before");
        assert_eq!(count_before, 1);

        // Switch active back to default (admin) and perform the reset.
        set_active_user(&conn, get_active_user(&conn).unwrap().unwrap().id).expect("noop");
        // The migrated default is implicitly the only admin; ensure
        // it is the active user before invoking the admin path.
        let default_id = list_users(&conn)
            .expect("list")
            .into_iter()
            .find(|u| u.is_admin)
            .expect("admin")
            .id;
        set_active_user(&conn, default_id).expect("switch default");

        admin_reset_user_passphrase(&mut conn, &root, alice.id, Some("recovered"))
            .expect("admin reset");

        // alice's encrypted partition is wiped.
        let count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM server_profiles WHERE user_id = ?1",
                params![alice.id],
                |row| row.get(0),
            )
            .expect("count after");
        assert_eq!(count_after, 0);

        // The new passphrase unlocks the new DEK (old one is gone for good).
        set_active_user(&conn, alice.id).expect("switch alice");
        unlock_user(&conn, &root, alice.id, Some("recovered")).expect("unlock with new passphrase");
        // And the OLD passphrase no longer works.
        assert!(unlock_user(&conn, &root, alice.id, Some("alice-pw")).is_err());
    }

    #[test]
    fn admin_reset_rejects_self_target() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default_id = get_active_user(&conn).expect("active").expect("default").id;
        assert_eq!(
            admin_reset_user_passphrase(&mut conn, &root, default_id, Some("x")),
            Err("ADMIN_RESET_NOT_FOR_SELF".to_string()),
        );
    }

    #[test]
    fn admin_reset_blocked_for_non_admin_caller() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let alice =
            create_passphrase_less_user(&mut conn, &root, "alice", Some("A"), Some("#10b981"))
                .expect("alice");
        let bob = create_passphrase_less_user(&mut conn, &root, "bob", Some("B"), Some("#3b82f6"))
            .expect("bob");
        // Switch active to alice (non-admin). alice tries to reset bob.
        set_active_user(&conn, alice.id).expect("switch alice");
        assert_eq!(
            admin_reset_user_passphrase(&mut conn, &root, bob.id, Some("x")),
            Err("NOT_AUTHORIZED".to_string())
        );
    }

    #[test]
    fn upgrade_v2_to_v3_promotes_lowest_id_user_to_admin() {
        let _guard = test_lock();
        // Build a v2-shaped database in-memory: schema without is_admin
        // + schema_version=2 + two users seeded directly.
        let mut conn = Connection::open_in_memory().expect("memory db");
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE users (
                 id                INTEGER PRIMARY KEY AUTOINCREMENT,
                 name              TEXT NOT NULL,
                 name_canonical    TEXT NOT NULL UNIQUE,
                 avatar_emoji      TEXT,
                 avatar_color      TEXT,
                 has_passphrase    INTEGER NOT NULL DEFAULT 0,
                 kdf_salt          BLOB,
                 kdf_params        TEXT,
                 wrapped_dek       BLOB NOT NULL,
                 dek_verifier      BLOB NOT NULL,
                 sort_order        INTEGER NOT NULL DEFAULT 0,
                 created_at        INTEGER NOT NULL,
                 updated_at        INTEGER NOT NULL,
                 last_unlocked_at  INTEGER
             );
             CREATE TABLE server_profiles (
                 id              INTEGER PRIMARY KEY AUTOINCREMENT,
                 user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                 profile_uid     TEXT NOT NULL,
                 dedup_key       TEXT NOT NULL,
                 name            TEXT NOT NULL,
                 encrypted_blob  BLOB NOT NULL,
                 nonce           BLOB NOT NULL,
                 aead_alg        TEXT NOT NULL DEFAULT 'aes-256-gcm',
                 created_at      INTEGER NOT NULL,
                 updated_at      INTEGER NOT NULL,
                 UNIQUE(user_id, profile_uid)
             );
             CREATE TABLE user_settings (
                 user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                 scope           TEXT NOT NULL,
                 encrypted_blob  BLOB NOT NULL,
                 nonce           BLOB NOT NULL,
                 aead_alg        TEXT NOT NULL DEFAULT 'aes-256-gcm',
                 updated_at      INTEGER NOT NULL,
                 PRIMARY KEY(user_id, scope)
             );
             CREATE TABLE global_state (
                 key             TEXT PRIMARY KEY,
                 value           TEXT NOT NULL,
                 updated_at      INTEGER NOT NULL
             );",
        )
        .expect("v2 schema");
        // Two users; lowest id (1) is the legacy default.
        let now = now_ms();
        conn.execute(
            "INSERT INTO users(name, name_canonical, wrapped_dek, dek_verifier, sort_order, created_at, updated_at)
             VALUES ('default', 'default', X'00', X'00', 0, ?1, ?1)",
            params![now],
        )
        .expect("seed default");
        conn.execute(
            "INSERT INTO users(name, name_canonical, wrapped_dek, dek_verifier, sort_order, created_at, updated_at)
             VALUES ('alice', 'alice', X'00', X'00', 1, ?1, ?1)",
            params![now],
        )
        .expect("seed alice");
        conn.execute(
            "INSERT INTO global_state(key, value, updated_at) VALUES (?1, '2', ?2)",
            params![SCHEMA_VERSION_KEY, now],
        )
        .expect("seed schema v2");

        upgrade_v2_to_v3(&mut conn).expect("upgrade");

        let is_admin_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users WHERE is_admin = 1", [], |row| {
                row.get(0)
            })
            .expect("count admin");
        assert_eq!(is_admin_count, 1);
        let admin_id: i64 = conn
            .query_row("SELECT id FROM users WHERE is_admin = 1", [], |row| {
                row.get(0)
            })
            .expect("admin id");
        assert_eq!(admin_id, 1);
        // upgrade_v2_to_v3 lands exactly on "3"; the v3 -> v4 step is a
        // separate stage in the cascade (`apply_pending_upgrades`).
        let version = current_schema_version(&conn).expect("version");
        assert_eq!(version.as_deref(), Some("3"));

        // Idempotent: rerun must not crash and must not flip admin off.
        upgrade_v2_to_v3(&mut conn).expect("idempotent rerun");
        let still_admin: i64 = conn
            .query_row("SELECT is_admin FROM users WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("admin still");
        assert_eq!(still_admin, 1);
    }

    // --- MUV-1: user_credentials ------------------------------------------

    #[test]
    fn credential_round_trip_device_wrapped() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active read")
            .expect("default active");

        set_user_credential_for(&conn, &root, default.id, "server_1", "server", "pw")
            .expect("set credential");
        let got = get_user_credential_for(&conn, &root, default.id, "server_1")
            .expect("get credential")
            .expect("credential present");
        assert_eq!(got.as_str(), "pw");

        // Missing credential reads back as None, not an error.
        assert!(get_user_credential_for(&conn, &root, default.id, "absent")
            .expect("get absent")
            .is_none());
    }

    #[test]
    fn credential_round_trip_passphrase_requires_session() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let bob = create_user(
            &mut conn,
            &root,
            "Bob",
            Some("B"),
            Some("#6366f1"),
            Some("correct horse battery staple"),
        )
        .expect("create locked user");
        assert!(bob.has_passphrase);

        // No session yet: a passphrase account is locked.
        let locked = set_user_credential_for(&conn, &root, bob.id, "server_2", "server", "secret")
            .expect_err("locked write");
        assert_eq!(locked, "USER_LOCKED");

        // Unlock primes the DEK session; set/get now succeed.
        unlock_user(&conn, &root, bob.id, Some("correct horse battery staple")).expect("unlock");
        set_user_credential_for(&conn, &root, bob.id, "server_2", "server", "secret")
            .expect("set after unlock");
        let got = get_user_credential_for(&conn, &root, bob.id, "server_2")
            .expect("get after unlock")
            .expect("present");
        assert_eq!(got.as_str(), "secret");

        // Dropping the session re-locks the partition.
        clear_user_session();
        let relocked =
            get_user_credential_for(&conn, &root, bob.id, "server_2").expect_err("relocked read");
        assert_eq!(relocked, "USER_LOCKED");
    }

    #[test]
    fn credential_isolation_across_users() {
        // R3 for secrets: a credential set by user A is invisible to user B
        // because the primary key includes user_id.
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let user_a = get_active_user(&conn)
            .expect("active read")
            .expect("default active");
        let user_b =
            create_passphrase_less_user(&mut conn, &root, "userb", Some("B"), Some("#10b981"))
                .expect("create userb");

        set_user_credential_for(&conn, &root, user_a.id, "server_1", "server", "a-secret")
            .expect("set on A");
        assert!(get_user_credential_for(&conn, &root, user_b.id, "server_1")
            .expect("read on B")
            .is_none());
        // A still reads its own.
        assert_eq!(
            get_user_credential_for(&conn, &root, user_a.id, "server_1")
                .expect("read on A")
                .expect("present")
                .as_str(),
            "a-secret"
        );
    }

    #[test]
    fn credential_upsert_keeps_single_row() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active read")
            .expect("default active");

        set_user_credential_for(&conn, &root, default.id, "oauth_dropbox_1", "oauth", "v1")
            .expect("first write");
        set_user_credential_for(&conn, &root, default.id, "oauth_dropbox_1", "oauth", "v2")
            .expect("second write");

        let got = get_user_credential_for(&conn, &root, default.id, "oauth_dropbox_1")
            .expect("get")
            .expect("present");
        assert_eq!(got.as_str(), "v2");

        let ids = list_user_credential_ids_for(&conn, default.id).expect("list ids");
        assert_eq!(
            ids,
            vec![("oauth_dropbox_1".to_string(), "oauth".to_string())]
        );
    }

    #[test]
    fn credential_cascade_on_user_delete() {
        // R9 for secrets: deleting a user removes its credentials (FK cascade).
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let userb =
            create_passphrase_less_user(&mut conn, &root, "userb", Some("B"), Some("#10b981"))
                .expect("create userb");

        set_user_credential_for(&conn, &root, userb.id, "server_9", "server", "doomed")
            .expect("set credential");
        assert_eq!(
            list_user_credential_ids_for(&conn, userb.id)
                .expect("list before")
                .len(),
            1
        );

        delete_user(&mut conn, userb.id).expect("delete user");
        assert!(list_user_credential_ids_for(&conn, userb.id)
            .expect("list after")
            .is_empty());
    }

    #[test]
    fn upgrade_v3_to_v4_creates_user_credentials_idempotently() {
        let _guard = test_lock();
        // Build a v3-shaped database: the v2 schema plus the is_admin column,
        // and crucially WITHOUT user_credentials, so the upgrade has to create
        // it (not init_db_schema).
        let mut conn = Connection::open_in_memory().expect("memory db");
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE users (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 name TEXT NOT NULL,
                 name_canonical TEXT NOT NULL UNIQUE,
                 wrapped_dek BLOB NOT NULL,
                 dek_verifier BLOB NOT NULL,
                 sort_order INTEGER NOT NULL DEFAULT 0,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL,
                 is_admin INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE global_state (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL,
                 updated_at INTEGER NOT NULL
             );",
        )
        .expect("v3 schema");
        let now = now_ms();
        conn.execute(
            "INSERT INTO global_state(key, value, updated_at) VALUES (?1, '3', ?2)",
            params![SCHEMA_VERSION_KEY, now],
        )
        .expect("seed schema v3");

        upgrade_v3_to_v4(&mut conn).expect("upgrade v3->v4");

        let table_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                 WHERE type='table' AND name='user_credentials')",
                [],
                |row| row.get(0),
            )
            .expect("check table");
        assert!(table_exists);
        assert_eq!(
            current_schema_version(&conn).expect("version").as_deref(),
            Some("4")
        );

        // Rerun is a no-op, no error.
        upgrade_v3_to_v4(&mut conn).expect("idempotent rerun");
        assert_eq!(
            current_schema_version(&conn).expect("version").as_deref(),
            Some("4")
        );
    }

    #[test]
    fn apply_pending_upgrades_chains_v2_to_v5() {
        let _guard = test_lock();
        // A v2 database (no is_admin, no user_credentials) must reach the current
        // schema in a single startup: v2 -> v3 -> v4 -> v5 (v5 = AeroShare peer
        // secret-store tables, version-stamp only).
        let mut conn = Connection::open_in_memory().expect("memory db");
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE users (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 name TEXT NOT NULL,
                 name_canonical TEXT NOT NULL UNIQUE,
                 wrapped_dek BLOB NOT NULL,
                 dek_verifier BLOB NOT NULL,
                 sort_order INTEGER NOT NULL DEFAULT 0,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             CREATE TABLE global_state (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL,
                 updated_at INTEGER NOT NULL
             );",
        )
        .expect("v2 schema");
        let now = now_ms();
        conn.execute(
            "INSERT INTO users(name, name_canonical, wrapped_dek, dek_verifier, created_at, updated_at)
             VALUES ('default', 'default', X'00', X'00', ?1, ?1)",
            params![now],
        )
        .expect("seed default");
        conn.execute(
            "INSERT INTO global_state(key, value, updated_at) VALUES (?1, '2', ?2)",
            params![SCHEMA_VERSION_KEY, now],
        )
        .expect("seed schema v2");

        let current = apply_pending_upgrades(&mut conn).expect("apply upgrades");
        assert!(current, "v2 must chain to current schema");
        assert_eq!(
            current_schema_version(&conn).expect("version").as_deref(),
            Some(SCHEMA_VERSION)
        );

        // Both intermediate effects landed: is_admin seed (v3) and the
        // user_credentials table (v4).
        let admin_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users WHERE is_admin = 1", [], |row| {
                row.get(0)
            })
            .expect("admin count");
        assert_eq!(admin_count, 1);
        let table_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                 WHERE type='table' AND name='user_credentials')",
                [],
                |row| row.get(0),
            )
            .expect("check table");
        assert!(table_exists);

        // Idempotent: a second pass is a clean no-op.
        assert!(apply_pending_upgrades(&mut conn).expect("rerun"));
    }

    // --- MUV-2: copy-only credential migration ----------------------------

    /// Build a closure-backed fake vault: returns the key list and a reader.
    #[allow(clippy::type_complexity)]
    fn fake_vault(
        entries: &[(&str, &str)],
    ) -> (
        Vec<String>,
        impl Fn(&str) -> Result<Option<Zeroizing<String>>, String>,
    ) {
        let map: HashMap<String, String> = entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let keys: Vec<String> = map.keys().cloned().collect();
        let reader = move |key: &str| Ok(map.get(key).map(|v| Zeroizing::new(v.clone())));
        (keys, reader)
    }

    #[test]
    fn eager_migration_copies_profile_and_owner_globals_only() {
        let _guard = test_lock();
        let conn = migrated_conn(2); // default (admin) owns profile-0, profile-1
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active")
            .expect("default active");

        let (keys, read) = fake_vault(&[
            ("server_profile-0", "pw0"),
            ("server_profile-1", "pw1"),
            ("oauth_dropbox_profile-0", "{\"access\":\"a\"}"),
            ("jottacloud_refresh_profile-1", "jrt"),
            ("ai_apikey_anthropic", "sk-ant"),
            ("github_pat", "ghp_x"),
            ("github_oauth_token", "gho_y"),
            // machine-global (MUV-0): must NOT migrate
            ("github_pem_1234_5678", "-----PEM-----"),
            ("github_app_credentials", "{\"appId\":1}"),
            ("totp_secret", "vault-reserved"),
        ]);

        let migrated = migrate_user_credentials_inner(&conn, &root, default.id, &keys, &read)
            .expect("migrate");
        assert_eq!(migrated, 7);

        let ids: HashSet<String> = list_user_credential_ids_for(&conn, default.id)
            .expect("list")
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        for present in [
            "server_profile-0",
            "server_profile-1",
            "oauth_dropbox_profile-0",
            "jottacloud_refresh_profile-1",
            "ai_apikey_anthropic",
            "github_pat",
            "github_oauth_token",
        ] {
            assert!(ids.contains(present), "missing {present}");
        }
        for absent in [
            "github_pem_1234_5678",
            "github_app_credentials",
            "totp_secret",
        ] {
            assert!(!ids.contains(absent), "unexpected {absent}");
        }
        assert_eq!(
            get_user_credential_for(&conn, &root, default.id, "server_profile-0")
                .expect("get")
                .expect("present")
                .as_str(),
            "pw0"
        );

        // Idempotent: marked, second pass copies nothing, no duplicate rows.
        assert!(is_creds_migrated(&conn, default.id).expect("marker"));
        let again =
            migrate_user_credentials_inner(&conn, &root, default.id, &keys, &read).expect("rerun");
        assert_eq!(again, 0);
        assert_eq!(
            list_user_credential_ids_for(&conn, default.id)
                .expect("list2")
                .len(),
            7
        );
    }

    #[test]
    fn eager_migration_excludes_globals_from_non_owner() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let alice = create_passphrase_less_user(&mut conn, &root, "alice", None, None)
            .expect("create alice");
        replace_server_profiles_for(
            &mut conn,
            &root,
            alice.id,
            &[json!({"id":"ap1","name":"A","protocol":"sftp"})],
        )
        .expect("alice profile");

        let (keys, read) = fake_vault(&[("server_ap1", "apw"), ("ai_apikey_openai", "sk-openai")]);
        let migrated =
            migrate_user_credentials_inner(&conn, &root, alice.id, &keys, &read).expect("migrate");
        // Only the profile-bound secret; the AI key belongs to the lowest-id
        // admin (default), not alice.
        assert_eq!(migrated, 1);
        let ids: HashSet<String> = list_user_credential_ids_for(&conn, alice.id)
            .expect("list")
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert!(ids.contains("server_ap1"));
        assert!(!ids.contains("ai_apikey_openai"));
    }

    #[test]
    fn passphrase_user_migrates_lazily_at_unlock() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let bob = create_user(&mut conn, &root, "Bob", None, None, Some("bobpass123"))
            .expect("create bob");
        // Write bob's profile while unlocked, then drop the session to simulate
        // a locked account at migration time.
        unlock_user(&conn, &root, bob.id, Some("bobpass123")).expect("unlock to seed");
        replace_active_server_profiles(
            &mut conn,
            &root,
            &[json!({"id":"bp1","name":"B","protocol":"sftp"})],
        )
        .expect("seed profile");
        clear_user_session();

        let (keys, read) = fake_vault(&[("server_bp1", "bpw")]);

        // Locked: no-op, not marked (deferred to unlock).
        let deferred =
            migrate_user_credentials_inner(&conn, &root, bob.id, &keys, &read).expect("deferred");
        assert_eq!(deferred, 0);
        assert!(!is_creds_migrated(&conn, bob.id).expect("marker off"));
        assert!(list_user_credential_ids_for(&conn, bob.id)
            .expect("empty")
            .is_empty());

        // Unlock primes the session DEK; now the lazy pass copies + marks.
        unlock_user(&conn, &root, bob.id, Some("bobpass123")).expect("unlock");
        let migrated =
            migrate_user_credentials_inner(&conn, &root, bob.id, &keys, &read).expect("lazy");
        assert_eq!(migrated, 1);
        assert!(is_creds_migrated(&conn, bob.id).expect("marker on"));
        assert_eq!(
            get_user_credential_for(&conn, &root, bob.id, "server_bp1")
                .expect("get")
                .expect("present")
                .as_str(),
            "bpw"
        );
    }

    #[test]
    fn reader_fallback_prefers_user_store_then_vault() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active")
            .expect("default active");

        set_user_credential_for(&conn, &root, default.id, "server_x", "server", "from-store")
            .expect("seed store");
        let (_keys, read) = fake_vault(&[("server_x", "from-vault"), ("server_y", "vault-only")]);

        // Per-user row wins over the legacy vault.
        assert_eq!(
            read_credential_with_fallback_inner(&conn, &root, default.id, "server_x", &read)
                .expect("read x")
                .expect("present")
                .as_str(),
            "from-store"
        );
        // No per-user row -> fall back to the vault.
        assert_eq!(
            read_credential_with_fallback_inner(&conn, &root, default.id, "server_y", &read)
                .expect("read y")
                .expect("present")
                .as_str(),
            "vault-only"
        );
        // Absent everywhere -> None.
        assert!(
            read_credential_with_fallback_inner(&conn, &root, default.id, "server_z", &read)
                .expect("read z")
                .is_none()
        );
    }

    #[test]
    fn reader_fallback_refuses_locked_passphrase_user() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let bob = create_user(&mut conn, &root, "Bob", None, None, Some("bobpass123"))
            .expect("create bob");
        clear_user_session();

        let (_keys, read) = fake_vault(&[("server_bp1", "vault-bob")]);
        assert_eq!(
            read_credential_with_fallback_inner(&conn, &root, bob.id, "server_bp1", &read)
                .expect_err("locked partition must fail closed"),
            "USER_LOCKED"
        );
    }

    #[test]
    fn legacy_owner_survives_demotion_and_is_not_reassigned_on_delete() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active")
            .expect("default active");
        assert_eq!(nonprofile_secret_owner(&conn).unwrap(), Some(default.id));
        let second = create_passphrase_less_user(&mut conn, &root, "second", None, None)
            .expect("create second");
        conn.execute(
            "UPDATE users SET is_admin = 1 WHERE id = ?1",
            params![second.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE users SET is_admin = 0 WHERE id = ?1",
            params![default.id],
        )
        .unwrap();
        assert_eq!(
            nonprofile_secret_owner(&conn).unwrap(),
            Some(default.id),
            "a demotion does not move the legacy owner"
        );
        conn.execute("DELETE FROM users WHERE id = ?1", params![default.id])
            .unwrap();
        assert_eq!(
            nonprofile_secret_owner(&conn).unwrap(),
            None,
            "a deleted owner is not reassigned to the next admin"
        );
    }

    #[test]
    fn owner_aware_reader_preserves_legacy_owner_fallback() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active")
            .expect("default active");
        let vault_reads = std::cell::Cell::new(0u32);
        let read = |_key: &str| {
            vault_reads.set(vault_reads.get() + 1);
            Ok(Some(Zeroizing::new("legacy-secret".to_string())))
        };

        assert_eq!(
            read_credential_owner_aware_inner(&conn, &root, default.id, "server_legacy", &read)
                .expect("legacy owner read")
                .expect("legacy value")
                .as_str(),
            "legacy-secret"
        );
        assert_eq!(vault_reads.get(), 1, "legacy owner migrates on a miss");
    }

    /// Every key a private token is written under maps to its type, and only
    /// those: the app settings and globals that share a prefix with them, and
    /// every other credential, map to none. Each type of
    /// `PRIVATE_TOKEN_TYPES` is reached.
    #[test]
    fn every_private_token_key_maps_to_its_type_and_no_other_key_does() {
        let private = [
            ("jottacloud_refresh", "jottacloud_refresh"),
            ("jottacloud_refresh_srv_1", "jottacloud_refresh"),
            ("oauth_google", "oauth"),
            ("oauth_google_srv_1", "oauth"),
            ("oauth_googlephotos", "oauth"),
            ("oauth_googlephotos_srv_1", "oauth"),
            ("oauth_dropbox_srv_1", "oauth"),
            ("oauth_onedrive_srv_1", "oauth"),
            ("oauth_box_srv_1", "oauth"),
            ("oauth_pcloud_srv_1", "oauth"),
            ("oauth_zohoworkdrive_srv_1", "oauth"),
            ("oauth_yandexdisk_srv_1", "oauth"),
            ("oauth_fourshared_srv_1", "oauth"),
            ("profile_auth_config_srv_1", "oauth"),
            ("profile_auth_pending_srv_1", "oauth"),
            ("profile_auth_deadline_srv_1", "oauth"),
            ("github_oauth_srv_1", "oauth"),
            ("github_pem_srv_1", "oauth"),
            ("github_pem_srv_1730000000000_123456789", "oauth"),
            ("proton_native_session", "proton_native_session"),
            ("proton_native_session_srv_1", "proton_native_session"),
            ("proton_native_unbound_user_7", "proton_native_session"),
        ];
        for (key, kind) in private {
            assert_eq!(private_token_type(key), Some(kind), "{key}");
        }
        for kind in PRIVATE_TOKEN_TYPES {
            assert!(
                private.iter().any(|(_, k)| *k == kind),
                "no key reaches the private type {kind}"
            );
        }
        for key in [
            "oauth_google_client_id",
            "oauth_dropbox_client_secret",
            "oauth_pcloud_region",
            "oauth_zohoworkdrive_region",
            "oauth_rollback_app_client_id",
            "oauth_token",
            "oauth_",
            "oauth_google_",
            "github_oauth_token",
            "github_pat",
            "github_oauth",
            // A GitHub App's PEM, keyed by its app and installation ids.
            "github_pem_1234_5678",
            "profile_auth_config",
            "profile_auth_config_",
            "jottacloud_refreshed",
            "proton_native_session_",
            "proton_native_unbound_user_",
            "proton_native_sessions",
            "server_srv_1",
            "server_modes_srv_1",
            "ai_apikey_openai",
            "aerocrypt_overlay_pw_srv_1",
            "filen_api_key_srv_1",
            "onedrive_drive_id_srv_1",
            "totp_secret",
        ] {
            assert_eq!(private_token_type(key), None, "{key}");
        }
    }

    /// A private token of an account other than the legacy owner is read from
    /// that account's partition alone: the vault key of the same name is the
    /// legacy owner's, and the owner still reads it. Any other key keeps the
    /// vault fallback for every account.
    #[test]
    fn a_private_token_is_never_read_from_the_vault_for_another_account() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let owner = get_active_user(&conn)
            .expect("active")
            .expect("default active");
        let user = create_passphrase_less_user(&mut conn, &root, "private", None, None)
            .expect("create user");
        let vault_reads = std::cell::Cell::new(0u32);
        let read = |_key: &str| {
            vault_reads.set(vault_reads.get() + 1);
            Ok(Some(Zeroizing::new("legacy-owners-token".to_string())))
        };
        for key in [
            "oauth_dropbox_srv_1",
            "jottacloud_refresh",
            "github_oauth_srv_1",
        ] {
            vault_reads.set(0);
            assert!(
                read_credential_with_fallback_inner(&conn, &root, user.id, key, &read)
                    .expect("private read")
                    .is_none(),
                "{key}"
            );
            assert_eq!(vault_reads.get(), 0, "{key}: the vault was read");
            assert_eq!(
                read_credential_with_fallback_inner(&conn, &root, owner.id, key, &read)
                    .expect("owner read")
                    .expect("owner value")
                    .as_str(),
                "legacy-owners-token",
                "{key}"
            );
        }
        assert_eq!(
            read_credential_with_fallback_inner(&conn, &root, user.id, "server_srv_1", &read)
                .expect("server read")
                .expect("vault fallback")
                .as_str(),
            "legacy-owners-token"
        );
    }

    #[test]
    fn owner_aware_reader_never_reads_vault_for_nonowner() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let user = create_passphrase_less_user(&mut conn, &root, "isolated", None, None)
            .expect("create user");
        set_user_credential_for(&conn, &root, user.id, "server_own", "server", "own-secret")
            .expect("seed own credential");
        let vault_reads = std::cell::Cell::new(0u32);
        let read = |_key: &str| {
            vault_reads.set(vault_reads.get() + 1);
            Ok(Some(Zeroizing::new("another-users-secret".to_string())))
        };

        assert_eq!(
            read_credential_owner_aware_inner(&conn, &root, user.id, "server_own", &read)
                .expect("own read")
                .expect("own value")
                .as_str(),
            "own-secret"
        );
        assert!(
            read_credential_owner_aware_inner(&conn, &root, user.id, "server_missing", &read)
                .expect("absent own credential")
                .is_none(),
            "a global value is not an owned credential"
        );
        assert_eq!(vault_reads.get(), 0, "non-owner never reads the vault");
    }

    #[test]
    fn owner_aware_reader_preserves_locked_error_without_vault_read() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let bob = create_user(&mut conn, &root, "Bob", None, None, Some("bobpass123"))
            .expect("create bob");
        clear_user_session();
        let vault_reads = std::cell::Cell::new(0u32);
        let read = |_key: &str| {
            vault_reads.set(vault_reads.get() + 1);
            Ok(Some(Zeroizing::new("must-not-leak".to_string())))
        };

        let error = read_credential_owner_aware_inner(&conn, &root, bob.id, "server_bob", &read)
            .expect_err("locked partition must fail closed");
        assert_eq!(error, "USER_LOCKED");
        assert!(!error.contains("must-not-leak"));
        assert_eq!(
            vault_reads.get(),
            0,
            "locked non-owner never reads the vault"
        );
    }

    #[test]
    fn reader_fallback_consults_vault_only_on_partition_miss_for_nondefault_user() {
        // R-MUV-10 coverage: post-cutover, a non-default user is active and a
        // server_* credential lives ONLY in the legacy vault (never mirrored into
        // the partition). The reader must (a) return the vault value via fallback
        // and (b) consult the vault ONLY on the partition miss, never when the
        // per-user row is present. This pins down the partition-miss -> vault-
        // fallback transition the live R-MUV-10 run could not observe directly
        // (no explicit log line marked the fallback).
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let user = create_passphrase_less_user(&mut conn, &root, "cutover", None, None)
            .expect("create user");

        // Only `server_hit` is mirrored into the user's partition; the
        // `server_vaultonly` key is never written there.
        set_user_credential_for(&conn, &root, user.id, "server_hit", "server", "from-store")
            .expect("seed store");

        let vault_reads = std::cell::Cell::new(0u32);
        let read = |key: &str| {
            vault_reads.set(vault_reads.get() + 1);
            Ok(match key {
                "server_hit" => Some(Zeroizing::new("from-vault".to_string())),
                "server_vaultonly" => Some(Zeroizing::new("vault-only".to_string())),
                _ => None,
            })
        };

        // Partition hit: the per-user row wins and the vault is NOT consulted.
        assert_eq!(
            read_credential_with_fallback_inner(&conn, &root, user.id, "server_hit", &read)
                .expect("read hit")
                .expect("present")
                .as_str(),
            "from-store"
        );
        assert_eq!(
            vault_reads.get(),
            0,
            "vault must not be read on a partition hit"
        );

        // Partition miss: the vault-only credential resolves via fallback and the
        // vault is read exactly once.
        assert_eq!(
            read_credential_with_fallback_inner(&conn, &root, user.id, "server_vaultonly", &read)
                .expect("read vault-only")
                .expect("present")
                .as_str(),
            "vault-only"
        );
        assert_eq!(
            vault_reads.get(),
            1,
            "vault consulted exactly once on the partition miss"
        );

        // The partition genuinely has no row for it (it really was the fallback).
        assert!(
            get_user_credential_for(&conn, &root, user.id, "server_vaultonly")
                .expect("read partition")
                .is_none()
        );
    }

    #[test]
    fn oauth_token_refresh_updates_partition_and_reader_resolves_with_vault_fallback() {
        // R-MUV-4 coverage (Dropbox-style OAuth refresh cutover): an
        // `oauth_<provider>_<id>` token is mirrored into the active user's
        // partition via the typed write path, a refresh overwrites it in place
        // (upsert), and the reader resolves the refreshed value from the
        // partition. A token that was never mirrored still resolves via the
        // legacy vault fallback. Mirrors the live R-MUV-4 GUI test (which needs an
        // interactive Dropbox OAuth refresh) at the engine level.
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let user = create_passphrase_less_user(&mut conn, &root, "oauthuser", None, None)
            .expect("create user");

        // Initial token, then a refresh that rewrites it in place.
        set_user_credential_for(
            &conn,
            &root,
            user.id,
            "oauth_dropbox_42",
            "oauth",
            "token-v1",
        )
        .expect("seed oauth");
        set_user_credential_for(
            &conn,
            &root,
            user.id,
            "oauth_dropbox_42",
            "oauth",
            "token-v2",
        )
        .expect("refresh oauth");

        let (_keys, read) = fake_vault(&[("oauth_dropbox_99", "vault-token")]);

        // Refreshed value resolves from the partition (not the older vault copy).
        assert_eq!(
            read_credential_with_fallback_inner(&conn, &root, user.id, "oauth_dropbox_42", &read)
                .expect("read refreshed")
                .expect("present")
                .as_str(),
            "token-v2"
        );
        // A never-mirrored OAuth token resolves via the vault fallback for the
        // legacy owner, whose key it is, and for nobody else.
        let owner = nonprofile_secret_owner(&conn)
            .expect("owner")
            .expect("legacy owner");
        assert_ne!(owner, user.id);
        assert_eq!(
            read_credential_with_fallback_inner(&conn, &root, owner, "oauth_dropbox_99", &read)
                .expect("read vault-only oauth")
                .expect("present")
                .as_str(),
            "vault-token"
        );
        assert!(read_credential_with_fallback_inner(
            &conn,
            &root,
            user.id,
            "oauth_dropbox_99",
            &read
        )
        .expect("read vault-only oauth")
        .is_none());
    }

    // --- MUV-3: server_* reader/writer cutover ----------------------------

    #[test]
    fn muv3_credential_type_classifies_server_and_ai_apikey_keys() {
        // The prefix-mirrored namespaces: `server_*` (MUV-3) and `ai_apikey_*`
        // (MUV-5, unambiguous so prefix matching is safe).
        assert_eq!(muv3_credential_type("server_42"), Some("server"));
        assert_eq!(muv3_credential_type("server_"), Some("server"));
        assert_eq!(
            muv3_credential_type("ai_apikey_anthropic"),
            Some("ai_apikey")
        );
        assert_eq!(muv3_credential_type("ai_apikey_openai"), Some("ai_apikey"));
        // OAuth/Jottacloud/GitHub stay out of the prefix classifier: they are
        // mirrored by type-explicit call-sites, and `config_server_profiles` is
        // never a secret.
        assert_eq!(muv3_credential_type("oauth_dropbox_42"), None);
        assert_eq!(muv3_credential_type("jottacloud_refresh_42"), None);
        assert_eq!(muv3_credential_type("github_pat"), None);
        assert_eq!(muv3_credential_type("github_oauth_token"), None);
        assert_eq!(muv3_credential_type("config_server_profiles"), None);
    }

    #[test]
    fn dek_based_insert_writes_to_target_user_only() {
        // set_user_credential_with_dek (the relocation write path) lands the
        // secret in the supplied user's partition and nowhere else (R3 on write).
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        let user_a = get_active_user(&conn)
            .expect("active read")
            .expect("default active");
        let user_b = create_passphrase_less_user(&mut conn, &root, "userb", None, None)
            .expect("create userb");

        let dek_b =
            resolve_user_dek_scoped(&conn, &root_secret, user_b.id, None).expect("resolve B dek");
        set_user_credential_with_dek(&conn, user_b.id, &dek_b, "server_x", "server", "b-secret")
            .expect("dek insert");

        // B reads its own; A sees nothing under the same key.
        assert_eq!(
            get_user_credential_for(&conn, &root, user_b.id, "server_x")
                .expect("read B")
                .expect("present")
                .as_str(),
            "b-secret"
        );
        assert!(get_user_credential_for(&conn, &root, user_a.id, "server_x")
            .expect("read A")
            .is_none());
    }

    #[test]
    fn relocate_credential_to_locked_passphrase_target_via_scoped_dek() {
        // The cross-user relocation must be able to write the secret onto a
        // passphrase-protected target that has NO active session, using the
        // target's scoped DEK (unwrapped with its passphrase), without leaking
        // into the active (source) partition.
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let root_secret = user_crypto::secret_key_from_bytes(&root);
        let source = get_active_user(&conn)
            .expect("active read")
            .expect("default active"); // device-wrapped, active
        let target = create_user(&mut conn, &root, "Target", None, None, Some("targetpass1"))
            .expect("create target");
        assert!(target.has_passphrase);

        set_user_credential_for(
            &conn,
            &root,
            source.id,
            "server_src",
            "server",
            "src-secret",
        )
        .expect("set source secret");

        // Relocation write: scoped DEK for the locked target, no session.
        clear_user_session();
        let target_dek =
            resolve_user_dek_scoped(&conn, &root_secret, target.id, Some("targetpass1"))
                .expect("resolve target dek");
        set_user_credential_with_dek(
            &conn,
            target.id,
            &target_dek,
            "server_new",
            "server",
            "src-secret",
        )
        .expect("mirror onto target");

        // Without a session the target row is locked; after unlock it decrypts.
        assert_eq!(
            get_user_credential_for(&conn, &root, target.id, "server_new").expect_err("locked"),
            "USER_LOCKED"
        );
        unlock_user(&conn, &root, target.id, Some("targetpass1")).expect("unlock target");
        assert_eq!(
            get_user_credential_for(&conn, &root, target.id, "server_new")
                .expect("read target")
                .expect("present")
                .as_str(),
            "src-secret"
        );
        clear_user_session();

        // The active (source) partition keeps its own secret and never received
        // the relocated id.
        assert_eq!(
            get_user_credential_for(&conn, &root, source.id, "server_src")
                .expect("read source")
                .expect("present")
                .as_str(),
            "src-secret"
        );
        assert!(
            get_user_credential_for(&conn, &root, source.id, "server_new")
                .expect("read source new")
                .is_none()
        );
    }

    // --- MUV-4: OAuth / Jottacloud token cutover --------------------------

    #[test]
    fn oauth_and_jotta_tokens_use_explicit_type_outside_the_classifier() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active read")
            .expect("default active");

        // The prefix classifier stays `server_`-only on purpose: it cannot tell a
        // per-user token (`oauth_<p>_<id>`) from machine/app config
        // (`oauth_<p>_client_id`), so the generic store_credential command never
        // mirrors OAuth keys. MUV-4 call-sites pass the type explicitly instead.
        assert_eq!(muv3_credential_type("oauth_dropbox_42"), None);
        assert_eq!(muv3_credential_type("jottacloud_refresh_42"), None);
        assert_eq!(muv3_credential_type("oauth_google_client_id"), None);

        // An explicit-type write round-trips and is listed under its own type.
        set_user_credential_for(
            &conn,
            &root,
            default.id,
            "oauth_dropbox_42",
            "oauth",
            "{\"access\":\"a\"}",
        )
        .expect("store oauth");
        set_user_credential_for(
            &conn,
            &root,
            default.id,
            "jottacloud_refresh_42",
            "jottacloud_refresh",
            "{\"refresh_token\":\"r\"}",
        )
        .expect("store jotta");

        assert_eq!(
            get_user_credential_for(&conn, &root, default.id, "oauth_dropbox_42")
                .expect("read oauth")
                .expect("present")
                .as_str(),
            "{\"access\":\"a\"}"
        );
        let ids = list_user_credential_ids_for(&conn, default.id).expect("list");
        assert!(ids
            .iter()
            .any(|(k, t)| k == "oauth_dropbox_42" && t == "oauth"));
        assert!(ids
            .iter()
            .any(|(k, t)| k == "jottacloud_refresh_42" && t == "jottacloud_refresh"));
    }

    #[test]
    fn forced_unmirror_removes_per_profile_oauth_without_classifying_app_globals() {
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active read")
            .expect("default active");

        set_user_credential_for(
            &conn,
            &root,
            default.id,
            "oauth_dropbox_profile-1",
            "oauth",
            "partition-token",
        )
        .expect("store per-profile oauth");
        set_user_credential_for(
            &conn,
            &root,
            default.id,
            "oauth_dropbox_client_id",
            "oauth_config",
            "app-client-id",
        )
        .expect("store app-global oauth config");

        assert_eq!(muv3_credential_type("oauth_dropbox_profile-1"), None);
        assert_eq!(muv3_credential_type("oauth_dropbox_client_id"), None);

        delete_user_credential_for(&conn, default.id, "oauth_dropbox_profile-1")
            .expect("forced per-profile unmirror");
        assert!(
            get_user_credential_for(&conn, &root, default.id, "oauth_dropbox_profile-1")
                .expect("read per-profile oauth")
                .is_none(),
            "the per-profile OAuth token must be gone from the partition"
        );
        assert_eq!(
            get_user_credential_for(&conn, &root, default.id, "oauth_dropbox_client_id")
                .expect("read app-global config")
                .expect("app-global config remains")
                .as_str(),
            "app-client-id"
        );

        let empty_vault = |_key: &str| Ok(None);
        assert!(
            read_credential_with_fallback_inner(
                &conn,
                &root,
                default.id,
                "oauth_dropbox_profile-1",
                &empty_vault,
            )
            .expect("resolve deleted oauth")
            .is_none(),
            "after partition unmirror and vault delete, fallback must resolve None"
        );
    }

    // --- MUV-5: ai_apikey + github cutover, transport portability ---------

    #[test]
    fn ai_apikey_mirrors_into_the_active_user_and_isolates_per_user() {
        // ai_apikey is in the prefix classifier, so the generic dual-write
        // mirrors it into the active user's partition and never into another's.
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active read")
            .expect("default active");
        let alice = create_passphrase_less_user(&mut conn, &root, "alice", None, None)
            .expect("create alice");

        // Mirror into the active (default) user via the type derived from the
        // classifier (mirrors what store_active_credential_dual does in prod).
        let ctype = muv3_credential_type("ai_apikey_openai").expect("ai key is classified");
        set_user_credential_for(
            &conn,
            &root,
            default.id,
            "ai_apikey_openai",
            ctype,
            "sk-openai",
        )
        .expect("store ai key");

        // Round-trip on the owner; absent for another user (R3 on write).
        assert_eq!(
            get_user_credential_for(&conn, &root, default.id, "ai_apikey_openai")
                .expect("read default")
                .expect("present")
                .as_str(),
            "sk-openai"
        );
        assert!(
            get_user_credential_for(&conn, &root, alice.id, "ai_apikey_openai")
                .expect("read alice")
                .is_none()
        );
        // Listed under its own type.
        let ids = list_user_credential_ids_for(&conn, default.id).expect("list");
        assert!(ids
            .iter()
            .any(|(k, t)| k == "ai_apikey_openai" && t == "ai_apikey"));
    }

    #[test]
    fn github_tokens_round_trip_typed_and_isolated() {
        // GitHub tokens are out of the prefix classifier (so they never touch
        // the machine-global github_pem_*), but the type-explicit writers store
        // them per-user under a "github" type.
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn)
            .expect("active read")
            .expect("default active");
        let alice = create_passphrase_less_user(&mut conn, &root, "alice", None, None)
            .expect("create alice");

        assert_eq!(muv3_credential_type("github_pat"), None);
        assert_eq!(muv3_credential_type("github_oauth_token"), None);

        set_user_credential_for(&conn, &root, default.id, "github_pat", "github", "ghp_x")
            .expect("store pat");
        set_user_credential_for(
            &conn,
            &root,
            default.id,
            "github_oauth_token",
            "github",
            "gho_y",
        )
        .expect("store oauth token");

        assert_eq!(
            get_user_credential_for(&conn, &root, default.id, "github_pat")
                .expect("read pat")
                .expect("present")
                .as_str(),
            "ghp_x"
        );
        // Not leaked into another user.
        assert!(
            get_user_credential_for(&conn, &root, alice.id, "github_pat")
                .expect("read alice")
                .is_none()
        );
        let ids = list_user_credential_ids_for(&conn, default.id).expect("list");
        assert!(ids.iter().any(|(k, t)| k == "github_pat" && t == "github"));
        assert!(ids
            .iter()
            .any(|(k, t)| k == "github_oauth_token" && t == "github"));
    }

    #[test]
    fn user_credentials_row_is_portable_via_transport_dek() {
        // R-MUV-3: a per-user secret is encrypted under the user's DEK, the same
        // DEK the F-012 transport sidecar carries. After re-keying the DEK to a
        // different machine's local root_key, the credential row decrypts with
        // no per-row change. This is what makes a Full export's user_credentials
        // portable cross-machine without secret-specific wrapping.
        let _guard = test_lock();
        let conn = migrated_conn(0);
        let root_a = test_root();
        let root_b = [0x5au8; 32]; // another machine's local root_key

        let default = get_active_user(&conn)
            .expect("active read")
            .expect("default active");

        // Seed a device-wrapped secret on machine A.
        set_user_credential_for(
            &conn,
            &root_a,
            default.id,
            "server_42",
            "server",
            "top-secret",
        )
        .expect("seed secret");
        assert_eq!(
            get_user_credential_for(&conn, &root_a, default.id, "server_42")
                .expect("read on A")
                .expect("present")
                .as_str(),
            "top-secret"
        );

        // Export the transport DEK under the backup password.
        let transport = export_transport_deks(&conn, &root_a, "backup password 123")
            .expect("export transport")
            .expect("a passphrase-less user exists");
        assert!(transport.wrapped_deks.contains_key(&default.id));

        // Blind-overwrite import on machine B: same rows, different local root.
        // Before re-keying, B cannot decrypt the secret.
        assert!(
            get_user_credential_for(&conn, &root_b, default.id, "server_42").is_err(),
            "secret must be unreadable under the wrong root_key before rekey"
        );

        // Re-key the DEK to machine B's local root_key.
        let report = rekey_transport_deks(
            &conn,
            &root_b,
            "backup password 123",
            &transport.salt,
            &transport.kdf_params,
            &transport.wrapped_deks,
        )
        .expect("rekey");
        assert_eq!(report.rekeyed, 1);
        assert_eq!(report.unreadable, 0);

        // Machine B now decrypts the same secret (DEK unchanged, only rewrapped).
        assert_eq!(
            get_user_credential_for(&conn, &root_b, default.id, "server_42")
                .expect("read on B")
                .expect("present")
                .as_str(),
            "top-secret"
        );
    }

    /// Headless end-to-end validation of the MUV-1..5 release surface against a
    /// REAL on-disk `CredentialStore` (master mode, no keyring) and a REAL
    /// `user_partitions.db`, isolated under a throwaway `XDG_CONFIG_HOME`.
    ///
    /// Unlike the other tests in this module (closure-backed engine + a fixed
    /// `test_root`), this drives the production store-backed path:
    /// `init_or_migrate_cli` -> `migrate_credentials_eager_all` /
    /// `migrate_credentials_for_user`, the `resolve`/`read_credential_with_fallback`
    /// readers, `mcp_list_active_server_profiles`, and the transport DEK
    /// round-trip. It covers DoD #1/#4 with real output for R-MUV-1, R-MUV-3,
    /// R-MUV-5 and R-MUV-8.
    ///
    /// `#[ignore]` because it mutates process-global state (`XDG_CONFIG_HOME`,
    /// the `VAULT_CACHE`/`USER_SESSION` statics) and so must run ALONE:
    ///   cargo test --lib user_partitions::tests::muv_release_e2e_validation \
    ///       -- --ignored --nocapture --test-threads=1
    #[test]
    #[ignore = "headless MUV e2e; run alone with --ignored --test-threads=1 --nocapture"]
    fn muv_release_e2e_validation() {
        use crate::credential_store::CredentialStore;

        fn count_prefix(conn: &Connection, uid: i64, prefix: &str) -> usize {
            list_user_credential_ids_for(conn, uid)
                .expect("list credential ids")
                .into_iter()
                .filter(|(id, _)| id.starts_with(prefix))
                .count()
        }

        let _guard = test_lock();

        // --- Isolation: a throwaway data root, master-mode vault (no keyring) --
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let prev_pp = std::env::var_os("AEROFTP_USER_PASSPHRASE");
        let tmp = tempfile::TempDir::new().expect("tempdir");
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        std::env::remove_var("AEROFTP_USER_PASSPHRASE");

        CredentialStore::bootstrap_master_password("muv-validation-pass")
            .expect("bootstrap master vault");
        CredentialStore::unlock_with_master("muv-validation-pass", None).expect("unlock master");
        let store = CredentialStore::from_cache().expect("store cached");

        // Schema v4 + the legacy `default` admin user.
        init_or_migrate_cli(&store).expect("init_or_migrate_cli");
        let mut conn = open_or_init_cli().expect("open partition db");
        let root_key = store.derive_user_partition_wrapping_key();

        // --- Build the pre-migration state: 3 users x 10 profiles = 30 --------
        let alice = create_user(&mut conn, &root_key, "Alice", None, None, None).expect("alice");
        let bob = create_user(&mut conn, &root_key, "Bob", None, None, None).expect("bob");
        let carol = create_user(
            &mut conn,
            &root_key,
            "Carol",
            None,
            None,
            Some("carol-pass"),
        )
        .expect("carol");
        assert!(!alice.has_passphrase && !bob.has_passphrase && carol.has_passphrase);

        let make_profiles = |prefix: &str| -> Vec<serde_json::Value> {
            (1..=10)
                .map(|i| {
                    json!({
                        "id": format!("{prefix}{i}"),
                        "name": format!("{prefix}-{i}"),
                        "protocol": "ftp",
                        "host": format!("{prefix}{i}.example.test"),
                        "port": 21,
                        "username": "tester"
                    })
                })
                .collect()
        };
        replace_server_profiles_for(&mut conn, &root_key, alice.id, &make_profiles("a"))
            .expect("alice profiles");
        replace_server_profiles_for(&mut conn, &root_key, bob.id, &make_profiles("b"))
            .expect("bob profiles");
        // Carol is passphrase-protected: prime her session to seed, then re-lock.
        unlock_user(&conn, &root_key, carol.id, Some("carol-pass")).expect("unlock carol to seed");
        replace_server_profiles_for(&mut conn, &root_key, carol.id, &make_profiles("c"))
            .expect("carol profiles");

        // Legacy vault secrets (vault-only, the pre-MUV state).
        for prefix in ["a", "b", "c"] {
            for i in 1..=10 {
                store
                    .store(
                        &format!("server_{prefix}{i}"),
                        &format!("secret-{prefix}{i}"),
                    )
                    .expect("seed server secret");
            }
        }
        // An orphan secret (profile deleted but secret left behind) + globals.
        store
            .store("server_orphan99", "orphan-secret")
            .expect("orphan");
        store
            .store("ai_apikey_openai", "sk-test-key")
            .expect("ai key");
        store
            .store("github_pat", "ghp_testpat")
            .expect("github pat");

        // Reset the idempotency gate + session to simulate a fresh upgrade boot.
        conn.execute(
            "DELETE FROM global_state WHERE key LIKE 'creds_migrated_%'",
            [],
        )
        .expect("clear markers");
        clear_user_session();

        let admin_id = list_users(&conn)
            .expect("list users")
            .into_iter()
            .filter(|u| u.is_admin)
            .map(|u| u.id)
            .min()
            .expect("an admin exists");

        // --- R-MUV-1: eager migration of device-wrapped users -----------------
        let migrated = migrate_credentials_eager_all(&conn, &store, &root_key).expect("eager");
        println!("[R-MUV-1] eager migrated {migrated} rows (alice+bob server_* + admin globals)");

        assert_eq!(
            count_prefix(&conn, alice.id, "server_"),
            10,
            "alice has 10 server_*"
        );
        assert_eq!(
            count_prefix(&conn, bob.id, "server_"),
            10,
            "bob has 10 server_*"
        );
        assert_eq!(
            count_prefix(&conn, carol.id, "server_"),
            0,
            "carol (locked passphrase) deferred, not eager-migrated"
        );
        // Value round-trips under the user DEK.
        assert_eq!(
            get_user_credential_for(&conn, &root_key, alice.id, "server_a1")
                .expect("read alice")
                .expect("present")
                .as_str(),
            "secret-a1"
        );
        // Isolation (R3): alice's partition never holds bob's secret.
        assert!(
            get_user_credential_for(&conn, &root_key, alice.id, "server_b1")
                .expect("read cross")
                .is_none(),
            "no cross-user leak in user_credentials"
        );
        // Globals went to the lowest-id admin only.
        assert!(
            get_user_credential_for(&conn, &root_key, admin_id, "ai_apikey_openai")
                .expect("admin ai key")
                .is_some(),
            "ai key owned by admin"
        );
        assert!(
            get_user_credential_for(&conn, &root_key, alice.id, "ai_apikey_openai")
                .expect("alice ai key")
                .is_none(),
            "ai key NOT duplicated to non-owner"
        );
        // Orphan: readable via the vault fallback, but never in any partition.
        assert_eq!(
            read_credential_with_fallback(&conn, &store, &root_key, alice.id, "server_orphan99")
                .expect("orphan fallback")
                .expect("present")
                .as_str(),
            "orphan-secret"
        );
        assert_eq!(
            count_prefix(&conn, alice.id, "server_orphan"),
            0,
            "orphan stays in vault"
        );
        assert!(
            resolve_credential_for_user_owner_aware(&store, alice.id, "server_orphan99")
                .expect("owner-aware orphan read")
                .is_none(),
            "owner-aware reader does not lend the orphan to Alice"
        );
        assert!(
            resolve_credential_for_user_owner_aware(&store, alice.id, "ai_apikey_openai")
                .expect("owner-aware AI key read")
                .is_none(),
            "owner-aware reader does not lend the admin AI key to Alice"
        );
        assert_eq!(
            resolve_credential_for_user_owner_aware(&store, admin_id, "server_orphan99")
                .expect("legacy owner orphan read")
                .expect("legacy orphan")
                .as_str(),
            "orphan-secret",
            "legacy owner retains the migration fallback"
        );
        println!("[R-MUV-1] OK: 30 profiles separated per-user, isolation + orphan-fallback hold");

        // --- R-MUV-8: idempotent re-run (kill/restart safety) -----------------
        let again = migrate_credentials_eager_all(&conn, &store, &root_key).expect("re-run");
        assert_eq!(again, 0, "second eager pass is a no-op");
        assert_eq!(
            count_prefix(&conn, alice.id, "server_"),
            10,
            "no duplication on re-run"
        );
        println!("[R-MUV-8] OK: re-run migrated 0 rows, counts stable (idempotent)");

        // --- Lazy migration for the passphrase user at unlock -----------------
        unlock_user(&conn, &root_key, carol.id, Some("carol-pass")).expect("unlock carol");
        let lazy = migrate_credentials_for_user(&conn, &store, &root_key, carol.id).expect("lazy");
        assert_eq!(
            count_prefix(&conn, carol.id, "server_"),
            10,
            "carol migrated lazily"
        );
        println!("[lazy] OK: carol migrated {lazy} rows at first unlock");

        // --- R-MUV-5: MCP profile resolution per active user ------------------
        set_active_user(&conn, alice.id).expect("active=alice"); // clears session
        let p_alice = mcp_list_active_server_profiles(&store).expect("mcp alice");
        assert_eq!(p_alice.len(), 10, "MCP auto-resolves a device-wrapped user");

        set_active_user(&conn, carol.id).expect("active=carol"); // clears session, carol locked
        std::env::remove_var("AEROFTP_USER_PASSPHRASE");
        let p_carol_noenv = mcp_list_active_server_profiles(&store);
        assert!(
            p_carol_noenv.is_err(),
            "locked passphrase user without env fails clean (no silent leak)"
        );
        std::env::set_var("AEROFTP_USER_PASSPHRASE", "carol-pass");
        let p_carol = mcp_list_active_server_profiles(&store).expect("mcp carol via env");
        assert_eq!(
            p_carol.len(),
            10,
            "MCP transient-unlocks a passphrase user via env"
        );
        std::env::remove_var("AEROFTP_USER_PASSPHRASE");
        println!("[R-MUV-5] OK: device-wrapped auto; passphrase via AEROFTP_USER_PASSPHRASE; clean fail without");

        // --- R-MUV-3: transport DEK round-trip (store-backed) -----------------
        let export = export_transport_deks(&conn, &root_key, "transport-pw-12345")
            .expect("export")
            .expect("transport section present");
        let root_b = [0x42u8; 32]; // a different machine's local root key
        let report = rekey_transport_deks(
            &conn,
            &root_b,
            "transport-pw-12345",
            &export.salt,
            &export.kdf_params,
            &export.wrapped_deks,
        )
        .expect("rekey");
        assert!(report.rekeyed >= 1, "at least bob's DEK was rekeyed");
        assert_eq!(
            get_user_credential_for(&conn, &root_b, bob.id, "server_b1")
                .expect("machine B read")
                .expect("present")
                .as_str(),
            "secret-b1",
            "secret decrypts on the second machine after rekey"
        );
        assert!(
            get_user_credential_for(&conn, &root_key, bob.id, "server_b1").is_err(),
            "the original machine root key no longer unwraps after rekey"
        );
        println!(
            "[R-MUV-3] OK: rekeyed {} DEK(s); secrets portable cross-machine",
            report.rekeyed
        );

        println!("[MUV e2e] ALL SCENARIOS PASSED");

        // Restore env (best-effort; the tempdir auto-cleans on drop).
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        if let Some(v) = prev_pp {
            std::env::set_var("AEROFTP_USER_PASSPHRASE", v);
        }
    }

    #[test]
    fn upgrade_v4_to_v5_stamps_version_and_preserves_users() {
        // Regression guard for the AeroShare v4->v5 branch: a stored-v4 database
        // (which already has users) must upgrade to v5 by ONLY stamping the version.
        // The P2P secret-store tables are additive (created by init_db_schema on
        // open), so the upgrade must NOT re-insert a default user or touch existing
        // partitions (which a fall-through to migrate_legacy_payloads would cause).
        let _guard = test_lock();
        let mut conn = Connection::open_in_memory().expect("memory db");
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE users (
                 id                INTEGER PRIMARY KEY AUTOINCREMENT,
                 name              TEXT NOT NULL,
                 name_canonical    TEXT NOT NULL UNIQUE,
                 avatar_emoji      TEXT,
                 avatar_color      TEXT,
                 has_passphrase    INTEGER NOT NULL DEFAULT 0,
                 kdf_salt          BLOB,
                 kdf_params        TEXT,
                 wrapped_dek       BLOB NOT NULL,
                 dek_verifier      BLOB NOT NULL,
                 sort_order        INTEGER NOT NULL DEFAULT 0,
                 created_at        INTEGER NOT NULL,
                 updated_at        INTEGER NOT NULL,
                 last_unlocked_at  INTEGER,
                 is_admin          INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE global_state (
                 key             TEXT PRIMARY KEY,
                 value           TEXT NOT NULL,
                 updated_at      INTEGER NOT NULL
             );",
        )
        .expect("v4 schema");
        let now = now_ms();
        conn.execute(
            "INSERT INTO users(name, name_canonical, wrapped_dek, dek_verifier, sort_order, created_at, updated_at, is_admin)
             VALUES ('default', 'default', X'00', X'00', 0, ?1, ?1, 1)",
            params![now],
        )
        .expect("seed default");
        conn.execute(
            "INSERT INTO users(name, name_canonical, wrapped_dek, dek_verifier, sort_order, created_at, updated_at, is_admin)
             VALUES ('alice', 'alice', X'00', X'00', 1, ?1, ?1, 0)",
            params![now],
        )
        .expect("seed alice");
        conn.execute(
            "INSERT INTO global_state(key, value, updated_at) VALUES (?1, '4', ?2)",
            params![SCHEMA_VERSION_KEY, now],
        )
        .expect("seed schema v4");

        let users_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .expect("count before");
        assert_eq!(users_before, 2);

        upgrade_v4_to_v5(&mut conn).expect("upgrade v4->v5");

        let version = current_schema_version(&conn).expect("version");
        assert_eq!(version.as_deref(), Some(SCHEMA_VERSION));
        let users_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .expect("count after");
        assert_eq!(
            users_after, 2,
            "v4->v5 must preserve users (no default re-insert)"
        );

        // Idempotent: rerunning must not crash nor change the user set.
        upgrade_v4_to_v5(&mut conn).expect("idempotent rerun");
        let users_final: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .expect("count final");
        assert_eq!(users_final, 2);
    }

    #[test]
    fn credential_free_profile_copies_replace_all_identity_aliases() {
        let original = vec![
            serde_json::json!({
                "id": "srv_source",
                "uid": "srv_source",
                "profileUid": "srv_source",
                "host": "one.example.test"
            }),
            serde_json::json!({"id": "srv_other", "host": "two.example.test"}),
        ];
        let copied = credential_free_profile_copies(&original);
        let ids: HashSet<String> = copied
            .iter()
            .map(|profile| {
                profile["id"]
                    .as_str()
                    .expect("fresh profile id")
                    .to_string()
            })
            .collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| id.starts_with("srv_")));
        assert!(!ids.contains("srv_source"));
        assert!(!ids.contains("srv_other"));

        for profile in &copied {
            let id = profile["id"].as_str().unwrap();
            for alias in ["uid", "profileUid"] {
                if profile.get(alias).is_some() {
                    assert_eq!(profile[alias].as_str(), Some(id));
                }
            }
        }
        assert_eq!(copied[0]["host"], "one.example.test");
        assert_eq!(copied[1]["host"], "two.example.test");
        assert_eq!(original[0]["id"], "srv_source");
        assert_eq!(original[1]["id"], "srv_other");
    }

    /// A credential-free copy carries no option-borne secret either: the
    /// rclone-crypt passwords, an API key, a key passphrase and the injected
    /// Proton session go with the copy's fresh id (#347).
    #[test]
    fn a_credential_free_copy_strips_option_borne_secrets() {
        let profiles = vec![json!({
            "id": "srv_source", "name": "Drive", "protocol": "s3",
            "options": {
                "rcloneCryptPassword": "crypt-one",
                "rcloneCryptPassword2": "crypt-two",
                "filen_api_key": "api-key",
                "key_passphrase": "key-pass",
                "proton_native_session": "session-blob",
                "region": "eu-west-1"
            }
        })];
        let copied = credential_free_profile_copies(&profiles);
        assert_ne!(copied[0]["id"], "srv_source");
        let options = copied[0]["options"].as_object().unwrap();
        for secret in [
            "rcloneCryptPassword",
            "rcloneCryptPassword2",
            "filen_api_key",
            "key_passphrase",
            "proton_native_session",
        ] {
            assert!(!options.contains_key(secret), "{secret} survived the copy");
        }
        assert_eq!(options["region"], "eu-west-1");
        // A copy whose options held secrets only keeps no options object.
        let bare = credential_free_profile_copies(&[json!({
            "id": "srv_bare", "options": {"rcloneCryptPassword": "crypt-one"}
        })]);
        assert!(bare[0].get("options").is_none());
    }
}
