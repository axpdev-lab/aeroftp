/// File Tags SQLite Backend
///
/// Provides a label-based tagging system for files in both local and remote
/// panels. Labels are color-coded and orderable; each file can carry multiple
/// labels. Data is persisted in a per-user SQLite database with WAL mode.

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
    let config_dir = app
        .path()
        .app_config_dir()
        .map_err(|_| "Cannot resolve app config dir".to_string())?;
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
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create config dir: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
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
pub async fn file_tags_list_labels(
    app: AppHandle,
) -> Result<Vec<TagLabel>, String> {
    let db = app.state::<FileTagsDb>();
    let conn = acquire_lock(&db);

    let mut stmt = conn
        .prepare(
            "SELECT id, name, color, sort_order, is_preset FROM labels ORDER BY sort_order",
        )
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

    Ok(rows.filter_map(|r| match r {
        Ok(v) => Some(v),
        Err(e) => { tracing::warn!("Row decode error in file_tags: {e}"); None }
    }).collect())
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
        .query_row("SELECT COALESCE(MAX(sort_order), -1) FROM labels", [], |r| {
            r.get(0)
        })
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
pub async fn file_tags_delete_label(
    app: AppHandle,
    id: i64,
) -> Result<(), String> {
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

    let db = app.state::<FileTagsDb>();
    let conn = acquire_lock(&db);

    // SAFETY: placeholders are always "?" — never interpolate user values in the IN clause
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
    let params_vec: Vec<&dyn rusqlite::types::ToSql> =
        file_paths.iter().map(|p| p as &dyn rusqlite::types::ToSql).collect();

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

    Ok(rows.filter_map(|r| match r {
        Ok(v) => Some(v),
        Err(e) => { tracing::warn!("Row decode error in file_tags: {e}"); None }
    }).collect())
}

/// Get all file paths that have a specific label
#[tauri::command]
pub async fn file_tags_get_files_by_label(
    app: AppHandle,
    label_id: i64,
) -> Result<Vec<String>, String> {
    let db = app.state::<FileTagsDb>();
    let conn = acquire_lock(&db);

    let mut stmt = conn
        .prepare("SELECT file_path FROM file_tags WHERE label_id = ?1")
        .map_err(|e| format!("Prepare: {e}"))?;

    let rows = stmt
        .query_map(params![label_id], |row| row.get::<_, String>(0))
        .map_err(|e| format!("Query: {e}"))?;

    Ok(rows.filter_map(|r| match r {
        Ok(v) => Some(v),
        Err(e) => { tracing::warn!("Row decode error in file_tags: {e}"); None }
    }).collect())
}

/// Get label usage counts (how many files each label is applied to)
#[tauri::command]
pub async fn file_tags_get_label_counts(
    app: AppHandle,
) -> Result<Vec<LabelCount>, String> {
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

    Ok(rows.filter_map(|r| match r {
        Ok(v) => Some(v),
        Err(e) => { tracing::warn!("Row decode error in file_tags: {e}"); None }
    }).collect())
}
