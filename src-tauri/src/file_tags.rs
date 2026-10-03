//! File Tags SQLite Backend
//!
//! Provides a label-based tagging system for files in both local and remote
//! panels. Labels are color-coded and orderable; each file can carry multiple
//! labels. Data is persisted in a per-user SQLite database with WAL mode.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::filesystem::validate_path;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TagLabel {
    pub id: i64,
    pub name: String,
    pub color: String,
    pub sort_order: i64,
    pub is_preset: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileTag {
    pub id: i64,
    pub file_path: String,
    pub label_id: i64,
    pub label_name: String,
    pub label_color: String,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LabelCount {
    pub id: i64,
    pub name: String,
    pub color: String,
    pub count: i64,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct FileTagsDb(pub Mutex<Connection>);

fn db_path(app: &AppHandle) -> Result<PathBuf, String> {
    let config_dir = crate::portable::app_config_dir(app)?;
    Ok(config_dir.join("file_tags.db"))
}

/// Acquire DB lock with poison recovery
fn acquire_lock(db: &FileTagsDb) -> std::sync::MutexGuard<'_, Connection> {
    db.0.lock().unwrap_or_else(|e| {
        log::warn!("File tags DB mutex was poisoned, recovering: {e}");
        e.into_inner()
    })
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize schema on an already-opened connection (used for in-memory fallback)
pub fn init_db_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA cache_size = -2000;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )
    .map_err(|e| format!("Pragma error: {e}"))?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS labels (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            color TEXT NOT NULL,
            sort_order INTEGER NOT NULL DEFAULT 0,
            is_preset INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS file_tags (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_path TEXT NOT NULL,
            label_id INTEGER NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
            FOREIGN KEY (label_id) REFERENCES labels(id) ON DELETE CASCADE,
            UNIQUE(file_path, label_id)
        );

        CREATE INDEX IF NOT EXISTS idx_ft_path ON file_tags(file_path);
        CREATE INDEX IF NOT EXISTS idx_ft_label ON file_tags(label_id);",
    )
    .map_err(|e| format!("Schema error: {e}"))?;

    // Seed 7 preset labels (only if table is empty)
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM labels", [], |r| r.get(0))
        .unwrap_or(0);
    if count == 0 {
        conn.execute_batch(
            "INSERT INTO labels (name, color, sort_order, is_preset) VALUES
             ('Red', '#FF3B30', 0, 1),
             ('Orange', '#FF9500', 1, 1),
             ('Yellow', '#FFCC00', 2, 1),
             ('Green', '#34C759', 3, 1),
             ('Blue', '#007AFF', 4, 1),
             ('Purple', '#AF52DE', 5, 1),
             ('Gray', '#8E8E93', 6, 1);",
        )
        .map_err(|e| format!("Seed labels: {e}"))?;
    }

    Ok(())
}

pub fn init_db(app: &AppHandle) -> Result<Connection, String> {
    let path = db_path(app)?;

    // Ensure parent dir exists with 0700
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create config dir: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }

    let conn = Connection::open(&path)
        .map_err(|_| "Failed to initialize file tags database".to_string())?;

    // Set DB file permissions to 0600
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    init_db_schema(&conn)?;
    Ok(conn)
}

// ---------------------------------------------------------------------------
// Tauri Commands
// ---------------------------------------------------------------------------

/// List all labels ordered by sort_order
#[tauri::command]
pub async fn file_tags_list_labels(app: AppHandle) -> Result<Vec<TagLabel>, String> {
    let db = app.state::<FileTagsDb>();
    let conn = acquire_lock(&db);

    let mut stmt = conn
        .prepare("SELECT id, name, color, sort_order, is_preset FROM labels ORDER BY sort_order")
        .map_err(|e| format!("Prepare: {e}"))?;

    let rows = stmt
        .query_map([], |row| {
            Ok(TagLabel {
                id: row.get(0)?,
                name: row.get(1)?,
                color: row.get(2)?,
                sort_order: row.get(3)?,
                is_preset: row.get::<_, i64>(4)? != 0,
            })
        })
        .map_err(|e| format!("Query: {e}"))?;

    Ok(rows
        .filter_map(|r| match r {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("Row decode error in file_tags: {e}");
                None
            }
        })
        .collect())
}

/// Create a new custom label
#[tauri::command]
pub async fn file_tags_create_label(
    app: AppHandle,
    name: String,
    color: String,
) -> Result<TagLabel, String> {
    let db = app.state::<FileTagsDb>();
    let conn = acquire_lock(&db);

    let max_order: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(sort_order), -1) FROM labels",
            [],
            |r| r.get(0),
        )
        .unwrap_or(-1);

    let next_order = max_order + 1;

    conn.execute(
        "INSERT INTO labels (name, color, sort_order, is_preset) VALUES (?1, ?2, ?3, 0)",
        params![name, color, next_order],
    )
    .map_err(|e| format!("Insert label: {e}"))?;

    let id = conn.last_insert_rowid();

    Ok(TagLabel {
        id,
        name,
        color,
        sort_order: next_order,
        is_preset: false,
    })
}

/// Update an existing label's name and color
#[tauri::command]
pub async fn file_tags_update_label(
    app: AppHandle,
    id: i64,
    name: String,
    color: String,
) -> Result<(), String> {
    let db = app.state::<FileTagsDb>();
    let conn = acquire_lock(&db);

    conn.execute(
        "UPDATE labels SET name = ?1, color = ?2 WHERE id = ?3",
        params![name, color, id],
    )
    .map_err(|e| format!("Update label: {e}"))?;

    Ok(())
}

/// Delete a label (CASCADE deletes associated file_tags)
#[tauri::command]
pub async fn file_tags_delete_label(app: AppHandle, id: i64) -> Result<(), String> {
    let db = app.state::<FileTagsDb>();
    let conn = acquire_lock(&db);

    conn.execute("DELETE FROM labels WHERE id = ?1", params![id])
        .map_err(|e| format!("Delete label: {e}"))?;

    Ok(())
}

/// Assign labels to files (batch). For each (file_path, label_id) pair,
/// inserts or ignores if already tagged.
#[tauri::command]
pub async fn file_tags_set_tags(
    app: AppHandle,
    file_paths: Vec<String>,
    label_ids: Vec<i64>,
) -> Result<(), String> {
    for p in &file_paths {
        validate_path(p)?;
    }
    let db = app.state::<FileTagsDb>();
    let conn = acquire_lock(&db);

    conn.execute("BEGIN", [])
        .map_err(|e| format!("Begin: {e}"))?;

    let result = (|| -> Result<(), String> {
        let mut stmt = conn
            .prepare("INSERT OR IGNORE INTO file_tags (file_path, label_id) VALUES (?1, ?2)")
            .map_err(|e| format!("Prepare: {e}"))?;

        for path in &file_paths {
            for &lid in &label_ids {
                stmt.execute(params![path, lid])
                    .map_err(|e| format!("Insert tag: {e}"))?;
            }
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute("COMMIT", [])
                .map_err(|e| format!("Commit: {e}"))?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

/// Remove a specific tag from a file
#[tauri::command]
pub async fn file_tags_remove_tag(
    app: AppHandle,
    file_path: String,
    label_id: i64,
) -> Result<(), String> {
    validate_path(&file_path)?;
    let db = app.state::<FileTagsDb>();
    let conn = acquire_lock(&db);

    conn.execute(
        "DELETE FROM file_tags WHERE file_path = ?1 AND label_id = ?2",
        params![file_path, label_id],
    )
    .map_err(|e| format!("Remove tag: {e}"))?;

    Ok(())
}

/// Get all tags for a list of files (batch query with JOIN)
#[tauri::command]
pub async fn file_tags_get_tags_for_files(
    app: AppHandle,
    file_paths: Vec<String>,
) -> Result<Vec<FileTag>, String> {
    if file_paths.is_empty() {
        return Ok(vec![]);
    }
    for p in &file_paths {
        validate_path(p)?;
    }

    let db = app.state::<FileTagsDb>();
    let conn = acquire_lock(&db);

    // SAFETY: placeholders are always "?": never interpolate user values in the IN clause
    let placeholders: String = file_paths.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT ft.id, ft.file_path, ft.label_id, l.name, l.color, ft.created_at
         FROM file_tags ft
         JOIN labels l ON ft.label_id = l.id
         WHERE ft.file_path IN ({})
         ORDER BY ft.file_path, l.sort_order",
        placeholders
    );

    let mut stmt = conn.prepare(&sql).map_err(|e| format!("Prepare: {e}"))?;
    let params_vec: Vec<&dyn rusqlite::types::ToSql> = file_paths
        .iter()
        .map(|p| p as &dyn rusqlite::types::ToSql)
        .collect();

    let rows = stmt
        .query_map(params_vec.as_slice(), |row| {
            Ok(FileTag {
                id: row.get(0)?,
                file_path: row.get(1)?,
                label_id: row.get(2)?,
                label_name: row.get(3)?,
                label_color: row.get(4)?,
                created_at: row.get(5)?,
            })
        })
        .map_err(|e| format!("Query: {e}"))?;

    Ok(rows
        .filter_map(|r| match r {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("Row decode error in file_tags: {e}");
                None
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Tags follow local moves and deletes
// ---------------------------------------------------------------------------
//
// Tags are keyed by path. A rename or move that left them behind lost them, and
// worse, handed them to whatever file later took the old name; a delete left
// them to the next file created at that path. The app's own local rename,
// move and delete paths (the GUI commands and AeroAgent's local tools) call
// `follow_move` / `follow_delete` after the filesystem change succeeds. Moving
// to the OS trash keeps the tags, so a file restored to its path gets them back.

/// Whether `path` is a Windows path (drive letter or UNC), where `\` and `/`
/// both separate components. On any other path a backslash is part of a name.
fn is_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || path.starts_with("\\\\")
}

/// `path` without trailing separators of its own style (a root stays as it is).
fn trim_separators(path: &str) -> &str {
    let trimmed = if is_windows_path(path) {
        path.trim_end_matches(['/', '\\'])
    } else {
        path.trim_end_matches('/')
    };
    if trimmed.is_empty() {
        path
    } else {
        trimmed
    }
}

/// Every tag row of `path` and of anything under it, with the separators of the
/// path's own style. An exact prefix comparison, not LIKE: LIKE folds ASCII
/// case and would match `/data/Foo` for `/data/foo`, and `%` or `_` in a name
/// would need escaping.
const UNDER_PATH: &str = "file_path = ?1 \
     OR substr(file_path, 1, length(?2)) = ?2 \
     OR substr(file_path, 1, length(?3)) = ?3";

fn under_patterns(path: &str) -> (String, String) {
    // A Unix path has a single separator, so its second pattern repeats it.
    let second = if is_windows_path(path) { '\\' } else { '/' };
    (format!("{path}/"), format!("{path}{second}"))
}

/// Drop the tags of `path` and of everything under it.
pub fn forget_tags_in_conn(conn: &Connection, path: &str) -> Result<usize, String> {
    let path = trim_separators(path);
    let (slash, backslash) = under_patterns(path);
    conn.execute(
        &format!("DELETE FROM file_tags WHERE {UNDER_PATH}"),
        params![path, slash, backslash],
    )
    .map_err(|e| format!("Forget file tags: {e}"))
}

/// Move the tags of `from` (and of everything under it) to `to`. Whatever was
/// tagged at `to` is replaced, as the move replaced the file there.
pub fn move_tags_in_conn(conn: &mut Connection, from: &str, to: &str) -> Result<usize, String> {
    let (from, to) = (trim_separators(from), trim_separators(to));
    if from == to {
        return Ok(0);
    }
    let tx = conn
        .transaction()
        .map_err(|e| format!("Begin tag move: {e}"))?;
    forget_tags_in_conn(&tx, to)?;
    let (slash, backslash) = under_patterns(from);
    let moved = tx
        .execute(
            &format!(
                "UPDATE file_tags SET file_path = ?4 || substr(file_path, length(?1) + 1) WHERE {UNDER_PATH}"
            ),
            params![from, slash, backslash, to],
        )
        .map_err(|e| format!("Move file tags: {e}"))?;
    tx.commit().map_err(|e| format!("Commit tag move: {e}"))?;
    Ok(moved)
}

/// After a successful local rename or move: carry the tags along. Best effort,
/// the filesystem change already happened; a failure is logged.
pub fn follow_move(app: &AppHandle, from: &str, to: &str) {
    let Some(db) = app.try_state::<FileTagsDb>() else {
        return;
    };
    let mut conn = acquire_lock(&db);
    if let Err(e) = move_tags_in_conn(&mut conn, from, to) {
        tracing::warn!("file tags did not follow {from} -> {to}: {e}");
    }
}

/// Drop the tags of the paths at or under `path` that `exists` says are gone.
/// A folder delete can stop half way (an error, a cancel) and still report
/// success: what is still on disk keeps its tags.
pub fn forget_tags_of_missing_in_conn(
    conn: &Connection,
    path: &str,
    exists: impl Fn(&str) -> bool,
) -> Result<usize, String> {
    let path = trim_separators(path);
    let (slash, backslash) = under_patterns(path);
    let mut stmt = conn
        .prepare(&format!(
            "SELECT DISTINCT file_path FROM file_tags WHERE {UNDER_PATH}"
        ))
        .map_err(|e| format!("Prepare tag lookup: {e}"))?;
    let tagged: Vec<String> = stmt
        .query_map(params![path, slash, backslash], |r| r.get(0))
        .map_err(|e| format!("Tag lookup: {e}"))?
        .filter_map(Result::ok)
        .collect();
    let mut forgotten = 0;
    for gone in tagged.iter().filter(|p| !exists(p)) {
        forgotten += conn
            .execute("DELETE FROM file_tags WHERE file_path = ?1", params![gone])
            .map_err(|e| format!("Forget file tags: {e}"))?;
    }
    Ok(forgotten)
}

/// After a local delete: drop the tags of what is no longer on disk.
/// Whether `p` is still on disk, for keeping its tags. Only a NotFound answer
/// counts as gone: a permission or I/O error says nothing about the entry
/// (`Path::exists` folds those into "missing"), and a dangling symlink is
/// still an entry in its folder.
fn still_on_disk(p: &str) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

pub fn follow_delete(app: &AppHandle, path: &str) {
    let Some(db) = app.try_state::<FileTagsDb>() else {
        return;
    };
    let conn = acquire_lock(&db);
    if let Err(e) = forget_tags_of_missing_in_conn(&conn, path, still_on_disk) {
        tracing::warn!("file tags of deleted {path} were kept: {e}");
    }
}

/// Get label usage counts (how many files each label is applied to)
#[tauri::command]
pub async fn file_tags_get_label_counts(app: AppHandle) -> Result<Vec<LabelCount>, String> {
    let db = app.state::<FileTagsDb>();
    let conn = acquire_lock(&db);

    let mut stmt = conn
        .prepare(
            "SELECT l.id, l.name, l.color, COUNT(ft.id) as count
             FROM labels l
             LEFT JOIN file_tags ft ON l.id = ft.label_id
             GROUP BY l.id
             ORDER BY l.sort_order",
        )
        .map_err(|e| format!("Prepare: {e}"))?;

    let rows = stmt
        .query_map([], |row| {
            Ok(LabelCount {
                id: row.get(0)?,
                name: row.get(1)?,
                color: row.get(2)?,
                count: row.get(3)?,
            })
        })
        .map_err(|e| format!("Query: {e}"))?;

    Ok(rows
        .filter_map(|r| match r {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("Row decode error in file_tags: {e}");
                None
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db_schema(&conn).unwrap();
        conn
    }

    fn tag(conn: &Connection, path: &str, label_id: i64) {
        conn.execute(
            "INSERT INTO file_tags (file_path, label_id) VALUES (?1, ?2)",
            params![path, label_id],
        )
        .unwrap();
    }

    fn tagged(conn: &Connection) -> Vec<(String, i64)> {
        let mut stmt = conn
            .prepare("SELECT file_path, label_id FROM file_tags ORDER BY file_path, label_id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// `file_tags_update_path` existed for this and nothing called it, so a
    /// rename lost the file's tags. A folder carries the tags of everything
    /// under it, and a sibling sharing the name as a prefix keeps its own.
    #[test]
    fn a_rename_carries_the_tags_of_the_item_and_everything_under_it() {
        let mut conn = db();
        tag(&conn, "/home/u/doc.txt", 1);
        tag(&conn, "/home/u/proj", 2);
        tag(&conn, "/home/u/proj/a.rs", 3);
        tag(&conn, "/home/u/proj/sub/b.rs", 4);
        tag(&conn, "/home/u/projects/c.rs", 5);
        tag(&conn, "C:\\work\\proj\\d.rs", 6);

        move_tags_in_conn(&mut conn, "/home/u/doc.txt", "/home/u/notes.txt").unwrap();
        move_tags_in_conn(&mut conn, "/home/u/proj/", "/home/u/app").unwrap();
        move_tags_in_conn(&mut conn, "C:\\work\\proj", "C:\\work\\app").unwrap();

        assert_eq!(
            tagged(&conn),
            [
                ("/home/u/app".to_string(), 2),
                ("/home/u/app/a.rs".to_string(), 3),
                ("/home/u/app/sub/b.rs".to_string(), 4),
                ("/home/u/notes.txt".to_string(), 1),
                ("/home/u/projects/c.rs".to_string(), 5),
                ("C:\\work\\app\\d.rs".to_string(), 6),
            ]
        );
    }

    /// On a Unix path a backslash is a character of the name, not a separator:
    /// `/tmp/a\b` is a sibling of `/tmp/a`, so a move or delete of `/tmp/a`
    /// leaves it alone. A drive or UNC path still matches both separators.
    #[test]
    fn a_unix_backslash_is_part_of_the_name_not_a_separator() {
        let mut conn = db();
        tag(&conn, "/tmp/a", 1);
        tag(&conn, "/tmp/a\\b", 2);
        tag(&conn, "/tmp/a/c", 3);
        move_tags_in_conn(&mut conn, "/tmp/a", "/tmp/z").unwrap();
        assert_eq!(
            tagged(&conn),
            [
                ("/tmp/a\\b".to_string(), 2),
                ("/tmp/z".to_string(), 1),
                ("/tmp/z/c".to_string(), 3),
            ]
        );
        forget_tags_in_conn(&conn, "/tmp/z").unwrap();
        assert_eq!(tagged(&conn), [("/tmp/a\\b".to_string(), 2)]);
    }

    /// A move that overwrote a tagged file takes the moved file's tags, and a
    /// `%` or `_` in a name is a character, not a wildcard.
    #[test]
    fn an_overwrite_replaces_the_destination_tags_and_wildcards_stay_literal() {
        let mut conn = db();
        tag(&conn, "/d/new.txt", 1);
        tag(&conn, "/d/old.txt", 2);
        tag(&conn, "/d/a_b/x", 3);
        tag(&conn, "/d/aXb/y", 4);

        move_tags_in_conn(&mut conn, "/d/new.txt", "/d/old.txt").unwrap();
        move_tags_in_conn(&mut conn, "/d/a_b", "/d/c").unwrap();

        assert_eq!(
            tagged(&conn),
            [
                ("/d/aXb/y".to_string(), 4),
                ("/d/c/x".to_string(), 3),
                ("/d/old.txt".to_string(), 1),
            ]
        );
    }

    /// Paths are case-sensitive here: SQLite's LIKE folds ASCII case, so a
    /// move of `/data/foo` also took the tags of the distinct `/data/Foo`.
    #[test]
    fn a_move_leaves_a_case_distinct_sibling_alone() {
        let mut conn = db();
        tag(&conn, "/data/foo/y", 1);
        tag(&conn, "/data/Foo/x", 2);
        tag(&conn, "/data/FOO", 3);

        move_tags_in_conn(&mut conn, "/data/foo", "/data/bar").unwrap();
        forget_tags_in_conn(&conn, "/data/fOo").unwrap();

        assert_eq!(
            tagged(&conn),
            [
                ("/data/FOO".to_string(), 3),
                ("/data/Foo/x".to_string(), 2),
                ("/data/bar/y".to_string(), 1),
            ]
        );
    }

    /// `file_tags_delete_all_for_file` existed for this and nothing called
    /// it, so a delete left the tags to the next file created at that path.
    /// A folder delete that stopped half way keeps the tags of what survived.
    #[test]
    fn a_delete_forgets_the_tags_of_what_is_gone_and_only_that() {
        let conn = db();
        tag(&conn, "/d/gone", 1);
        tag(&conn, "/d/gone/inner.txt", 2);
        tag(&conn, "/d/gone/survivor.txt", 5);
        tag(&conn, "/d/gone.txt", 3);
        tag(&conn, "/d/kept/x", 4);

        // The folder delete failed on one file, so the folder stays too.
        let on_disk = [
            "/d/gone",
            "/d/gone/survivor.txt",
            "/d/gone.txt",
            "/d/kept/x",
        ];
        forget_tags_of_missing_in_conn(&conn, "/d/gone/", |p| on_disk.contains(&p)).unwrap();
        assert_eq!(
            tagged(&conn),
            [
                ("/d/gone".to_string(), 1),
                ("/d/gone.txt".to_string(), 3),
                ("/d/gone/survivor.txt".to_string(), 5),
                ("/d/kept/x".to_string(), 4),
            ]
        );

        // Second attempt removes everything.
        let on_disk = ["/d/gone.txt", "/d/kept/x"];
        forget_tags_of_missing_in_conn(&conn, "/d/gone", |p| on_disk.contains(&p)).unwrap();
        assert_eq!(
            tagged(&conn),
            [("/d/gone.txt".to_string(), 3), ("/d/kept/x".to_string(), 4)]
        );
    }

    /// Only "not found" means gone. `Path::exists` also answers false for an
    /// entry it cannot stat (a survivor under a folder the process cannot
    /// traverse) and for a dangling symlink, and the delete then dropped the
    /// tags of something still on disk.
    #[cfg(unix)]
    #[test]
    fn a_path_that_cannot_be_checked_still_counts_as_on_disk() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = |p: &std::path::Path| p.to_string_lossy().to_string();

        let link = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("no-target"), &link).unwrap();
        assert!(
            still_on_disk(&path(&link)),
            "a dangling symlink is still an entry"
        );

        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("survivor.txt"), b"x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let blocked = std::fs::symlink_metadata(locked.join("survivor.txt")).is_err();
        let survivor_kept = still_on_disk(&path(&locked.join("survivor.txt")));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Running as root nothing is blocked; the check is then vacuous, not wrong.
        if blocked {
            assert!(
                survivor_kept,
                "an entry the process cannot stat is not a deleted one"
            );
        }

        assert!(!still_on_disk(&path(&dir.path().join("missing"))));
    }
}
