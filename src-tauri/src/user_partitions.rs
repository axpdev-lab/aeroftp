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
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::AppHandle;

const DB_FILENAME: &str = "user_partitions.db";
const SCHEMA_VERSION: &str = "2";
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

pub fn init_db_schema(conn: &Connection) -> Result<(), String> {
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
             created_at, updated_at, last_unlocked_at
         )
         VALUES (?1, ?2, ?3, ?4, 0, NULL, NULL, ?5, ?6, 0, ?7, ?7, ?7)",
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

    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
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

pub fn list_users(conn: &Connection) -> Result<Vec<UserMetadata>, String> {
    let active = active_user_id(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, name, avatar_emoji, avatar_color, has_passphrase, sort_order,
                    created_at, updated_at, last_unlocked_at
             FROM users
             ORDER BY sort_order ASC, name_canonical ASC",
        )
        .map_err(|e| format!("Prepare list users: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let has_passphrase: i64 = row.get(4)?;
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

fn with_active_user_dek<R>(
    conn: &Connection,
    root_key: &SecretKey,
    f: impl FnOnce(i64, &SecretKey) -> Result<R, String>,
) -> Result<R, String> {
    let user_id = active_user_id(conn)?.ok_or_else(|| "NO_ACTIVE_USER".to_string())?;
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

pub fn list_active_server_profiles(
    conn: &Connection,
    root_key: &[u8; 32],
) -> Result<Vec<Value>, String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_active_user_dek(conn, &root_secret, |user_id, dek| {
        let mut stmt = conn
            .prepare(
                "SELECT encrypted_blob, nonce
                 FROM server_profiles
                 WHERE user_id = ?1
                 ORDER BY id ASC",
            )
            .map_err(|e| format!("Prepare active profile list: {e}"))?;
        let rows = stmt
            .query_map(params![user_id], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(|e| format!("Query active profile list: {e}"))?;

        let mut profiles = Vec::new();
        for row in rows {
            let (encrypted_blob, nonce) =
                row.map_err(|e| format!("Read active profile row: {e}"))?;
            profiles.push(decrypt_value(dek, &nonce, &encrypted_blob)?);
        }
        Ok(profiles)
    })
}

pub fn replace_active_server_profiles(
    conn: &mut Connection,
    root_key: &[u8; 32],
    profiles: &[Value],
) -> Result<(), String> {
    let root_secret = user_crypto::secret_key_from_bytes(root_key);
    with_active_user_dek(conn, &root_secret, |user_id, dek| {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("Start replace active profiles: {e}"))?;
        tx.execute(
            "DELETE FROM server_profiles WHERE user_id = ?1",
            params![user_id],
        )
        .map_err(|e| format!("Delete previous active profiles: {e}"))?;

        let now = now_ms();
        let mut seen_uids = HashSet::new();
        for (index, profile) in profiles.iter().enumerate() {
            let uid_seed = profile_uid_seed(profile, index, &mut seen_uids);
            let uid = user_crypto::metadata_tag(&root_secret, b"profile-uid", &uid_seed)?;
            let key = profile_dedup_key(&root_secret, profile, &uid_seed)?;
            let (encrypted_blob, nonce) = encrypt_value(dek, profile)?;
            tx.execute(
                "INSERT INTO server_profiles(
                     user_id, profile_uid, dedup_key, name, encrypted_blob, nonce,
                     aead_alg, created_at, updated_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'aes-256-gcm', ?7, ?7)",
                params![user_id, uid, key, uid, encrypted_blob, nonce, now],
            )
            .map_err(|e| format!("Insert active profile: {e}"))?;
        }

        tx.commit()
            .map_err(|e| format!("Commit replace active profiles: {e}"))?;
        Ok(())
    })
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

    set_active_user_row(conn, user_id)?;
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

pub fn set_active_user(conn: &Connection, user_id: i64) -> Result<(), String> {
    clear_user_session();
    set_active_user_row(conn, user_id)
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

#[tauri::command]
pub async fn user_partitions_init(app: AppHandle) -> Result<MigrationReport, String> {
    init_or_migrate(&app)
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

#[tauri::command]
pub async fn user_partitions_add_user(
    app: AppHandle,
    name: String,
    avatar_emoji: Option<String>,
    avatar_color: Option<String>,
    mut passphrase: Option<String>,
) -> Result<UserMetadata, String> {
    init_or_migrate(&app)?;
    let store = CredentialStore::from_cache().ok_or_else(|| "STORE_NOT_READY".to_string())?;
    let mut root_key = store.derive_user_partition_wrapping_key();
    let mut conn = open_or_init(&app)?;
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
    rename_user(&conn, user_id, &name)
}

#[tauri::command]
pub async fn user_partitions_reorder_users(
    app: AppHandle,
    user_ids: Vec<i64>,
) -> Result<(), String> {
    init_or_migrate(&app)?;
    let mut conn = open_or_init(&app)?;
    reorder_users(&mut conn, &user_ids)
}

#[tauri::command]
pub async fn user_partitions_delete_user(app: AppHandle, user_id: i64) -> Result<(), String> {
    init_or_migrate(&app)?;
    let mut conn = open_or_init(&app)?;
    delete_user(&mut conn, user_id)
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
        USER_PARTITION_TEST_LOCK.lock().expect("test lock")
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
}
