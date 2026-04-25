//! Vault History SQLite Backend
//!
//! Tracks recently opened AeroVault files with metadata (security level,
//! version, cascade mode, file count). Persisted in a per-user SQLite
//! database with WAL mode. Automatically trims to 20 most-recent entries.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::State;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RecentVault {
    pub id: i64,
    pub vault_path: String,
    pub vault_name: String,
    pub security_level: String,
    pub vault_version: i64,
    pub cascade_mode: bool,
    pub file_count: i64,
    pub last_opened_at: i64,
    pub created_at: i64,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct VaultHistoryDb(pub Mutex<Connection>);

/// Acquire DB lock with poison recovery
fn acquire_lock(db: &VaultHistoryDb) -> std::sync::MutexGuard<'_, Connection> {
    db.0.lock().unwrap_or_else(|e| {
        log::warn!("Vault history DB mutex was poisoned, recovering: {e}");
        e.into_inner()
    })
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize schema on an already-opened connection (used for in-memory fallback too)
pub fn init_db(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA cache_size = -2000;
         PRAGMA synchronous = NORMAL;",
    )
    .map_err(|e| format!("Pragma error: {e}"))?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS recent_vaults (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            vault_path TEXT NOT NULL UNIQUE,
            vault_name TEXT NOT NULL,
            security_level TEXT NOT NULL DEFAULT 'advanced',
            vault_version INTEGER NOT NULL DEFAULT 2,
            cascade_mode INTEGER NOT NULL DEFAULT 0,
            file_count INTEGER NOT NULL DEFAULT 0,
            last_opened_at INTEGER NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_recent_vaults_opened
            ON recent_vaults(last_opened_at DESC);",
    )
    .map_err(|e| format!("Schema error: {e}"))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

const MAX_ENTRIES: i64 = 20;

// ---------------------------------------------------------------------------
// Tauri Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn vault_history_save(
    state: State<'_, VaultHistoryDb>,
    vault_path: String,
    vault_name: String,
    security_level: String,
    vault_version: i64,
    cascade_mode: bool,
    file_count: i64,
) -> Result<(), String> {
    let conn = acquire_lock(&state);
    let now = now_epoch();
    let cascade_int: i64 = if cascade_mode { 1 } else { 0 };

    conn.execute(
        "INSERT INTO recent_vaults
            (vault_path, vault_name, security_level, vault_version, cascade_mode, file_count, last_opened_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
         ON CONFLICT(vault_path) DO UPDATE SET
            vault_name = excluded.vault_name,
            security_level = excluded.security_level,
            vault_version = excluded.vault_version,
            cascade_mode = excluded.cascade_mode,
            file_count = excluded.file_count,
            last_opened_at = excluded.last_opened_at",
        params![vault_path, vault_name, security_level, vault_version, cascade_int, file_count, now],
    )
    .map_err(|e| e.to_string())?;

    // Trim to MAX_ENTRIES
    conn.execute(
        "DELETE FROM recent_vaults WHERE id NOT IN (
            SELECT id FROM recent_vaults ORDER BY last_opened_at DESC LIMIT ?1
        )",
        params![MAX_ENTRIES],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn vault_history_list(
    state: State<'_, VaultHistoryDb>,
) -> Result<Vec<RecentVault>, String> {
    let conn = acquire_lock(&state);
    let mut stmt = conn
        .prepare(
            "SELECT id, vault_path, vault_name, security_level, vault_version,
                    cascade_mode, file_count, last_opened_at, created_at
             FROM recent_vaults
             ORDER BY last_opened_at DESC
             LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map(params![MAX_ENTRIES], |row| {
            Ok(RecentVault {
                id: row.get(0)?,
                vault_path: row.get(1)?,
                vault_name: row.get(2)?,
                security_level: row.get(3)?,
                vault_version: row.get(4)?,
                cascade_mode: row.get::<_, i64>(5)? != 0,
                file_count: row.get(6)?,
                last_opened_at: row.get(7)?,
                created_at: row.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row.map_err(|e| e.to_string())?);
    }
    Ok(results)
}

#[tauri::command]
pub async fn vault_history_remove(
    state: State<'_, VaultHistoryDb>,
    vault_path: String,
) -> Result<(), String> {
    let conn = acquire_lock(&state);
    conn.execute(
        "DELETE FROM recent_vaults WHERE vault_path = ?1",
        params![vault_path],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn vault_history_clear(state: State<'_, VaultHistoryDb>) -> Result<(), String> {
    let conn = acquire_lock(&state);
    conn.execute("DELETE FROM recent_vaults", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_in_memory() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        init_db(&conn).expect("init schema");
        conn
    }

    fn insert_vault(conn: &Connection, path: &str, name: &str, last_opened: i64) -> i64 {
        conn.execute(
            "INSERT INTO recent_vaults
                (vault_path, vault_name, security_level, vault_version,
                 cascade_mode, file_count, last_opened_at, created_at)
             VALUES (?1, ?2, 'advanced', 2, 0, 0, ?3, ?3)
             ON CONFLICT(vault_path) DO UPDATE SET
                last_opened_at = excluded.last_opened_at,
                vault_name = excluded.vault_name",
            params![path, name, last_opened],
        )
        .expect("insert vault");
        conn.last_insert_rowid()
    }

    #[test]
    fn init_db_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(init_db(&conn).is_ok());
        // Running again over an existing schema must not error.
        assert!(init_db(&conn).is_ok());
    }

    #[test]
    fn init_db_creates_recent_vaults_table_with_unique_path() {
        let conn = open_in_memory();
        insert_vault(&conn, "/vault/one.aerovault", "one", 100);

        // Upsert on conflict: same path with later timestamp should update, not duplicate.
        insert_vault(&conn, "/vault/one.aerovault", "one-renamed", 200);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM recent_vaults", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let name: String = conn
            .query_row(
                "SELECT vault_name FROM recent_vaults WHERE vault_path = ?1",
                params!["/vault/one.aerovault"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name, "one-renamed");
    }

    #[test]
    fn init_db_creates_opened_at_index() {
        let conn = open_in_memory();
        let idx: String = conn
            .query_row(
                "SELECT name FROM sqlite_master
                 WHERE type='index' AND name='idx_recent_vaults_opened'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, "idx_recent_vaults_opened");
    }

    #[test]
    fn ordering_by_last_opened_desc_returns_newest_first() {
        let conn = open_in_memory();
        insert_vault(&conn, "/a.aerovault", "a", 100);
        insert_vault(&conn, "/b.aerovault", "b", 200);
        insert_vault(&conn, "/c.aerovault", "c", 150);

        let mut stmt = conn
            .prepare(
                "SELECT vault_name FROM recent_vaults
                 ORDER BY last_opened_at DESC",
            )
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(names, vec!["b", "c", "a"]);
    }

    #[test]
    fn trim_query_keeps_only_max_entries_by_last_opened() {
        let conn = open_in_memory();
        // Insert more than MAX_ENTRIES
        for i in 0..(MAX_ENTRIES + 5) {
            insert_vault(
                &conn,
                &format!("/vault/{}.aerovault", i),
                &format!("vault-{}", i),
                100 + i,
            );
        }
        // Apply the same trim query used by vault_history_save
        conn.execute(
            "DELETE FROM recent_vaults WHERE id NOT IN (
                SELECT id FROM recent_vaults ORDER BY last_opened_at DESC LIMIT ?1
            )",
            params![MAX_ENTRIES],
        )
        .unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM recent_vaults", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, MAX_ENTRIES);

        // Oldest entries should have been dropped — vault-0..vault-4 gone
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM recent_vaults WHERE vault_name = 'vault-0'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 0);
    }

    #[test]
    fn now_epoch_returns_positive_unix_seconds() {
        let t = now_epoch();
        // Sanity: must be after 2020-01-01 (1577836800)
        assert!(
            t > 1_577_836_800,
            "now_epoch returned suspiciously old value {}",
            t
        );
    }

    #[test]
    fn max_entries_constant_matches_documented_cap() {
        // Contract check — the retention cap is a documented invariant.
        assert_eq!(MAX_ENTRIES, 20);
    }
}
