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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::AppHandle;

const DB_FILENAME: &str = "user_partitions.db";
const SCHEMA_VERSION: &str = "5";
const LEGACY_PROFILES_KEY: &str = "__legacy_server_profiles";
const LEGACY_SETTINGS_KEY: &str = "__legacy_settings";
const ACTIVE_USER_KEY: &str = "active_user_id";
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
pub struct PartitionDebugState {
    pub db_path: String,
    pub schema_version: Option<String>,
    pub active_user_id: Option<i64>,
    pub user_count: i64,
    pub profile_count: i64,
    pub settings_count: i64,
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
        conn.execute(
            "ALTER TABLE users ADD COLUMN is_admin INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| format!("ALTER TABLE users ADD is_admin: {e}"))?;
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
        conn.execute(
            "ALTER TABLE users ADD COLUMN is_default INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| format!("ALTER TABLE users ADD is_default: {e}"))?;
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
             UNIQUE(user_id, profile_uid)
         );

         CREATE INDEX IF NOT EXISTS idx_profiles_user
             ON server_profiles(user_id);

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
         );",
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

fn clear_user_session() {
    if let Ok(mut session) = USER_SESSION.lock() {
        *session = None;
    }
}

fn set_user_session(user_id: i64, dek: SecretKey) -> Result<(), String> {
    let mut session = USER_SESSION
        .lock()
        .map_err(|_| "USER_SESSION_LOCK_POISONED".to_string())?;
    *session = Some(UserSession { user_id, dek });
    Ok(())
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

fn active_user_id(conn: &Connection) -> Result<Option<i64>, String> {
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

fn upsert_global_state(
    tx: &Transaction<'_>,
    key: &str,
    value: &str,
    updated_at: i64,
) -> Result<(), String> {
    tx.execute(
        "INSERT INTO global_state(key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
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
fn relocate_credential_key_candidates(protocol: &str, profile_id: &str) -> Vec<String> {
    let mut keys = vec![format!("server_{profile_id}")];
    if let Some(oauth_base) = oauth_vault_key_for_protocol(protocol) {
        // oauth_vault_key_for_protocol returns e.g. "oauth_pcloud" or
        // "jottacloud_refresh"; append the profile id for the per-profile key.
        keys.push(format!("{oauth_base}_{profile_id}"));
    }
    keys
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
    let candidates = relocate_credential_key_candidates(protocol, profile_id);
    for key in &candidates {
        let from_partition = if let Some(dek) = user_dek {
            read_credential_with_dek(conn, user_id, dek, key)?
        } else {
            // Session / device-wrapped path for the source (active) user.
            match get_user_credential_for(conn, root_key, user_id, key) {
                Ok(v) => v,
                // A locked passphrase target without an explicit DEK is not a
                // fatal relocate error here: fall through to the vault.
                Err(e) if e == "USER_LOCKED" => None,
                Err(e) => return Err(e),
            }
        };
        if let Some(secret) = from_partition {
            if !secret.is_empty() {
                return Ok(Some(secret));
            }
        }
        // Dual-write fallback: vault still holds secrets not yet mirrored.
        if let Some(store) = CredentialStore::from_cache() {
            if let Ok(secret) = store.get_secret(key) {
                if !secret.is_empty() {
                    return Ok(Some(secret));
                }
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
        let key = profile_dedup_key(&root_secret, profile, &uid_seed)?;
        let (encrypted_blob, nonce) = encrypt_value(&default_dek, profile)?;
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO server_profiles(
                 user_id, profile_uid, dedup_key, name, encrypted_blob, nonce,
                 aead_alg, created_at, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'aes-256-gcm', ?7, ?7)",
                params![default_user_id, uid, key, uid, encrypted_blob, nonce, now],
            )
            .map_err(|e| format!("Migrate legacy profile: {e}"))?;
        migrated_profiles += inserted;
    }

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
        tx.execute(
            "ALTER TABLE users ADD COLUMN is_admin INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| format!("ALTER TABLE users ADD is_admin: {e}"))?;
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
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
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
    let row = read_user_key_row(conn, user_id)?;
    if row.has_passphrase {
        let session = USER_SESSION
            .lock()
            .map_err(|_| "USER_SESSION_LOCK_POISONED".to_string())?;
        let Some(session) = session
            .as_ref()
            .filter(|session| session.user_id == user_id)
        else {
            return Err("USER_LOCKED".to_string());
        };
        if !user_crypto::verify_dek(&session.dek, &row.dek_verifier)? {
            return Err("DEK_VERIFIER_MISMATCH".to_string());
        }
        return f(user_id, &session.dek);
    }

    let dek = unwrap_user_dek_with_key(&row, root_key)?;
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
    let row = read_user_key_row(conn, user_id)?;
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

pub fn replace_active_server_profiles(
    conn: &mut Connection,
    root_key: &[u8; 32],
    profiles: &[Value],
) -> Result<(), String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    replace_server_profiles_for(conn, root_key, user_id, profiles)
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

    let mut profiles = Vec::new();
    for row in rows {
        let (encrypted_blob, nonce) = row.map_err(|e| format!("Read profile row: {e}"))?;
        profiles.push(decrypt_value(dek, &nonce, &encrypted_blob)?);
    }
    Ok(profiles)
}

/// Overwrite server profiles for a specific user id without changing
/// active_user_id. Companion of [`list_server_profiles_for`] (MU-3).
pub fn replace_server_profiles_for(
    conn: &mut Connection,
    root_key: &[u8; 32],
    user_id: i64,
    profiles: &[Value],
) -> Result<(), String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_user_dek(conn, &root_secret, user_id, |user_id, dek| {
        write_profiles_with_dek(conn, &root_secret, user_id, dek, profiles)
    })
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
) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Start replace profiles: {e}"))?;
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
        let key = profile_dedup_key(root_secret, profile, &uid_seed)?;
        let (encrypted_blob, nonce) = encrypt_value(dek, profile)?;
        tx.execute(
            "INSERT INTO server_profiles(
                 user_id, profile_uid, dedup_key, name, encrypted_blob, nonce,
                 aead_alg, created_at, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'aes-256-gcm', ?7, ?7)",
            params![user_id, uid, key, uid, encrypted_blob, nonce, now],
        )
        .map_err(|e| format!("Insert profile: {e}"))?;
    }

    tx.commit()
        .map_err(|e| format!("Commit replace profiles: {e}"))?;
    Ok(())
}

/// Outcome of a cross-user profile relocation (N4). Returned to the GUI/CLI so
/// the caller can refresh state and finish credential bookkeeping outside the
/// partition DB (`server_<id>` lives in the OS keyring, not the SQLite file).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRelocation {
    /// Source profile id, as it existed in the source partition.
    pub source_profile_id: String,
    /// Fresh id the relocated copy received in the target partition. Equal to
    /// `source_profile_id` only when the caller deliberately reuses it.
    pub new_profile_id: String,
    /// Display name of the relocated profile (for the confirmation toast).
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
}

/// Copy or move a single server profile from `source_user_id` into
/// `target_user_id` (N4, Ehud wishlist #270). The backend stays authoritative:
/// the caller passes only an id, the genuine source blob is read from the
/// vault, re-keyed under `new_profile_id`, and inserted into the target
/// partition. Credentials (`server_<id>`) live outside the partition DB and are
/// handled by the caller using the returned ids.
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
    if source_user_id == target_user_id {
        return Err("RELOCATE_SAME_USER".to_string());
    }
    if new_profile_id.trim().is_empty() {
        return Err("NEW_PROFILE_ID_REQUIRED".to_string());
    }
    let root_secret = user_crypto::secret_key_from_bytes(root_key);

    // The target must exist before we read the (potentially large) source list.
    read_user_key_row(conn, target_user_id)?;

    // 1. Read the source (active, already-unlocked) partition via its session.
    let source_profiles = list_server_profiles_for(conn, root_key, source_user_id)?;
    let source = source_profiles
        .iter()
        .find(|p| value_str(p, &["id", "uid", "profileUid"]) == Some(profile_id))
        .cloned()
        .ok_or_else(|| format!("PROFILE_NOT_FOUND: {profile_id}"))?;
    let profile_name = value_str(&source, &["name", "label", "host", "hostname"])
        .unwrap_or(profile_id)
        .to_string();
    let protocol = value_str(&source, &["protocol"]).unwrap_or("").to_string();

    // 2. Clone the blob under a fresh id and drop the per-account session field.
    let mut relocated = source.clone();
    if let Value::Object(map) = &mut relocated {
        map.insert("id".into(), Value::String(new_profile_id.to_string()));
        map.remove("lastConnected");
    }

    // 3. Resolve the target DEK with its own passphrase, never via the session.
    let target_dek =
        resolve_user_dek_scoped(conn, &root_secret, target_user_id, target_passphrase)?;

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
        &root_secret,
        source_user_id,
        None,
        &source,
        source_seed,
    )?;
    let target_profiles = read_profiles_with_dek(conn, target_user_id, &target_dek)?;
    let mut already_present = false;
    for existing in &target_profiles {
        let seed = value_str(existing, &["id", "uid", "profileUid"]).unwrap_or("");
        let existing_fp = relocate_credential_fingerprint(
            conn,
            root_key,
            &root_secret,
            target_user_id,
            Some(&target_dek),
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
        write_profiles_with_dek(conn, &root_secret, target_user_id, &target_dek, &new_list)?;
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
        replace_server_profiles_for(conn, root_key, source_user_id, &remaining)?;
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
    })
}

/// Partition credential_type for a relocate key (migrate precedent at
/// `copy_user_secrets_with_dek`: `server` / `oauth` / `jottacloud_refresh`).
fn relocate_secret_kind(credential_key: &str) -> &'static str {
    if credential_key.starts_with("server_") {
        "server"
    } else if credential_key.starts_with("jottacloud_refresh_") {
        "jottacloud_refresh"
    } else if credential_key.starts_with("oauth_") {
        "oauth"
    } else {
        // relocate_credential_key_candidates only emits the prefixes above.
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
) {
    // Copy onto the new id only when a fresh target row was inserted; a dedup
    // no-op (Copy into a target that already has the drive) leaves the target's
    // existing secret untouched. A Move always inserts (#366), so its secret
    // always follows the profile into the target before the source is dropped.
    if relocation.inserted {
        let secret = match store {
            Some(store) => {
                read_credential_with_fallback(conn, store, root_key, source_user_id, source_key)
                    .ok()
                    .flatten()
            }
            None => get_user_credential_for(conn, root_key, source_user_id, source_key)
                .ok()
                .flatten(),
        };
        if let Some(secret) = secret {
            // Vault stays in sync (source of truth + fallback + downgrade safe).
            if let Some(store) = store {
                let _ = store.store(new_key, &secret);
            }
            // Mirror onto the target partition under its scoped DEK.
            let root_secret = user_crypto::secret_key_from_bytes(root_key);
            if let Ok(target_dek) = resolve_user_dek_scoped(
                conn,
                &root_secret,
                relocation.target_user_id,
                target_passphrase,
            ) {
                let _ = set_user_credential_with_dek(
                    conn,
                    relocation.target_user_id,
                    &target_dek,
                    new_key,
                    kind,
                    &secret,
                );
            }
        }
    }

    // Move/Cut: the source profile row is gone, so its orphaned secret is
    // removed from both the vault and the source partition.
    if relocation.moved {
        if let Some(store) = store {
            let _ = store.delete(source_key);
        }
        let _ = delete_user_credential_for(conn, source_user_id, source_key);
    }
}

/// MUV-3 cross-user credential relocation: the partition profile row is already
/// moved by [`relocate_server_profile`]; this carries every per-profile secret
/// resolved by [`relocate_credential_key_candidates`] (`server_<id>` plus
/// OAuth / Jottacloud when the protocol has a vault base). The vault stays the
/// source of truth (copy onto the new id, drop the orphan on a Move), and each
/// secret is mirrored onto the TARGET user's partition under its own scoped DEK
/// (resolved with `target_passphrase`, never the source session). Best-effort on
/// the partition mirror: a locked/uncovered target falls back to the dual-written
/// vault. Caller must invoke this while `root_key` / `target_passphrase` are
/// still live (before zeroize).
///
/// #366: for every key, `inserted` gates copy before `moved` gates delete.
fn relocate_server_credential_dual(
    conn: &Connection,
    store: &CredentialStore,
    root_key: &[u8; 32],
    source_user_id: i64,
    relocation: &ProfileRelocation,
    target_passphrase: Option<&str>,
) {
    let source_keys =
        relocate_credential_key_candidates(&relocation.protocol, &relocation.source_profile_id);
    let new_keys =
        relocate_credential_key_candidates(&relocation.protocol, &relocation.new_profile_id);
    for (source_key, new_key) in source_keys.iter().zip(new_keys.iter()) {
        relocate_secret_key_dual(
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
    }
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

pub fn list_user_setting_scopes_for(
    conn: &Connection,
    user_id: i64,
) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT scope FROM user_settings
             WHERE user_id = ?1 ORDER BY scope ASC",
        )
        .map_err(|e| format!("Prepare list user settings: {e}"))?;
    let rows = stmt
        .query_map(params![user_id], |row| row.get::<_, String>(0))
        .map_err(|e| format!("Query list user settings: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("Read user setting scopes: {e}"))
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

pub fn list_active_user_setting_scopes(conn: &Connection) -> Result<Vec<String>, String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    list_user_setting_scopes_for(conn, user_id)
}

// --- MUV-1: per-user credentials (raw secrets under the user DEK) -----------
//
// Companion of `user_settings`, but for raw secrets (server passwords, OAuth
// token blobs, API keys, PEM). Secrets are updated one at a time (an OAuth
// refresh rewrites a single key), so the storage model is upsert-per-key with a
// composite primary key `(user_id, credential_id)`, NOT the delete-all + insert
// pattern used for `server_profiles`. The secret is encrypted with the user's
// DEK exactly like a profile blob; only the active user (or a primed session
// for a passphrase account) can read or write its own credentials. MUV-1 builds
// the store only: no existing caller is rewired and nothing is migrated yet
// (that is MUV-2..6).

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

/// Active-user wrapper for [`get_user_credential_for`].
pub fn get_active_user_credential(
    conn: &Connection,
    root_key: &[u8; 32],
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    get_user_credential_for(conn, root_key, user_id, credential_id)
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

fn creds_migrated_key(user_id: i64) -> String {
    format!("{CREDS_MIGRATED_PREFIX}{user_id}")
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

/// Owner of the non-profile per-user globals (`ai_apikey_*`, github tokens):
/// the lowest-id admin (the legacy `default`). Routing them to a single account
/// avoids duplicating them across every admin (MUV-0 / sec 2.3).
fn nonprofile_secret_owner(conn: &Connection) -> Result<Option<i64>, String> {
    conn.query_row(
        "SELECT id FROM users WHERE is_admin = 1 ORDER BY id ASC LIMIT 1",
        [],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map_err(|e| format!("Read nonprofile secret owner: {e}"))
}

/// Read one optional secret from the live store, mapping NotFound to None.
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
/// deliberately NOT migrated.
fn copy_user_secrets_with_dek(
    conn: &Connection,
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

    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("Start credential migration: {e}"))?;
    let mut migrated = 0usize;

    for id in &profile_ids {
        migrated += copy_one_credential(
            &tx,
            user_id,
            dek,
            &format!("server_{id}"),
            "server",
            read_secret,
        )?;
        migrated += copy_one_credential(
            &tx,
            user_id,
            dek,
            &format!("jottacloud_refresh_{id}"),
            "jottacloud_refresh",
            read_secret,
        )?;
        let suffix = format!("_{id}");
        for key in vault_keys
            .iter()
            .filter(|k| k.starts_with("oauth_") && k.ends_with(&suffix))
        {
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
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    let outcome = with_user_dek(conn, &root_secret, user_id, |uid, dek| {
        copy_user_secrets_with_dek(conn, uid, dek, owns_global_secrets, vault_keys, read_secret)
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

/// Reader-fallback net (sec 3.3), engine form: prefer the per-user encrypted
/// store, fall back to the legacy vault when the per-user row is absent or the
/// account is locked. MUV-3/4/5 point real readers here; MUV-6 drops the
/// fallback once the vault is purged.
fn read_credential_with_fallback_inner(
    conn: &Connection,
    root_key: &[u8; 32],
    user_id: i64,
    credential_id: &str,
    read_secret: &SecretReader<'_>,
) -> Result<Option<Zeroizing<String>>, String> {
    match get_user_credential_for(conn, root_key, user_id, credential_id) {
        Ok(Some(value)) => return Ok(Some(value)),
        Ok(None) => {}
        // Locked passphrase account: the per-user row is unreadable right now,
        // but the legacy vault copy still is. Fall back instead of failing.
        Err(e) if e == "USER_LOCKED" => {}
        Err(e) => return Err(e),
    }
    read_secret(credential_id)
}

/// True if any user can be migrated eagerly right now (device-wrapped, or a
/// passphrase user already in session) and is not yet migrated. Cheap pre-check
/// so the per-command boot path skips the vault read once everything reachable
/// is done; a locked passphrase user is intentionally NOT "eager-pending".
fn has_eager_pending_users(conn: &Connection) -> Result<bool, String> {
    for user in list_users(conn)? {
        if is_creds_migrated(conn, user.id)? {
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
    let mut pending = Vec::new();
    for user in &users {
        if is_creds_migrated(conn, user.id)? {
            continue;
        }
        if !user.has_passphrase || session_user_id() == Some(user.id) {
            pending.push(user.id);
        }
    }
    if pending.is_empty() {
        return Ok(0);
    }
    let vault_keys = store
        .list_accounts()
        .map_err(|e| format!("List vault accounts: {e}"))?;
    let read_secret = |key: &str| get_optional_secret(store, key);
    let mut total = 0usize;
    for user_id in pending {
        match migrate_user_credentials_inner(conn, root_key, user_id, &vault_keys, &read_secret) {
            Ok(migrated) => total += migrated,
            // Leave unmarked; a transient failure is retried on the next boot.
            Err(_) => continue,
        }
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
    if is_creds_migrated(conn, user_id)? {
        return Ok(0);
    }
    let vault_keys = store
        .list_accounts()
        .map_err(|e| format!("List vault accounts: {e}"))?;
    let read_secret = |key: &str| get_optional_secret(store, key);
    migrate_user_credentials_inner(conn, root_key, user_id, &vault_keys, &read_secret)
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
// vault (a not-yet-migrated key or a locked passphrase account still resolves);
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
/// no `_client_id`-style sibling, so prefix matching never misfires. Everything
/// else is `None`: OAuth tokens (`oauth_<p>_<id>` vs `oauth_<p>_client_id`) and
/// the GitHub tokens are mirrored by call-sites that pass the type explicitly
/// (MUV-4/5), and the machine-global `github_pem_*` / `github_app_credentials`
/// stay vault-only on purpose (MUV-0).
fn muv3_credential_type(credential_id: &str) -> Option<&'static str> {
    if credential_id.starts_with("server_") {
        Some("server")
    } else if credential_id.starts_with("ai_apikey_") {
        Some("ai_apikey")
    } else {
        None
    }
}

/// Active user id resolved quietly without an AppHandle. Returns `None` on any
/// error (DB unopenable, no active user): callers use it only to decide whether
/// a best-effort mirror is possible, never to gate the authoritative vault write.
fn active_user_id_quiet() -> Option<i64> {
    let conn = open_or_init_cli().ok()?;
    active_user_id(&conn).ok().flatten()
}

/// Best-effort mirror of one secret into `user_id`'s partition. Confined to
/// in-scope keys, swallows `USER_LOCKED` (a locked passphrase account keeps only
/// the vault copy; the MUV-2 lazy pass mirrors it at the next unlock) and any
/// store/DB error: the vault copy written by the caller is authoritative, so a
/// failed mirror must never fail the user's save. Zero-password safe (a
/// device-wrapped account mirrors via the keyring-derived root_key, no prompt).
fn mirror_credential_for_user_best_effort(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    secret: &str,
) {
    let Some(credential_type) = muv3_credential_type(credential_id) else {
        return;
    };
    mirror_credential_for_user_typed(store, user_id, credential_id, credential_type, secret);
}

/// Best-effort mirror with an EXPLICIT credential type. Used by call-sites that
/// already know the type (OAuth tokens, Jottacloud refresh) and therefore
/// bypass the prefix classifier, which cannot reliably tell `oauth_<p>_<id>`
/// (a per-user token) from `oauth_<p>_client_id` (machine/app config). Same
/// best-effort + zero-password semantics as the classifier-gated variant.
fn mirror_credential_for_user_typed(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) {
    let Ok(conn) = open_or_init_cli() else {
        return;
    };
    let mut root_key = store.derive_user_partition_wrapping_key();
    let _ = set_user_credential_for(
        &conn,
        &root_key,
        user_id,
        credential_id,
        credential_type,
        secret,
    );
    root_key.zeroize();
}

/// Best-effort removal of one in-scope secret from `user_id`'s partition. No DEK
/// is needed (DELETE by key); errors are swallowed because the vault delete is
/// the part that matters during the dual-write rollout. This is the partition
/// half of an explicit user delete, never a mass purge (that is MUV-6).
fn unmirror_credential_for_user_best_effort(user_id: i64, credential_id: &str) {
    if muv3_credential_type(credential_id).is_none() {
        return;
    }
    unmirror_credential_for_user_best_effort_any(user_id, credential_id);
}

/// Like [`unmirror_credential_for_user_best_effort`] but without the prefix
/// classifier gate. For explicitly-typed call-sites (OAuth/Jottacloud, MUV-4)
/// that already know the key is in scope.
pub fn unmirror_credential_for_user_best_effort_any(user_id: i64, credential_id: &str) {
    if let Ok(conn) = open_or_init_cli() {
        let _ = delete_user_credential_for(&conn, user_id, credential_id);
    }
}

/// Active-user reader (GUI + shared modules: factory/agent/ai_tools/mcp/AI core).
/// Resolves `credential_id` for the active user from the per-user store, falling
/// back to the legacy vault when the row is absent or the account is locked.
/// With no active user (pre-MU / fresh install) it reads the vault only.
pub fn resolve_active_credential(
    store: &CredentialStore,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    match active_user_id(&conn)? {
        None => get_optional_secret(store, credential_id),
        Some(user_id) => {
            let mut root_key = store.derive_user_partition_wrapping_key();
            let result =
                read_credential_with_fallback(&conn, store, &root_key, user_id, credential_id);
            root_key.zeroize();
            result
        }
    }
}

/// Active-user dual writer (GUI): write the vault first, then mirror in-scope
/// keys into the active user's partition. Out-of-scope keys (oauth/ai/github)
/// land in the vault only, which is the correct MUV-3 behaviour for the generic
/// `store_credential` command.
pub fn store_active_credential_dual(
    store: &CredentialStore,
    credential_id: &str,
    secret: &str,
) -> Result<(), String> {
    store
        .store(credential_id, secret)
        .map_err(|e| format!("Store credential in vault: {e}"))?;
    if muv3_credential_type(credential_id).is_some() {
        if let Some(user_id) = active_user_id_quiet() {
            mirror_credential_for_user_best_effort(store, user_id, credential_id, secret);
        }
    }
    Ok(())
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
    store
        .store(credential_id, secret)
        .map_err(|e| format!("Store credential in vault: {e}"))?;
    mirror_credential_for_user_best_effort(store, user_id, credential_id, secret);
    Ok(())
}

/// Active-user dual delete (GUI): delete from the vault and best-effort from the
/// active user's partition. Part of an explicit user delete, never a purge.
pub fn delete_active_credential_dual(
    store: &CredentialStore,
    credential_id: &str,
) -> Result<(), String> {
    store
        .delete(credential_id)
        .map_err(|e| format!("Delete credential from vault: {e}"))?;
    if muv3_credential_type(credential_id).is_some() {
        if let Some(user_id) = active_user_id_quiet() {
            unmirror_credential_for_user_best_effort(user_id, credential_id);
        }
    }
    Ok(())
}

/// Explicit-user dual delete (CLI `--user`): vault delete + best-effort removal
/// from `user_id`'s partition.
pub fn delete_credential_for_user_dual(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<(), String> {
    store
        .delete(credential_id)
        .map_err(|e| format!("Delete credential from vault: {e}"))?;
    unmirror_credential_for_user_best_effort(user_id, credential_id);
    Ok(())
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

/// Best-effort mirror of an explicitly-typed secret into the active user's
/// partition. For call-sites with bespoke vault-write logic (OAuth runtime,
/// Jottacloud persist) that have already written the vault and only need the
/// partition mirror. No active user / locked / error -> no-op.
pub fn mirror_active_credential(
    store: &CredentialStore,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) {
    if let Some(user_id) = active_user_id_quiet() {
        mirror_credential_for_user_typed(store, user_id, credential_id, credential_type, secret);
    }
}

/// Best-effort removal of one secret from the active user's partition. The
/// partition half of an explicit token delete (OAuth/Jottacloud logout).
pub fn unmirror_active_credential(credential_id: &str) {
    if let Some(user_id) = active_user_id_quiet() {
        unmirror_credential_for_user_best_effort_any(user_id, credential_id);
    }
}

/// Explicitly-typed dual writer (active user): vault first, then mirror into the
/// active user's partition. For import/bridge sites without bespoke vault logic.
pub fn store_active_credential_typed_dual(
    store: &CredentialStore,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    store
        .store(credential_id, secret)
        .map_err(|e| format!("Store credential in vault: {e}"))?;
    mirror_active_credential(store, credential_id, credential_type, secret);
    Ok(())
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
    store
        .store(credential_id, secret)
        .map_err(|e| format!("Store credential in vault: {e}"))?;
    mirror_credential_for_user_typed(store, user_id, credential_id, credential_type, secret);
    Ok(())
}

/// MU-7: list other users that already store a profile with the SAME dedup
/// signature (HMAC of canonical protocol/host/user/port keyed by the device
/// partition root key). Used by the frontend to warn before adding a profile
/// that another account already has. Excludes the requesting user. Returns
/// only public user metadata, never any portion of the encrypted blob.
pub fn cross_user_dedup_matches(
    conn: &Connection,
    root_key: &[u8; 32],
    requesting_user_id: i64,
    profile: &Value,
) -> Result<Vec<CrossUserDedupMatch>, String> {
    // EF-19(a): an empty S3 access key hashes to a constant dedup_key, so two
    // unrelated keyless S3 accounts would cross-warn. Ambiguous surface → no
    // warning (conservative: this guards the soft warning only; storage
    // aggregation semantics are unchanged — see storage_dedup::dedup_key).
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
    if row.has_passphrase {
        set_user_session(user_id, dek)?;
    }
    user_unlock_status(conn)
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
        "UPDATE users
         SET has_passphrase = ?1, kdf_salt = ?2, kdf_params = ?3,
             wrapped_dek = ?4, updated_at = ?5
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
/// lockout state and issue a brand-new DEK wrapped by the new
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
                 updated_at = ?6
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
    if make_default {
        let has_passphrase: Option<i64> = conn
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
    }
    let now = now_ms();
    let tx = conn
        .transaction()
        .map_err(|e| format!("Start set default user: {e}"))?;
    if make_default {
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

pub fn debug_state(app: &AppHandle) -> Result<PartitionDebugState, String> {
    let path = db_path(app)?;
    let conn = open_or_init(app)?;
    let active_user_id = active_user_id(&conn)?;
    let schema_version = current_schema_version(&conn)?;
    let user_count = conn
        .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
        .map_err(|e| format!("Count users: {e}"))?;
    let profile_count = conn
        .query_row("SELECT COUNT(*) FROM server_profiles", [], |row| row.get(0))
        .map_err(|e| format!("Count profiles: {e}"))?;
    let settings_count = conn
        .query_row("SELECT COUNT(*) FROM user_settings", [], |row| row.get(0))
        .map_err(|e| format!("Count settings: {e}"))?;

    Ok(PartitionDebugState {
        db_path: path.display().to_string(),
        schema_version,
        active_user_id,
        user_count,
        profile_count,
        settings_count,
    })
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
/// user exists (uncommon — migration always creates `default`).
pub fn cli_list_active_server_profiles(store: &CredentialStore) -> Result<Vec<Value>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = list_active_server_profiles(&conn, &root_key);
    root_key.zeroize();
    result
}

/// Overwrite the active user's server profiles in the per-user partition.
///
/// Same locking semantics as [`cli_list_active_server_profiles`].
pub fn cli_replace_active_server_profiles(
    store: &CredentialStore,
    profiles: &[Value],
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let mut conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = replace_active_server_profiles(&mut conn, &root_key, profiles);
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

/// Companion writer for [`cli_list_server_profiles_for_user`]. Replaces the
/// target user's profile rows without changing `active_user_id`.
pub fn cli_replace_server_profiles_for_user(
    store: &CredentialStore,
    user_id: i64,
    profiles: &[Value],
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let mut conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = replace_server_profiles_for(&mut conn, &root_key, user_id, profiles);
    root_key.zeroize();
    result
}

/// CLI counterpart of [`user_partitions_get_active_setting`]: read a per-user
/// `user_settings` scope for the active user, decrypted as JSON. Returns
/// `Ok(None)` when no row exists. Used by the CLI to keep low-stakes per-user
/// state (server groups, favourites) in the active user's partition instead of
/// the single global vault blob. `__`-reserved scopes are rejected.
pub fn cli_get_active_setting(
    store: &CredentialStore,
    scope: &str,
) -> Result<Option<Value>, String> {
    if scope.starts_with("__") {
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
    if scope.starts_with("__") {
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
/// `__`-reserved scopes (same rule as the active wrappers). Returns `Ok(None)`
/// when no row exists for that (user, scope). A passphrase-protected user that
/// is not the active session yields `Err("USER_LOCKED")`; the caller decides
/// how to surface it.
pub fn cli_get_user_setting(
    store: &CredentialStore,
    user_id: i64,
    scope: &str,
) -> Result<Option<Value>, String> {
    if scope.starts_with("__") {
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
    if scope.starts_with("__") {
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
/// Falls back to the dual-maintained legacy blob only when the partition cannot
/// serve the active user: no active user, a locked passphrase account with no
/// usable env passphrase, or a partition error (downgrade). The fallback keeps
/// MCP working during the rollout; MUV-6 drops it once the vault is purged.
pub fn mcp_list_active_server_profiles(store: &CredentialStore) -> Result<Vec<Value>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let active = match get_active_user(&conn)? {
        Some(user) => user,
        None => return read_legacy_server_profiles_blob(store),
    };

    // Unlock a passphrase-protected active user transiently from the env so the
    // session DEK is primed for both this profile read and later credential
    // reads. Best-effort: a wrong/absent passphrase just leaves the account
    // locked and the legacy fallback below answers.
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
    let result = list_active_server_profiles(&conn, &root_key);
    root_key.zeroize();
    match result {
        Ok(profiles) => Ok(profiles),
        // Locked passphrase account (no usable env passphrase) or no active
        // user: the partition cannot serve, so the dual-written legacy blob does.
        Err(e) if e == "USER_LOCKED" || e == "NO_ACTIVE_USER" => {
            read_legacy_server_profiles_blob(store)
        }
        Err(e) => Err(e),
    }
}

/// Read and parse the legacy single-user `config_server_profiles` vault blob.
/// The MUV-5 rollout fallback for [`mcp_list_active_server_profiles`]; removed
/// with the rest of the legacy reads at MUV-6.
fn read_legacy_server_profiles_blob(store: &CredentialStore) -> Result<Vec<Value>, String> {
    let raw = store
        .get("config_server_profiles")
        .map_err(|e| format!("Failed to read profiles: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("Failed to parse profiles: {e}"))
}

/// CLI bridge: read one secret from a user's partition (MUV-1). MUV-3 will wire
/// the CLI's credential resolution onto this; for now it is the binary the
/// later cutover slices call. Same locking semantics as the profile readers.
pub fn cli_get_user_credential(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<Option<Zeroizing<String>>, String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = get_user_credential_for(&conn, &root_key, user_id, credential_id);
    root_key.zeroize();
    result
}

/// CLI bridge: upsert one secret into a user's partition (MUV-1).
pub fn cli_set_user_credential(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
    credential_type: &str,
    secret: &str,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = set_user_credential_for(
        &conn,
        &root_key,
        user_id,
        credential_id,
        credential_type,
        secret,
    );
    root_key.zeroize();
    result
}

/// CLI bridge: delete one secret from a user's partition (MUV-1).
pub fn cli_delete_user_credential(
    store: &CredentialStore,
    user_id: i64,
    credential_id: &str,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    delete_user_credential_for(&conn, user_id, credential_id)
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
/// is protected) is supplied here. Credentials (`server_<id>`) are copied or
/// removed by the CLI command itself using the returned [`ProfileRelocation`].
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
    let result = relocate_server_profile(
        &mut conn,
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

/// CLI bridge (MUV-3) for the credential half of a cross-user relocation: copies
/// every per-profile secret from [`relocate_credential_key_candidates`]
/// (`server_<id>` plus OAuth/Jottacloud when applicable) onto the new id (vault
/// + the target user's partition under its scoped DEK) and, on a Move, drops
/// the orphaned source secrets from both stores. Call after
/// [`cli_relocate_server_profile`] with the same `target_passphrase` still live.
pub fn cli_relocate_server_credential_dual(
    store: &CredentialStore,
    source_user_id: i64,
    relocation: &ProfileRelocation,
    target_passphrase: Option<&str>,
) -> Result<(), String> {
    init_or_migrate_cli(store)?;
    let conn = open_or_init_cli()?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    relocate_server_credential_dual(
        &conn,
        store,
        &root_key,
        source_user_id,
        relocation,
        target_passphrase,
    );
    root_key.zeroize();
    Ok(())
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
// stay opaque-bytes in/out — the peer-l0 crypto lives in `crate::peer`, the CLI handler orchestrates.

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
    let result = with_user_dek(&conn, &root_secret, user.id, |_uid, dek| {
        crate::peer_identity::load_drive(&conn, user.id, dek, namespace_id)
    });
    root_key.zeroize();
    result.map(|opt| opt.map(|key| (user.id, key)))
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
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    init_or_migrate(app)?;
    let conn = open_or_init(app)?;
    let user = get_active_user(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let root_secret = user_crypto::secret_key_from_bytes(&root_key);
    let result = with_user_dek(&conn, &root_secret, user.id, |_uid, dek| {
        crate::peer_identity::store_drive(&conn, user.id, dek, namespace_id, role, content_key)
    });
    root_key.zeroize();
    result?;
    Ok(user.id)
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

/// Rebuild the active partition from this device's legacy credential vault.
/// Snapshots and removes the current (possibly machine-bound, unreadable)
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

    if path.is_file() {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let backup = path.with_file_name(format!("{DB_FILENAME}.pre-rebuild-{stamp}.bak"));
        std::fs::copy(&path, &backup)
            .map_err(|e| format!("Snapshot user_partitions.db before rebuild: {e}"))?;
        report.backup_path = Some(backup.to_string_lossy().into_owned());
        // Drop any unlocked session, then remove the DB and its SQLite
        // sidecars. Connections are short-lived per command, so no open handle
        // blocks the removal (cross-platform safe).
        clear_user_session();
        std::fs::remove_file(&path)
            .map_err(|e| format!("Remove user_partitions.db for rebuild: {e}"))?;
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    // Rebuild from the legacy credential vault under the local root_key.
    let migration = init_or_migrate(&app)?;
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

#[tauri::command]
pub async fn user_partitions_get_active_user(
    app: AppHandle,
) -> Result<Option<UserMetadata>, String> {
    init_or_migrate(&app)?;
    let conn = open_or_init(&app)?;
    get_active_user(&conn)
}

#[tauri::command]
pub async fn user_partitions_load_active_server_profiles(
    app: AppHandle,
) -> Result<Vec<Value>, String> {
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let conn = open_or_init(&app)?;
    let result = list_active_server_profiles(&conn, &root_key);
    root_key.zeroize();
    result
}

#[tauri::command]
pub async fn user_partitions_save_active_server_profiles(
    app: AppHandle,
    profiles: Vec<Value>,
) -> Result<(), String> {
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let mut conn = open_or_init(&app)?;
    let result = replace_active_server_profiles(&mut conn, &root_key, &profiles);
    root_key.zeroize();
    result
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
    mut target_passphrase: Option<String>,
    move_profile: bool,
) -> Result<ProfileRelocation, String> {
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut conn = open_or_init(&app)?;
    let source_user_id = active_user_id(&conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let result = relocate_server_profile(
        &mut conn,
        &root_key,
        source_user_id,
        target_user_id,
        &profile_id,
        &new_profile_id,
        target_passphrase.as_deref(),
        move_profile,
    );
    // MUV-3/F4: relocate per-profile secrets (server_* + OAuth/Jottacloud) while
    // the root_key and the target passphrase are still live. The vault stays in
    // sync and each secret is mirrored onto the target partition under its DEK.
    let outcome = match result {
        Ok(relocation) => {
            relocate_server_credential_dual(
                &conn,
                &store,
                &root_key,
                source_user_id,
                &relocation,
                target_passphrase.as_deref(),
            );
            Ok(relocation)
        }
        Err(e) => Err(e),
    };
    root_key.zeroize();
    if let Some(passphrase) = target_passphrase.as_mut() {
        passphrase.zeroize();
    }
    outcome
}

/// Generic per-user setting access (MU-4 foundation). Settings are keyed by
/// scope (e.g. `aerosync_schedule`, `aerosync_profiles`) and encrypted with
/// the active user's DEK. Reserved scopes starting with `__` are blocked at
/// this boundary so the legacy backup payload cannot be overwritten through
/// the public API. Returns JSON null when no setting exists for that scope.
#[tauri::command]
pub async fn user_partitions_get_active_setting(
    app: AppHandle,
    scope: String,
) -> Result<Option<Value>, String> {
    if scope.starts_with("__") {
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
    if scope.starts_with("__") {
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
    if scope.starts_with("__") {
        return Err("USER_SETTING_RESERVED_SCOPE".to_string());
    }
    init_or_migrate(&app)?;
    let conn = open_or_init(&app)?;
    delete_active_user_setting(&conn, &scope)
}

#[tauri::command]
pub async fn user_partitions_list_active_setting_scopes(
    app: AppHandle,
) -> Result<Vec<String>, String> {
    init_or_migrate(&app)?;
    let conn = open_or_init(&app)?;
    let scopes = list_active_user_setting_scopes(&conn)?;
    Ok(scopes
        .into_iter()
        .filter(|s| !s.starts_with("__"))
        .collect())
}

/// MUV-1: read one secret from the active user's encrypted partition. Returns
/// JSON null when the credential does not exist. Errors with `USER_LOCKED` when
/// the active user is a passphrase account that has not been unlocked. The
/// secret crosses the IPC boundary as a plain string for the GUI to consume,
/// the same way profile blobs already do; in-process it stays zeroize-on-drop.
#[tauri::command]
pub async fn user_partitions_get_user_credential(
    app: AppHandle,
    credential_id: String,
) -> Result<Option<String>, String> {
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let conn = open_or_init(&app)?;
    let result = get_active_user_credential(&conn, &root_key, &credential_id);
    root_key.zeroize();
    Ok(result?.map(|secret| secret.to_string()))
}

/// MUV-1: upsert one secret into the active user's encrypted partition.
#[tauri::command]
pub async fn user_partitions_set_user_credential(
    app: AppHandle,
    credential_id: String,
    credential_type: String,
    mut secret: String,
) -> Result<(), String> {
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let conn = open_or_init(&app)?;
    let result =
        set_active_user_credential(&conn, &root_key, &credential_id, &credential_type, &secret);
    root_key.zeroize();
    secret.zeroize();
    result
}

/// MUV-1: delete one secret from the active user's encrypted partition.
#[tauri::command]
pub async fn user_partitions_delete_user_credential(
    app: AppHandle,
    credential_id: String,
) -> Result<(), String> {
    init_or_migrate(&app)?;
    let conn = open_or_init(&app)?;
    delete_active_user_credential(&conn, &credential_id)
}

/// MU-7: ask "is this profile already saved by another user account?". Used
/// by the SavedServers add/edit flow to surface a soft warning. Returns the
/// public metadata of every OTHER user with a matching dedup_key; intra-user
/// duplicates remain blocked by the existing dedup check (R11).
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
pub async fn user_partitions_set_active_user(app: AppHandle, user_id: i64) -> Result<(), String> {
    init_or_migrate(&app)?;
    let conn = open_or_init(&app)?;
    set_active_user(&conn, user_id)
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

#[tauri::command]
pub async fn user_partitions_debug_state(app: AppHandle) -> Result<PartitionDebugState, String> {
    debug_state(&app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use serde_json::json;
    use std::sync::{Mutex as TestMutex, MutexGuard};

    static USER_PARTITION_TEST_LOCK: TestMutex<()> = TestMutex::new(());

    fn test_lock() -> MutexGuard<'static, ()> {
        // Recover from poisoning so a single failing test does not cascade
        // into the rest of the suite as PoisonError.
        USER_PARTITION_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    fn test_root() -> [u8; 32] {
        [7u8; 32]
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
            relocate_credential_key_candidates(&relocation.protocol, &relocation.source_profile_id);
        let new_keys =
            relocate_credential_key_candidates(&relocation.protocol, &relocation.new_profile_id);
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
        // sftp: resolver returns no oauth base → only server_* is relocated;
        // no phantom oauth_* / jottacloud_refresh_* rows.
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
        let scopes = list_active_user_setting_scopes(&conn).expect("list");
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

        // list_active_user_setting_scopes is also scoped: switching back to
        // alice shows only her scopes plus any legacy backup row migrated
        // for her (none, since alice is a fresh user).
        set_active_user(&conn, alice.id).expect("switch alice 2");
        let alice_scopes = list_active_user_setting_scopes(&conn).expect("alice scopes");
        assert_eq!(alice_scopes, vec!["aerosync_schedule".to_string()]);
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
            ("github_pem_app_1", "-----PEM-----"),
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
        for absent in ["github_pem_app_1", "github_app_credentials", "totp_secret"] {
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
    fn reader_fallback_uses_vault_for_locked_passphrase_user() {
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let bob = create_user(&mut conn, &root, "Bob", None, None, Some("bobpass123"))
            .expect("create bob");
        clear_user_session(); // bob is locked: per-user read would be USER_LOCKED

        let (_keys, read) = fake_vault(&[("server_bp1", "vault-bob")]);
        // The locked per-user partition is skipped; the legacy vault still serves.
        assert_eq!(
            read_credential_with_fallback_inner(&conn, &root, bob.id, "server_bp1", &read)
                .expect("read")
                .expect("present")
                .as_str(),
            "vault-bob"
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
        // A never-mirrored OAuth token resolves via the vault fallback.
        assert_eq!(
            read_credential_with_fallback_inner(&conn, &root, user.id, "oauth_dropbox_99", &read)
                .expect("read vault-only oauth")
                .expect("present")
                .as_str(),
            "vault-token"
        );
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
}
