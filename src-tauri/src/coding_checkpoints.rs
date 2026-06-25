// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Workspace checkpoint store for AeroAgent coding mode.
//!
//! Checkpoints snapshot explicit touched files before a coding-agent mutation.
//! They are intentionally file-scoped: later patch/git tools can build on this
//! store without implying whole-workspace rollback.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_CHECKPOINT_FILES: usize = 100;
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingCheckpointFile {
    pub path: String,
    pub absolute_path: String,
    pub existed: bool,
    pub size_bytes: u64,
    pub sha256: Option<String>,
    pub modified_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingCheckpoint {
    pub id: String,
    pub session_id: Option<String>,
    pub anchor_message_id: Option<String>,
    pub conversation_anchor: Option<String>,
    pub workspace_root: String,
    pub label: Option<String>,
    pub created_at: i64,
    pub total_bytes: u64,
    pub files: Vec<CodingCheckpointFile>,
}

#[derive(Debug, Clone)]
pub struct CreateCodingCheckpointRequest {
    pub workspace_root: String,
    pub paths: Vec<String>,
    pub session_id: Option<String>,
    pub anchor_message_id: Option<String>,
    pub conversation_anchor: Option<String>,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RestoreCodingCheckpointRequest {
    pub checkpoint_id: String,
    pub paths: Option<Vec<String>>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingCheckpointRestoreFile {
    pub path: String,
    pub action: String,
    pub existed_at_checkpoint: bool,
    pub size_bytes: u64,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingCheckpointRestoreResult {
    pub checkpoint_id: String,
    pub workspace_root: String,
    pub dry_run: bool,
    pub files: Vec<CodingCheckpointRestoreFile>,
}

#[derive(Debug, Clone)]
struct SnapshotFile {
    summary: CodingCheckpointFile,
    content: Option<Vec<u8>>,
    unix_mode: Option<i64>,
}

#[derive(Debug, Clone)]
struct StoredCheckpointRow {
    id: String,
    workspace_root: String,
}

#[derive(Debug, Clone)]
struct StoredFileRow {
    path: String,
    existed: bool,
    content: Option<Vec<u8>>,
    size_bytes: u64,
    sha256: Option<String>,
    unix_mode: Option<i64>,
}

pub fn init_db_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS coding_checkpoints (
            id TEXT PRIMARY KEY,
            session_id TEXT,
            anchor_message_id TEXT,
            conversation_anchor TEXT,
            workspace_root TEXT NOT NULL,
            label TEXT,
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS coding_checkpoint_files (
            checkpoint_id TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            absolute_path TEXT NOT NULL,
            existed INTEGER NOT NULL,
            content BLOB,
            size_bytes INTEGER NOT NULL DEFAULT 0,
            sha256 TEXT,
            modified_at INTEGER,
            unix_mode INTEGER,
            PRIMARY KEY (checkpoint_id, relative_path),
            FOREIGN KEY (checkpoint_id) REFERENCES coding_checkpoints(id) ON DELETE CASCADE
        );

        CREATE INDEX IF NOT EXISTS idx_coding_checkpoints_session
            ON coding_checkpoints(session_id, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_coding_checkpoints_created
            ON coding_checkpoints(created_at DESC);",
    )
    .map_err(|e| format!("Coding checkpoint schema error: {e}"))
}

pub fn create_checkpoint(
    conn: &mut Connection,
    req: CreateCodingCheckpointRequest,
) -> Result<CodingCheckpoint, String> {
    init_db_schema(conn)?;

    let root = validate_workspace_root(&req.workspace_root)?;
    if req.paths.is_empty() {
        return Err("At least one path is required".to_string());
    }
    if req.paths.len() > MAX_CHECKPOINT_FILES {
        return Err(format!(
            "Too many checkpoint paths: {} (max {})",
            req.paths.len(),
            MAX_CHECKPOINT_FILES
        ));
    }

    let mut seen = HashSet::new();
    let mut snapshots = Vec::new();
    let mut total_bytes = 0u64;
    for path in req.paths {
        let snapshot = snapshot_path(&root, &path)?;
        if !seen.insert(snapshot.summary.path.clone()) {
            continue;
        }
        total_bytes = total_bytes.saturating_add(snapshot.summary.size_bytes);
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(format!(
                "Checkpoint exceeds total snapshot limit: {:.1} MB (max {:.1} MB)",
                total_bytes as f64 / 1_048_576.0,
                MAX_TOTAL_BYTES as f64 / 1_048_576.0
            ));
        }
        snapshots.push(snapshot);
    }

    let id = format!("cagcp_{}", uuid::Uuid::new_v4());
    let created_at = now_millis();
    let workspace_root = root.to_string_lossy().to_string();

    let tx = conn
        .transaction()
        .map_err(|e| format!("Begin checkpoint transaction: {e}"))?;
    tx.execute(
        "INSERT INTO coding_checkpoints
            (id, session_id, anchor_message_id, conversation_anchor, workspace_root, label, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            req.session_id,
            req.anchor_message_id,
            req.conversation_anchor,
            workspace_root,
            req.label,
            created_at
        ],
    )
    .map_err(|e| format!("Insert checkpoint: {e}"))?;

    for snapshot in &snapshots {
        tx.execute(
            "INSERT INTO coding_checkpoint_files
                (checkpoint_id, relative_path, absolute_path, existed, content, size_bytes, sha256, modified_at, unix_mode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                snapshot.summary.path,
                snapshot.summary.absolute_path,
                if snapshot.summary.existed { 1 } else { 0 },
                snapshot.content,
                snapshot.summary.size_bytes as i64,
                snapshot.summary.sha256,
                snapshot.summary.modified_at,
                snapshot.unix_mode,
            ],
        )
        .map_err(|e| format!("Insert checkpoint file {}: {e}", snapshot.summary.path))?;
    }
    tx.commit()
        .map_err(|e| format!("Commit checkpoint transaction: {e}"))?;

    Ok(CodingCheckpoint {
        id,
        session_id: req.session_id,
        anchor_message_id: req.anchor_message_id,
        conversation_anchor: req.conversation_anchor,
        workspace_root,
        label: req.label,
        created_at,
        total_bytes,
        files: snapshots.into_iter().map(|s| s.summary).collect(),
    })
}

pub fn restore_checkpoint(
    conn: &Connection,
    req: RestoreCodingCheckpointRequest,
) -> Result<CodingCheckpointRestoreResult, String> {
    init_db_schema(conn)?;
    validate_id(&req.checkpoint_id, "checkpoint_id")?;

    let checkpoint = load_checkpoint_row(conn, &req.checkpoint_id)?;
    let root = validate_workspace_root(&checkpoint.workspace_root)?;
    let mut files = load_file_rows(conn, &checkpoint.id)?;

    if let Some(paths) = req.paths {
        if paths.is_empty() {
            return Err("Restore path subset cannot be empty".to_string());
        }
        let subset = normalize_restore_subset(&root, paths)?;
        let mut found = HashSet::new();
        files.retain(|file| {
            let keep = subset.contains(&file.path);
            if keep {
                found.insert(file.path.clone());
            }
            keep
        });
        let missing: Vec<String> = subset.difference(&found).cloned().collect();
        if !missing.is_empty() {
            return Err(format!(
                "Path(s) not found in checkpoint {}: {}",
                req.checkpoint_id,
                missing.join(", ")
            ));
        }
    }

    let mut restored = Vec::new();
    for file in files {
        let target = resolve_relative_target(&root, &file.path)?;
        let action = if file.existed {
            restore_file(&target, &file, req.dry_run)?;
            "restore"
        } else if target.exists() {
            remove_created_file(&target, req.dry_run)?;
            "delete"
        } else {
            "noop"
        };
        restored.push(CodingCheckpointRestoreFile {
            path: file.path,
            action: action.to_string(),
            existed_at_checkpoint: file.existed,
            size_bytes: file.size_bytes,
            sha256: file.sha256,
        });
    }

    Ok(CodingCheckpointRestoreResult {
        checkpoint_id: checkpoint.id,
        workspace_root: root.to_string_lossy().to_string(),
        dry_run: req.dry_run,
        files: restored,
    })
}

fn load_checkpoint_row(conn: &Connection, id: &str) -> Result<StoredCheckpointRow, String> {
    conn.query_row(
        "SELECT id, workspace_root
         FROM coding_checkpoints WHERE id = ?1",
        params![id],
        |row| {
            Ok(StoredCheckpointRow {
                id: row.get(0)?,
                workspace_root: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(|e| format!("Load checkpoint: {e}"))?
    .ok_or_else(|| format!("Checkpoint not found: {id}"))
}

fn load_file_rows(conn: &Connection, checkpoint_id: &str) -> Result<Vec<StoredFileRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT relative_path, existed, content, size_bytes, sha256, unix_mode
             FROM coding_checkpoint_files
             WHERE checkpoint_id = ?1
             ORDER BY relative_path ASC",
        )
        .map_err(|e| format!("Prepare checkpoint files query: {e}"))?;
    let rows = stmt
        .query_map(params![checkpoint_id], |row| {
            let existed_i64: i64 = row.get(1)?;
            let size_i64: i64 = row.get(3)?;
            Ok(StoredFileRow {
                path: row.get(0)?,
                existed: existed_i64 != 0,
                content: row.get(2)?,
                size_bytes: size_i64.max(0) as u64,
                sha256: row.get(4)?,
                unix_mode: row.get(5)?,
            })
        })
        .map_err(|e| format!("Query checkpoint files: {e}"))?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("Read checkpoint file row: {e}"))?);
    }
    Ok(out)
}

fn snapshot_path(root: &Path, raw_path: &str) -> Result<SnapshotFile, String> {
    let resolved = resolve_checkpoint_path(root, raw_path)?;
    if !resolved.exists() {
        return Ok(SnapshotFile {
            summary: CodingCheckpointFile {
                path: normalized_relative_path(root, &resolved)?,
                absolute_path: resolved.to_string_lossy().to_string(),
                existed: false,
                size_bytes: 0,
                sha256: None,
                modified_at: None,
            },
            content: None,
            unix_mode: None,
        });
    }

    let meta = std::fs::symlink_metadata(&resolved)
        .map_err(|e| format!("Failed to inspect {}: {e}", resolved.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "Refusing to checkpoint symlink: {}",
            resolved.display()
        ));
    }
    if !meta.is_file() {
        return Err(format!(
            "Only files can be checkpointed: {}",
            resolved.display()
        ));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "File too large for checkpoint: {} ({:.1} MB, max {:.1} MB)",
            resolved.display(),
            meta.len() as f64 / 1_048_576.0,
            MAX_FILE_BYTES as f64 / 1_048_576.0
        ));
    }

    let mut file = std::fs::File::open(&resolved)
        .map_err(|e| format!("Failed to open {}: {e}", resolved.display()))?;
    let mut content = Vec::with_capacity(meta.len() as usize);
    file.read_to_end(&mut content)
        .map_err(|e| format!("Failed to read {}: {e}", resolved.display()))?;
    let sha256 = Some(hex::encode(Sha256::digest(&content)));
    let modified_at = meta.modified().ok().and_then(system_time_to_millis);
    let unix_mode = unix_mode(&meta);

    Ok(SnapshotFile {
        summary: CodingCheckpointFile {
            path: normalized_relative_path(root, &resolved)?,
            absolute_path: resolved.to_string_lossy().to_string(),
            existed: true,
            size_bytes: meta.len(),
            sha256,
            modified_at,
        },
        content: Some(content),
        unix_mode,
    })
}

fn restore_file(target: &Path, file: &StoredFileRow, dry_run: bool) -> Result<(), String> {
    let content = file
        .content
        .as_ref()
        .ok_or_else(|| format!("Checkpoint row for {} has no content", file.path))?;

    if dry_run {
        return Ok(());
    }

    if target.exists() {
        let meta = std::fs::symlink_metadata(target)
            .map_err(|e| format!("Failed to inspect restore target {}: {e}", target.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "Refusing to restore over symlink: {}",
                target.display()
            ));
        }
        if meta.is_dir() {
            return Err(format!(
                "Refusing to restore file over directory: {}",
                target.display()
            ));
        }
    }

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create parent {}: {e}", parent.display()))?;
    }
    std::fs::write(target, content)
        .map_err(|e| format!("Failed to restore {}: {e}", target.display()))?;
    set_unix_mode(target, file.unix_mode)?;
    Ok(())
}

fn remove_created_file(target: &Path, dry_run: bool) -> Result<(), String> {
    if dry_run {
        return Ok(());
    }
    let meta = std::fs::symlink_metadata(target)
        .map_err(|e| format!("Failed to inspect restore target {}: {e}", target.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("Refusing to delete symlink: {}", target.display()));
    }
    if meta.is_dir() {
        return Err(format!(
            "Refusing to delete directory for absent-file checkpoint: {}",
            target.display()
        ));
    }
    std::fs::remove_file(target)
        .map_err(|e| format!("Failed to delete created file {}: {e}", target.display()))
}

fn validate_workspace_root(root: &str) -> Result<PathBuf, String> {
    validate_path_string(root, "workspace_root")?;
    let path = Path::new(root);
    if !path.exists() {
        return Err(format!("Workspace root does not exist: {root}"));
    }
    if !path.is_dir() {
        return Err(format!("Workspace root is not a directory: {root}"));
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|e| format!("Resolve workspace root: {e}"))?;
    Ok(canonical)
}

fn resolve_checkpoint_path(root: &Path, raw_path: &str) -> Result<PathBuf, String> {
    validate_path_string(raw_path, "path")?;
    let raw = Path::new(raw_path);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    };
    ensure_existing_ancestor_inside(root, &candidate)?;

    if candidate.exists() {
        let canonical = std::fs::canonicalize(&candidate)
            .map_err(|e| format!("Resolve checkpoint path {}: {e}", candidate.display()))?;
        if !canonical.starts_with(root) {
            return Err(format!(
                "Path resolves outside workspace root: {}",
                candidate.display()
            ));
        }
        return Ok(canonical);
    }

    if !candidate.starts_with(root) {
        return Err(format!(
            "Path is outside workspace root: {}",
            candidate.display()
        ));
    }
    Ok(candidate)
}

fn resolve_relative_target(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    validate_path_string(relative_path, "path")?;
    let target = root.join(relative_path);
    ensure_existing_ancestor_inside(root, &target)?;
    if !target.starts_with(root) {
        return Err(format!("Path is outside workspace root: {relative_path}"));
    }
    Ok(target)
}

fn ensure_existing_ancestor_inside(root: &Path, path: &Path) -> Result<(), String> {
    let mut current = path.to_path_buf();
    while !current.exists() {
        if !current.pop() {
            return Err(format!(
                "No existing ancestor for path {}",
                path.to_string_lossy()
            ));
        }
    }
    let canonical = std::fs::canonicalize(&current)
        .map_err(|e| format!("Resolve path ancestor {}: {e}", current.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!(
            "Path ancestor resolves outside workspace root: {}",
            current.display()
        ));
    }
    Ok(())
}

fn normalize_restore_subset(root: &Path, paths: Vec<String>) -> Result<HashSet<String>, String> {
    let mut out = HashSet::new();
    for path in paths {
        let resolved = resolve_checkpoint_path(root, &path)?;
        out.insert(normalized_relative_path(root, &resolved)?);
    }
    Ok(out)
}

fn normalized_relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let path = path
        .strip_prefix(root)
        .map_err(|_| format!("Path is outside workspace root: {}", path.display()))?;
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::ParentDir => return Err("Path traversal ('..') not allowed".to_string()),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    if parts.is_empty() {
        return Err("Path cannot point to workspace root".to_string());
    }
    Ok(parts.join("/"))
}

fn validate_path_string(path: &str, label: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err(format!("{label}: path is empty"));
    }
    if path.len() > 4096 {
        return Err(format!("{label}: path exceeds 4096 characters"));
    }
    if path.contains('\0') {
        return Err(format!("{label}: path contains null bytes"));
    }
    for component in Path::new(path).components() {
        if matches!(component, Component::ParentDir) {
            return Err(format!("{label}: path traversal ('..') not allowed"));
        }
    }
    Ok(())
}

fn validate_id(id: &str, label: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!("{label}: value is empty"));
    }
    if id.len() > 128 {
        return Err(format!("{label}: value exceeds 128 characters"));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("{label}: invalid characters"));
    }
    Ok(())
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn system_time_to_millis(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

#[cfg(unix)]
fn unix_mode(meta: &std::fs::Metadata) -> Option<i64> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() as i64)
}

#[cfg(not(unix))]
fn unix_mode(_meta: &std::fs::Metadata) -> Option<i64> {
    None
}

#[cfg(unix)]
fn set_unix_mode(path: &Path, mode: Option<i64>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(mode) = mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode as u32))
            .map_err(|e| format!("Failed to restore permissions for {}: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_unix_mode(_path: &Path, _mode: Option<i64>) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open memory db");
        init_db_schema(&conn).expect("schema");
        conn
    }

    #[test]
    fn coding_checkpoint_create_and_restore_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("src.txt");
        std::fs::write(&path, "before").expect("write before");
        let mut conn = memory_conn();

        let checkpoint = create_checkpoint(
            &mut conn,
            CreateCodingCheckpointRequest {
                workspace_root: dir.path().to_string_lossy().to_string(),
                paths: vec!["src.txt".to_string()],
                session_id: Some("session-1".to_string()),
                anchor_message_id: Some("message-1".to_string()),
                conversation_anchor: Some("before edit".to_string()),
                label: Some("test".to_string()),
            },
        )
        .expect("checkpoint");

        assert_eq!(checkpoint.files.len(), 1);
        assert!(checkpoint.files[0].existed);
        assert_eq!(checkpoint.session_id.as_deref(), Some("session-1"));

        std::fs::write(&path, "after").expect("write after");
        let result = restore_checkpoint(
            &conn,
            RestoreCodingCheckpointRequest {
                checkpoint_id: checkpoint.id,
                paths: None,
                dry_run: false,
            },
        )
        .expect("restore");

        assert_eq!(result.files[0].action, "restore");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "before");
    }

    #[test]
    fn coding_checkpoint_restore_deletes_file_absent_at_checkpoint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("new.txt");
        let mut conn = memory_conn();

        let checkpoint = create_checkpoint(
            &mut conn,
            CreateCodingCheckpointRequest {
                workspace_root: dir.path().to_string_lossy().to_string(),
                paths: vec!["new.txt".to_string()],
                session_id: None,
                anchor_message_id: None,
                conversation_anchor: None,
                label: None,
            },
        )
        .expect("checkpoint");
        assert!(!checkpoint.files[0].existed);

        std::fs::write(&path, "created later").expect("write new");
        let result = restore_checkpoint(
            &conn,
            RestoreCodingCheckpointRequest {
                checkpoint_id: checkpoint.id,
                paths: None,
                dry_run: false,
            },
        )
        .expect("restore");

        assert_eq!(result.files[0].action, "delete");
        assert!(!path.exists());
    }

    #[test]
    fn coding_checkpoint_rejects_traversal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut conn = memory_conn();
        let err = create_checkpoint(
            &mut conn,
            CreateCodingCheckpointRequest {
                workspace_root: dir.path().to_string_lossy().to_string(),
                paths: vec!["../secret.txt".to_string()],
                session_id: None,
                anchor_message_id: None,
                conversation_anchor: None,
                label: None,
            },
        )
        .expect_err("traversal rejected");

        assert!(err.contains("traversal"));
    }

    #[test]
    fn coding_checkpoint_dry_run_does_not_mutate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("src.txt");
        std::fs::write(&path, "before").expect("write before");
        let mut conn = memory_conn();
        let checkpoint = create_checkpoint(
            &mut conn,
            CreateCodingCheckpointRequest {
                workspace_root: dir.path().to_string_lossy().to_string(),
                paths: vec!["src.txt".to_string()],
                session_id: None,
                anchor_message_id: None,
                conversation_anchor: None,
                label: None,
            },
        )
        .expect("checkpoint");

        std::fs::write(&path, "after").expect("write after");
        restore_checkpoint(
            &conn,
            RestoreCodingCheckpointRequest {
                checkpoint_id: checkpoint.id,
                paths: None,
                dry_run: true,
            },
        )
        .expect("dry run");

        assert_eq!(std::fs::read_to_string(&path).expect("read"), "after");
    }
}
