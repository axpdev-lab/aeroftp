//! Multi-user partition metadata store.
//!
//! `vault.db` remains the existing encrypted JSON credential vault. This
//! module owns the additive SQLite database used to partition profiles and
//! per-user settings without changing the legacy vault format in-place.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::credential_store::{CredentialError, CredentialStore};
use crate::storage_dedup::{dedup_key, ProfileView};
use crate::user_crypto::{self, SecretKey};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use secrecy::zeroize::Zeroize;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::AppHandle;

const DB_FILENAME: &str = "user_partitions.db";
const SCHEMA_VERSION: &str = "4";
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

pub fn init_db_schema(conn: &Connection) -> Result<(), String> {
    // Idempotent schema migration: if a pre-v3 `users` table already exists
    // without the is_admin column, add it before CREATE TABLE IF NOT EXISTS
    // becomes a no-op. Safe to run on fresh installs (PRAGMA returns 0 rows
    // until CREATE TABLE runs, so the check just skips).
    ensure_users_is_admin_column(conn)?;
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

fn insert_default_user(
    tx: &Transaction<'_>,
    root_key: &SecretKey,
    now: i64,
) -> Result<(i64, SecretKey), String> {
    let (name, canonical) = normalize_name(DEFAULT_USER_NAME)?;
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

    Ok((tx.last_insert_rowid(), dek))
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

    let tx = conn
        .transaction()
        .map_err(|e| format!("Start user partitions migration: {e}"))?;
    let now = now_ms();
    let (default_user_id, default_dek) = insert_default_user(&tx, &root_secret, now)?;
    let mut seen_uids = HashSet::new();
    let mut migrated_profiles = 0usize;

    for (index, profile) in legacy_profiles.iter().enumerate() {
        let uid_seed = profile_uid_seed(profile, index, &mut seen_uids);
        let uid = user_crypto::metadata_tag(&root_secret, b"profile-uid", &uid_seed)?;
        let key = profile_dedup_key(&root_secret, profile, &uid_seed)?;
        let (encrypted_blob, nonce) = encrypt_value(&default_dek, profile)?;
        tx.execute(
            "INSERT INTO server_profiles(
                 user_id, profile_uid, dedup_key, name, encrypted_blob, nonce,
                 aead_alg, created_at, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'aes-256-gcm', ?7, ?7)",
            params![default_user_id, uid, key, uid, encrypted_blob, nonce, now],
        )
        .map_err(|e| format!("Migrate legacy profile: {e}"))?;
        migrated_profiles += 1;
    }

    let mut migrated_settings_scopes = 0usize;
    if let Some(settings) = legacy_settings.as_ref() {
        let (encrypted_blob, nonce) = encrypt_value(&default_dek, settings)?;
        tx.execute(
            "INSERT INTO user_settings(
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
        migrated_settings_scopes = 1;
    }

    upsert_global_state(&tx, LEGACY_PROFILES_KEY, &legacy_profiles_backup, now)?;
    upsert_global_state(&tx, LEGACY_SETTINGS_KEY, &legacy_settings_backup, now)?;
    upsert_global_state(&tx, ACTIVE_USER_KEY, &default_user_id.to_string(), now)?;
    upsert_global_state(&tx, SCHEMA_VERSION_KEY, SCHEMA_VERSION, now)?;
    tx.commit()
        .map_err(|e| format!("Commit user partitions migration: {e}"))?;

    Ok(MigrationReport {
        schema_version: SCHEMA_VERSION.to_string(),
        created_default_user: true,
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

pub fn init_or_migrate(app: &AppHandle) -> Result<MigrationReport, String> {
    let mut conn = open_or_init(app)?;
    if matches!(
        current_schema_version(&conn)?.as_deref(),
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

    // v2 -> v3: the schema CREATE TABLE already adds is_admin on fresh
    // installs; existing v2 databases need a column add and a one-shot
    // seed that promotes the lowest-id user (the legacy default) to
    // admin so a returning operator keeps full control over the new
    // account-management surface.
    let stored_version = current_schema_version(&conn)?;
    if stored_version.as_deref() == Some("2") {
        upgrade_v2_to_v3(&mut conn)?;
        return Ok(MigrationReport {
            schema_version: SCHEMA_VERSION.to_string(),
            created_default_user: false,
            migrated_profiles: 0,
            migrated_settings_scopes: 0,
            already_migrated: true,
        });
    }

    // v3 -> v4: the P2P secret-store tables (peer_identity / peer_contacts /
    // peer_drives) are additive and already created by `init_db_schema`'s
    // CREATE TABLE IF NOT EXISTS on every open, so the upgrade only stamps the
    // new version. This branch is REQUIRED: without it a v3 database would fall
    // through to `migrate_legacy_payloads`, which re-inserts the default user
    // and re-imports legacy payloads into a partition that already has users.
    if stored_version.as_deref() == Some("3") {
        upgrade_v3_to_v4(&mut conn)?;
        return Ok(MigrationReport {
            schema_version: SCHEMA_VERSION.to_string(),
            created_default_user: false,
            migrated_profiles: 0,
            migrated_settings_scopes: 0,
            already_migrated: true,
        });
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
    result
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
    upsert_global_state(&tx, SCHEMA_VERSION_KEY, SCHEMA_VERSION, now_ms())?;
    tx.commit()
        .map_err(|e| format!("Commit v2->v3 upgrade: {e}"))?;
    Ok(())
}

/// v3 -> v4 upgrade. The new P2P secret-store tables are additive (already
/// created by `init_db_schema` on open), so this only stamps the new schema
/// version inside a transaction, mirroring [`upgrade_v2_to_v3`].
fn upgrade_v3_to_v4(conn: &mut Connection) -> Result<(), String> {
    let tx = conn
        .transaction()
        .map_err(|e| format!("Start v3->v4 upgrade: {e}"))?;
    upsert_global_state(&tx, SCHEMA_VERSION_KEY, SCHEMA_VERSION, now_ms())?;
    tx.commit()
        .map_err(|e| format!("Commit v3->v4 upgrade: {e}"))?;
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

pub fn list_users(conn: &Connection) -> Result<Vec<UserMetadata>, String> {
    let active = active_user_id(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, name, avatar_emoji, avatar_color, has_passphrase, sort_order,
                    created_at, updated_at, last_unlocked_at, is_admin
             FROM users
             ORDER BY sort_order ASC, name_canonical ASC",
        )
        .map_err(|e| format!("Prepare list users: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let has_passphrase: i64 = row.get(4)?;
            let is_admin: i64 = row.get(9)?;
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
    /// True when the target partition already contained an equivalent server
    /// (same dedup identity): the insert was skipped to avoid a duplicate.
    pub already_present: bool,
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
/// disturbed. When the target already holds an equivalent server the insert is
/// skipped (`already_present`), but for `remove_from_source` the source row is
/// still deleted so a Move always satisfies "this profile now lives in B".
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

    // 2. Clone the blob under a fresh id and drop the per-account session field.
    let mut relocated = source.clone();
    if let Value::Object(map) = &mut relocated {
        map.insert("id".into(), Value::String(new_profile_id.to_string()));
        map.remove("lastConnected");
    }

    // 3. Resolve the target DEK with its own passphrase, never via the session.
    let target_dek =
        resolve_user_dek_scoped(conn, &root_secret, target_user_id, target_passphrase)?;

    // 4. Dedup against the target on the stable identity (username-based when
    //    available, so a fresh id never masks an existing same-account server).
    let source_seed = value_str(&source, &["id", "uid", "profileUid"]).unwrap_or(profile_id);
    let source_tag = profile_dedup_key(&root_secret, &source, source_seed)?;
    let target_profiles = read_profiles_with_dek(conn, target_user_id, &target_dek)?;
    let mut already_present = false;
    for existing in &target_profiles {
        let seed = value_str(existing, &["id", "uid", "profileUid"]).unwrap_or("");
        if profile_dedup_key(&root_secret, existing, seed)? == source_tag {
            already_present = true;
            break;
        }
    }

    // 5. Insert into the target (prepend, like Duplicate) unless it is a dup.
    if !already_present {
        let mut new_list = Vec::with_capacity(target_profiles.len() + 1);
        new_list.push(relocated);
        new_list.extend(target_profiles);
        write_profiles_with_dek(conn, &root_secret, target_user_id, &target_dek, &new_list)?;
    }

    // 6. Move/Cut: drop the source row from the active partition.
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
    })
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

pub fn init_or_migrate_cli(store: &CredentialStore) -> Result<MigrationReport, String> {
    let mut conn = open_or_init_cli()?;
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
    result
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
    root_key.zeroize();
    if let Some(passphrase) = target_passphrase.as_mut() {
        passphrase.zeroize();
    }
    let relocation = result?;

    // Credentials live in the OS keyring (`server_<id>`), outside the partition
    // DB. Copy the source secret onto the new id when a fresh row was inserted;
    // a no-op dedup leaves the existing target secret untouched.
    if !relocation.already_present {
        if let Ok(secret) = store.get(&format!("server_{}", relocation.source_profile_id)) {
            let _ = store.store(&format!("server_{}", relocation.new_profile_id), &secret);
        }
    }
    // Move/Cut: the source row is gone, so its now-orphaned secret is removed.
    if relocation.moved {
        let _ = store.delete(&format!("server_{}", relocation.source_profile_id));
    }
    Ok(relocation)
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
        let _guard = test_lock();
        let mut conn = migrated_conn(0);
        let root = test_root();
        let default = get_active_user(&conn).expect("active").expect("default");
        let bob = create_passphrase_less_user(&mut conn, &root, "Bob", Some("B"), Some("#6366f1"))
            .expect("create bob");
        replace_active_server_profiles(&mut conn, &root, &[sftp_profile("srv_src")])
            .expect("seed source profile");

        relocate_server_profile(
            &mut conn, &root, default.id, bob.id, "srv_src", "srv_new1", None, false,
        )
        .expect("first copy");
        // Same logical server (same host/user) -> dedup skip, no duplicate row.
        let report = relocate_server_profile(
            &mut conn, &root, default.id, bob.id, "srv_src", "srv_new2", None, false,
        )
        .expect("second copy");
        assert!(report.already_present);
        let target = list_server_profiles_for(&conn, &root, bob.id).expect("target list");
        assert_eq!(target.len(), 1, "dedup must not create a second copy");
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
        let version = current_schema_version(&conn).expect("version");
        assert_eq!(version.as_deref(), Some(SCHEMA_VERSION));

        // Idempotent: rerun must not crash and must not flip admin off.
        upgrade_v2_to_v3(&mut conn).expect("idempotent rerun");
        let still_admin: i64 = conn
            .query_row("SELECT is_admin FROM users WHERE id = 1", [], |row| {
                row.get(0)
            })
            .expect("admin still");
        assert_eq!(still_admin, 1);
    }

    #[test]
    fn upgrade_v3_to_v4_stamps_version_and_preserves_users() {
        // Regression guard for the data-loss-class finding behind the WI-4b v3->v4
        // branch: a stored-v3 database (which already has users) must upgrade to v4
        // by ONLY stamping the version - it must NOT re-insert a default user or
        // otherwise touch existing partitions (that is what the missing branch would
        // have caused by falling through to migrate_legacy_payloads).
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
        .expect("v3 schema");
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
            "INSERT INTO global_state(key, value, updated_at) VALUES (?1, '3', ?2)",
            params![SCHEMA_VERSION_KEY, now],
        )
        .expect("seed schema v3");

        let users_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .expect("count before");
        assert_eq!(users_before, 2);

        upgrade_v3_to_v4(&mut conn).expect("upgrade v3->v4");

        let version = current_schema_version(&conn).expect("version");
        assert_eq!(version.as_deref(), Some(SCHEMA_VERSION));
        let users_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .expect("count after");
        assert_eq!(
            users_after, 2,
            "v3->v4 must preserve users (no default re-insert)"
        );

        // Idempotent: rerunning must not crash nor change the user set.
        upgrade_v3_to_v4(&mut conn).expect("idempotent rerun");
        let users_final: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .expect("count final");
        assert_eq!(users_final, 2);
    }
}
