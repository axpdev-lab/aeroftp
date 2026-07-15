// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Transfer-queue journal (TQ-7a): atomic persistence + restart detection.
//!
//! Mirrors the SyncJournal pattern (sync.rs) for a single global queue file at
//! `aeroftp_data_root()/transfer-queue/queue.json`. Stores re-executable
//! descriptors only; no byte-range resume and no auto-reconnect on startup.
//! Frontend restore wiring is TQ-7b.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Mutex to prevent concurrent journal writes from corrupting the file.
static QUEUE_JOURNAL_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueItemDirection {
    Upload,
    Download,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueItemStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

/// Re-executable transfer descriptor for restart recovery.
///
/// `profile_id` is the stable saved-server id (no secrets). `None` means
/// ad-hoc / quick-connect with no saved profile; TQ-7b decides how to prompt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransferQueueJournalEntry {
    pub id: String,
    pub direction: QueueItemDirection,
    pub local_path: String,
    pub remote_path: String,
    /// Which saved server/profile this transfer belongs to. Stable id, not secrets.
    pub profile_id: Option<String>,
    pub filename: String,
    pub size: u64,
    pub is_folder: bool,
    pub status: QueueItemStatus,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransferQueueJournal {
    pub updated_at: DateTime<Utc>,
    pub entries: Vec<TransferQueueJournalEntry>,
}

/// Directory that holds the single global queue journal file.
pub fn queue_journal_dir() -> Result<PathBuf, String> {
    let dir = crate::portable::aeroftp_data_root()
        .ok_or_else(|| "Cannot determine AeroFTP data root".to_string())?
        .join("transfer-queue");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create transfer-queue journal directory: {}", e))?;
    Ok(dir)
}

fn journal_file_path(dir: &Path) -> PathBuf {
    dir.join("queue.json")
}

/// Atomic write: write to temp file, then rename to target path.
/// Replicated from sync.rs (module-private there; do not make it pub).
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, data)
        .map_err(|e| format!("Failed to write temp file {}: {}", tmp_path.display(), e))?;
    std::fs::rename(&tmp_path, path).map_err(|e| {
        format!(
            "Failed to rename {} to {}: {}",
            tmp_path.display(),
            path.display(),
            e
        )
    })?;
    Ok(())
}

fn ensure_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("Failed to create transfer-queue journal directory: {}", e))
}

/// Save the queue journal under `dir` (testable path override).
fn save_transfer_queue_journal_to(
    dir: &Path,
    entries: Vec<TransferQueueJournalEntry>,
) -> Result<(), String> {
    let _lock = QUEUE_JOURNAL_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ensure_dir(dir)?;
    let path = journal_file_path(dir);
    let journal = TransferQueueJournal {
        updated_at: Utc::now(),
        entries,
    };
    let data = serde_json::to_string_pretty(&journal)
        .map_err(|e| format!("Failed to serialize transfer-queue journal: {}", e))?;
    atomic_write(&path, data.as_bytes())?;
    Ok(())
}

/// Load the queue journal from `dir`. `Ok(None)` if the file is absent.
fn load_transfer_queue_journal_from(dir: &Path) -> Result<Option<TransferQueueJournal>, String> {
    let path = journal_file_path(dir);
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read transfer-queue journal: {}", e))?;
    let journal: TransferQueueJournal = serde_json::from_str(&data)
        .map_err(|e| format!("Failed to parse transfer-queue journal: {}", e))?;
    Ok(Some(journal))
}

fn clear_transfer_queue_journal_at(dir: &Path) -> Result<(), String> {
    let _lock = QUEUE_JOURNAL_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = journal_file_path(dir);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to delete transfer-queue journal: {}", e))?;
    }
    Ok(())
}

/// Filter non-terminal entries from a loaded journal (restart detection).
fn filter_interrupted(entries: Vec<TransferQueueJournalEntry>) -> Vec<TransferQueueJournalEntry> {
    entries
        .into_iter()
        .filter(|e| {
            !matches!(
                e.status,
                QueueItemStatus::Completed | QueueItemStatus::Cancelled
            )
        })
        .collect()
}

fn interrupted_entries_from(dir: &Path) -> Result<Vec<TransferQueueJournalEntry>, String> {
    let Some(journal) = load_transfer_queue_journal_from(dir)? else {
        return Ok(Vec::new());
    };
    Ok(filter_interrupted(journal.entries))
}

/// Persist the current transfer queue (creates or overwrites).
pub fn save_transfer_queue_journal(entries: Vec<TransferQueueJournalEntry>) -> Result<(), String> {
    save_transfer_queue_journal_to(&queue_journal_dir()?, entries)
}

/// Load the transfer queue journal. `Ok(None)` if no journal file exists yet.
pub fn load_transfer_queue_journal() -> Result<Option<TransferQueueJournal>, String> {
    load_transfer_queue_journal_from(&queue_journal_dir()?)
}

/// Entries that are not terminal (restart detection).
///
/// Returns Pending / InProgress / Failed; never Completed or Cancelled.
/// Intended for TQ-7b / Rust callers; not yet registered as a Tauri command
/// (frontend can also filter from `load_transfer_queue_journal`).
#[allow(dead_code)] // Public restart-detection API; wired by TQ-7b
pub fn interrupted_entries() -> Result<Vec<TransferQueueJournalEntry>, String> {
    interrupted_entries_from(&queue_journal_dir()?)
}

/// Remove the journal file entirely.
pub fn clear_transfer_queue_journal() -> Result<(), String> {
    clear_transfer_queue_journal_at(&queue_journal_dir()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_entry(
        id: &str,
        status: QueueItemStatus,
        profile_id: Option<&str>,
    ) -> TransferQueueJournalEntry {
        TransferQueueJournalEntry {
            id: id.to_string(),
            direction: QueueItemDirection::Upload,
            local_path: format!("/tmp/{}", id),
            remote_path: format!("/remote/{}", id),
            profile_id: profile_id.map(|s| s.to_string()),
            filename: format!("{}.bin", id),
            size: 1024,
            is_folder: false,
            status,
            attempts: 0,
            last_error: None,
        }
    }

    #[test]
    fn round_trip_save_and_load() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let entries = vec![
            sample_entry("a", QueueItemStatus::Pending, Some("srv_1")),
            sample_entry("b", QueueItemStatus::InProgress, None),
            sample_entry("c", QueueItemStatus::Completed, Some("srv_2")),
            sample_entry("d", QueueItemStatus::Failed, Some("srv_1")),
            sample_entry("e", QueueItemStatus::Cancelled, None),
        ];
        save_transfer_queue_journal_to(dir, entries.clone()).unwrap();
        let loaded = load_transfer_queue_journal_from(dir).unwrap().unwrap();
        assert_eq!(loaded.entries, entries);
        assert!(loaded.updated_at <= Utc::now());
    }

    #[test]
    fn interrupted_entries_excludes_terminal_statuses() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let entries = vec![
            sample_entry("pending", QueueItemStatus::Pending, Some("p1")),
            sample_entry("running", QueueItemStatus::InProgress, Some("p1")),
            sample_entry("done", QueueItemStatus::Completed, Some("p1")),
            sample_entry("fail", QueueItemStatus::Failed, Some("p1")),
            sample_entry("cancel", QueueItemStatus::Cancelled, Some("p1")),
        ];
        save_transfer_queue_journal_to(dir, entries).unwrap();
        let interrupted = interrupted_entries_from(dir).unwrap();
        let ids: Vec<&str> = interrupted.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["pending", "running", "fail"]);
        assert!(interrupted.iter().all(|e| !matches!(
            e.status,
            QueueItemStatus::Completed | QueueItemStatus::Cancelled
        )));
    }

    #[test]
    fn load_absent_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        let loaded = load_transfer_queue_journal_from(tmp.path()).unwrap();
        assert!(loaded.is_none());
        let interrupted = interrupted_entries_from(tmp.path()).unwrap();
        assert!(interrupted.is_empty());
    }

    #[test]
    fn save_twice_overwrites_cleanly_no_temp_leftover() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        save_transfer_queue_journal_to(
            dir,
            vec![sample_entry("first", QueueItemStatus::Pending, None)],
        )
        .unwrap();
        save_transfer_queue_journal_to(
            dir,
            vec![
                sample_entry("second", QueueItemStatus::Failed, Some("p2")),
                sample_entry("third", QueueItemStatus::InProgress, Some("p2")),
            ],
        )
        .unwrap();

        let loaded = load_transfer_queue_journal_from(dir).unwrap().unwrap();
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].id, "second");
        assert_eq!(loaded.entries[1].id, "third");

        // No leftover .tmp from atomic write.
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "tmp")
                    .unwrap_or(false)
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "unexpected temp leftovers: {:?}",
            leftovers
        );
        // Only queue.json should remain.
        let files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "queue.json");
    }

    #[test]
    fn clear_removes_journal_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        save_transfer_queue_journal_to(
            dir,
            vec![sample_entry("x", QueueItemStatus::Pending, None)],
        )
        .unwrap();
        assert!(journal_file_path(dir).exists());
        clear_transfer_queue_journal_at(dir).unwrap();
        assert!(!journal_file_path(dir).exists());
        assert!(load_transfer_queue_journal_from(dir).unwrap().is_none());
    }
}
