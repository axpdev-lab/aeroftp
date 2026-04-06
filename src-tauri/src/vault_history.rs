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
