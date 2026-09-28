// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroCloud Sync Service
// Background synchronization between local and remote folders
// Supports multi-protocol providers: FTP, WebDAV, S3, etc.
// NOTE: Some items prepared for Phase 5+ background sync loop
#![allow(dead_code)]
#![allow(unused_imports)]

use crate::cloud_config::{
    validate_compress_level, CloudConfig, CloudSyncStatus, ConflictStrategy,
};
use crate::cloud_provider_factory;
use crate::compress_overlay_provider::CompressOverlayProvider;
use crate::credential_store;
use crate::crypt_overlay_provider;
use crate::providers::{ProviderError, RemoteEntry as ProviderRemoteEntry, StorageProvider};
use crate::sync::{
    build_comparison_results_with_index, decide_sync_action, load_sync_index,
    normalize_relative_key, save_sync_index, validate_relative_path, CompareDirection,
    CompareOptions, FileComparison, FileInfo, SyncAction, SyncIndex, SyncIndexEntry, SyncStatus,
    SYNC_INDEX_VERSION,
};
// file_watcher module available for Phase 3A+ watcher integration
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, Mutex, RwLock};

/// Sync task to be executed
#[derive(Debug, Clone)]
pub enum SyncTask {
    /// Full sync of all files
    FullSync,
    /// Sync specific files that changed
    IncrementalSync { paths: Vec<PathBuf> },
    /// Download specific file
    Download {
        remote_path: String,
        local_path: PathBuf,
    },
    /// Upload specific file
    Upload {
        local_path: PathBuf,
        remote_path: String,
    },
    /// Stop the service
    Stop,
}

/// Generate a Dropbox-style conflict filename.
/// Example: `report.pdf` → `report (AeroCloud conflict 2026-03-26 14-30-22 myhost).pdf`
fn conflict_rename(local_path: &Path) -> String {
    let stem = local_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = local_path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let ts = chrono::Utc::now().format("%Y-%m-%d %H-%M-%S");
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    format!("{} (AeroCloud conflict {} {}){}", stem, ts, host, ext)
}

/// Archive a local file to `.aeroversions/` before a sync deletes it, mirroring
/// the archive-before-overwrite path. Without this, a delete propagated from a
/// remote-side disappearance (`SyncAction::DeleteLocal`) was permanent, while an
/// overwrite of the same file WAS recoverable — an inconsistent gap. Best-effort:
/// a failed archive is logged, not fatal, matching the overwrite path.
/// CLAUDE-AV-B3-08.
fn archive_before_propagated_delete(config: &CloudConfig, local_path: &Path) {
    let versioning = crate::sync_versioning::SyncVersioning::new(
        &config.local_folder,
        config.versioning_strategy.clone(),
    );
    if versioning.is_enabled() {
        if let Err(e) = versioning.archive(local_path) {
            tracing::warn!("Versioning archive before delete failed: {}", e);
        }
    }
}

/// Result of a sync operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncedFileDetail {
    pub path: String,
    pub direction: String,
    pub size: u64,
}

/// One entry the cycle decided not to move, and why.
///
/// `skipped` is a single number over several different situations: an object
/// whose name this platform cannot represent, a directory that needs nothing
/// done, a file the rules exclude. Reported from real use: "the junk files left
/// on the server show up as skipped: 2 every run, with no explanation", and the
/// count was about to become more ambiguous rather than less, because once
/// directories stop being counted as uploads they land in this same number.
///
/// The reason already exists on the comparison (`sync_reason`) and was being
/// discarded at the moment of counting. Keeping it costs one push per skip and
/// turns a number you have to guess at into one you can read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedEntry {
    pub path: String,
    pub reason: String,
    pub is_dir: bool,
}

/// Result of a sync operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncOperationResult {
    pub uploaded: u32,
    pub downloaded: u32,
    pub deleted: u32,
    pub skipped: u32,
    /// What `skipped` is made of. `serde(default)` so a result serialized by an
    /// older build still deserializes.
    #[serde(default)]
    pub skipped_details: Vec<SkippedEntry>,
    pub conflicts: u32,
    pub errors: Vec<String>,
    pub duration_secs: u64,
    pub file_details: Vec<SyncedFileDetail>,
}

/// The scanned + compared sync plan produced by `build_sync_plan_with_provider`.
/// Internal seam shared by the real executor and the `--dry-run` preview.
struct SyncPlan {
    comparisons: Vec<FileComparison>,
    index: Option<SyncIndex>,
    local_str: String,
    remote_str: String,
}

/// A file conflict that needs resolution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileConflict {
    pub relative_path: String,
    pub local_modified: Option<DateTime<Utc>>,
    pub remote_modified: Option<DateTime<Utc>>,
    pub local_size: u64,
    pub remote_size: u64,
    pub status: SyncStatus,
}

/// Cloud Sync Service state
pub struct CloudService {
    config: Arc<RwLock<CloudConfig>>,
    status: Arc<RwLock<CloudSyncStatus>>,
    conflicts: Arc<RwLock<Vec<FileConflict>>>,
    task_tx: Option<mpsc::Sender<SyncTask>>,
    app_handle: Option<AppHandle>,
    /// What each transfer of the running cycle left (relative path), for the
    /// baseline.
    landed: Arc<std::sync::Mutex<HashMap<String, Landed>>>,
    /// Files of the running cycle that kept changing while they uploaded
    /// (relative path): the cycle reports them once its baseline is saved.
    unsettled: Arc<std::sync::Mutex<Vec<String>>>,
    /// Files a previous cycle left unsettled (their baseline is
    /// `UNSETTLED_BASELINE`), read with the index each plan loads.
    left_unsettled: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// How long an upload waits for its file to be left alone (see
    /// `SEND_QUIET_WINDOW`), a field so a test can give it a length.
    send_quiet_window: std::time::Duration,
    /// See `SEND_QUIET_BOUND`, a field for the same reason.
    send_quiet_bound: std::time::Duration,
}

/// What a transfer left, for the baseline: the time it left on the local file
/// and, when it is not the scan's, the size it sent.
#[derive(Debug, Clone, Copy)]
struct Landed {
    size: Option<u64>,
    modified: DateTime<Utc>,
}

/// A local file's state as an upload reads it (see
/// `CloudService::local_file_state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalFileState {
    len: u64,
    modified: Option<DateTime<Utc>>,
    identity: (u64, i64, i64),
}

/// How many times one cycle sends a file that keeps changing while it goes
/// up. After that the cycle leaves it: the next one reads a conflict, where a
/// remote copy recorded as the synced one would hide the edit.
const UPLOAD_SENDS_WHILE_EDITED: usize = 3;

/// How long a file must have been left alone before an upload reads it. A
/// write inside the tick of the file's timestamps moves none of them (the
/// change time moves at the kernel's timer tick on Linux, and shares the 2 s
/// time field on FAT), so a file read sooner could change unseen. Zero under
/// test, whose files are written by the test itself right before.
const SEND_QUIET_WINDOW: std::time::Duration = if cfg!(test) {
    std::time::Duration::ZERO
} else {
    std::time::Duration::from_secs(2)
};

/// The longest an upload waits for its file to be left alone, over all its
/// sends in a cycle: a file written without pause is then sent as it is, and
/// the resend loop reads it again without waiting any more.
const SEND_QUIET_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

/// How far ahead of the clock a file's time may be and still read as a change
/// now: FAT rounds the last write up to its 2 s field, and a share's clock
/// can run ahead (on Unix its change time comes from the server too).
const AHEAD_READ_AS_NOW: std::time::Duration = std::time::Duration::from_secs(2);

/// The baseline time of a file that kept changing while it uploaded: a time
/// neither side has, so the next cycle reads both as changed, a conflict.
const UNSETTLED_BASELINE: DateTime<Utc> = DateTime::<Utc>::UNIX_EPOCH;

impl CloudService {
    /// Create a new cloud service
    pub fn new() -> Self {
        Self {
            config: Arc::new(RwLock::new(CloudConfig::default())),
            status: Arc::new(RwLock::new(CloudSyncStatus::NotConfigured)),
            conflicts: Arc::new(RwLock::new(Vec::new())),
            task_tx: None,
            app_handle: None,
            landed: Arc::default(),
            unsettled: Arc::default(),
            left_unsettled: Arc::default(),
            send_quiet_window: SEND_QUIET_WINDOW,
            send_quiet_bound: SEND_QUIET_BOUND,
        }
    }

    /// Note the time a transfer left on the local file, read now: the
    /// baseline records it, and a local edit made later in the cycle stays a
    /// change the next cycle sees.
    fn note_landed(&self, config: &CloudConfig, relative_path: &str) {
        let on_disk = std::fs::metadata(config.local_folder.join(relative_path))
            .and_then(|meta| meta.modified())
            .ok()
            .map(DateTime::<Utc>::from);
        if let Some(modified) = on_disk {
            self.note_landed_as(
                relative_path,
                Landed {
                    size: None,
                    modified,
                },
            );
        }
    }

    /// Note what a transfer left as the caller established it, without
    /// reading the disk again.
    fn note_landed_as(&self, relative_path: &str, what: Landed) {
        if let Ok(mut landed) = self.landed.lock() {
            landed.insert(relative_path.to_string(), what);
        }
    }

    /// The local file as an upload reads it: size and time, and on Unix its
    /// inode and change time. The change time moves on an in-place save that
    /// puts the time back, the inode on a save that renames a new file over
    /// the old one. Neither moves inside the tick of the timestamps
    /// themselves (on FAT the change time shares the 2 s time field, exFAT
    /// keeps 10 ms, Linux moves it at the kernel's timer tick): the quiet
    /// window before each send covers that. Stores through a shared memory
    /// map to pages already dirty move no time at all, and no reading of the
    /// metadata sees them.
    fn local_file_state(path: &Path) -> Option<LocalFileState> {
        let meta = std::fs::metadata(path).ok()?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            (meta.ino(), meta.ctime(), meta.ctime_nsec())
        };
        #[cfg(not(unix))]
        let identity = (0, 0, 0);
        Some(LocalFileState {
            len: meta.len(),
            modified: meta.modified().ok().map(DateTime::<Utc>::from),
            identity,
        })
    }

    /// Wait until `path` has been left alone for `window`, until `deadline`
    /// at the latest (see `SEND_QUIET_WINDOW`). A file that cannot be read is
    /// not waited for: the upload reports what is wrong with it.
    async fn wait_until_quiet(
        path: &Path,
        window: std::time::Duration,
        deadline: tokio::time::Instant,
    ) {
        while let Some(quiet_for) = Self::quiet_for(path) {
            let now = tokio::time::Instant::now();
            if quiet_for >= window || now >= deadline {
                return;
            }
            tokio::time::sleep((window - quiet_for).min(deadline - now)).await;
        }
    }

    /// How long ago `path` last changed, by its time and, on Unix, its change
    /// time (see `quiet_age`).
    fn quiet_for(path: &Path) -> Option<std::time::Duration> {
        let meta = std::fs::metadata(path).ok()?;
        #[cfg(unix)]
        let changed = {
            use std::os::unix::fs::MetadataExt;
            u64::try_from(meta.ctime()).ok().map(|secs| {
                std::time::UNIX_EPOCH
                    + std::time::Duration::new(secs, meta.ctime_nsec().clamp(0, 999_999_999) as u32)
            })
        };
        #[cfg(not(unix))]
        let changed = None;
        Self::quiet_age(
            std::time::SystemTime::now(),
            [meta.modified().ok(), changed],
        )
    }

    /// The age at `now` of the youngest of `times`. A time at most
    /// `AHEAD_READ_AS_NOW` ahead of the clock is a change now; one further
    /// ahead (a stamp from a server whose clock is well ahead, a copied time)
    /// says nothing about the last write and is left out.
    fn quiet_age(
        now: std::time::SystemTime,
        times: [Option<std::time::SystemTime>; 2],
    ) -> Option<std::time::Duration> {
        times
            .into_iter()
            .flatten()
            .filter_map(|time| match now.duration_since(time) {
                Ok(age) => Some(age),
                Err(ahead) if ahead.duration() <= AHEAD_READ_AS_NOW => {
                    Some(std::time::Duration::ZERO)
                }
                Err(_) => None,
            })
            .min()
    }

    fn forget_landed(&self) {
        if let Ok(mut landed) = self.landed.lock() {
            landed.clear();
        }
    }

    /// The files of the cycle that kept changing while they uploaded, taken:
    /// the cycle ends with none left.
    fn take_unsettled(&self) -> Vec<String> {
        self.unsettled
            .lock()
            .map(|mut unsettled| std::mem::take(&mut *unsettled))
            .unwrap_or_default()
    }

    /// Initialize with config and optional app handle for status events
    pub async fn init(&self, config: CloudConfig) {
        let mut cfg = self.config.write().await;
        *cfg = config;

        if cfg.enabled {
            let mut status = self.status.write().await;
            *status = CloudSyncStatus::Idle {
                last_sync: cfg.last_sync,
                next_sync: None,
            };
        }
    }

    /// Set app handle for emitting status change events
    pub fn set_app_handle(&mut self, handle: AppHandle) {
        self.app_handle = Some(handle);
    }

    /// Get current sync status
    pub async fn get_status(&self) -> CloudSyncStatus {
        self.status.read().await.clone()
    }

    /// Set sync status and emit event
    pub async fn set_status(&self, new_status: CloudSyncStatus) {
        let mut status = self.status.write().await;
        *status = new_status.clone();

        // Emit status change event
        if let Some(app) = &self.app_handle {
            let _ = app.emit("cloud_status_change", &new_status);
        }
    }

    /// Emit `cloud-reauth-required` when a provider error signals a revoked or
    /// invalid OAuth token (4shared OAuth 1.0a cannot be refreshed silently).
    ///
    /// Returns `true` if the notification was emitted so callers can decide
    /// whether to abort the current sync cycle. Returns `false` otherwise.
    fn notify_reauth_if_token_revoked(&self, provider: &str, error_message: &str) -> bool {
        if !error_message.contains("token_revoked") {
            return false;
        }
        if let Some(app) = &self.app_handle {
            let _ = app.emit(
                "cloud-reauth-required",
                serde_json::json!({
                    "provider": provider,
                    "reason": "token_revoked",
                    "message": error_message,
                }),
            );
        }
        true
    }

    /// Get pending conflicts
    pub async fn get_conflicts(&self) -> Vec<FileConflict> {
        self.conflicts.read().await.clone()
    }

    /// Clear conflicts
    pub async fn clear_conflicts(&self) {
        let mut conflicts = self.conflicts.write().await;
        conflicts.clear();
    }

    // --- Task 1/2 helpers: index-aware sync + safety gate + post-sync baseline ---

    fn load_index(&self, config: &CloudConfig) -> Option<SyncIndex> {
        let local = config.local_folder.to_string_lossy().to_string();
        let remote = config.remote_folder.clone();
        load_sync_index(&local, &remote).ok().flatten()
    }

    /// Resolve the action for a comparison EXACTLY as the executor does, so the
    /// post-sync baseline is derived from the same decision that actually ran.
    /// Conflicts/size mismatches go through the configured conflict strategy;
    /// every other status goes through the central `decide_sync_action`.
    /// Directory deletes are downgraded to `Skip`: AeroCloud removes files, not
    /// directories, so counting or executing a dir delete would inflate the
    /// report and orphan the tree.
    fn resolve_action(&self, config: &CloudConfig, comparison: &FileComparison) -> SyncAction {
        // On Windows a key containing `\` cannot name a file of its own: the
        // local scan keys with `/`, so such a key only comes from a remote
        // object whose name holds a literal backslash, and `join` would turn it
        // into a path that aliases the nested file (`sub\b.txt` IS
        // `sub/b.txt`). Downloading it overwrites that file with another
        // object's contents, and deleting it acts on a name the user cannot
        // see. Before G111 this scanner created exactly such objects on Unix
        // servers; they are left alone here rather than guessed at.
        #[cfg(windows)]
        if comparison.relative_path.contains('\\') {
            return SyncAction::Skip;
        }
        let left_unsettled = || {
            self.left_unsettled
                .lock()
                .is_ok_and(|left| left.contains(&comparison.relative_path))
        };
        let action = match &comparison.status {
            // A file a previous cycle left unsettled: the remote holds one of
            // its sends, possibly torn, and a backend that sets its own time
            // makes that send look newer. It is asked, not settled by time or
            // by a preference for the remote; under PreferLocal the local
            // file, complete, is still sent.
            SyncStatus::Conflict | SyncStatus::SizeMismatch
                if matches!(
                    config.conflict_strategy,
                    ConflictStrategy::PreferNewer | ConflictStrategy::PreferRemote
                ) && left_unsettled() =>
            {
                SyncAction::AskUser
            }
            SyncStatus::Conflict | SyncStatus::SizeMismatch => match config.conflict_strategy {
                ConflictStrategy::AskUser => SyncAction::AskUser,
                ConflictStrategy::KeepBoth => SyncAction::KeepBoth,
                ConflictStrategy::PreferLocal => SyncAction::Upload,
                ConflictStrategy::PreferRemote => SyncAction::Download,
                ConflictStrategy::PreferNewer => {
                    let local_time = comparison.local_info.as_ref().and_then(|i| i.modified);
                    let remote_time = comparison.remote_info.as_ref().and_then(|i| i.modified);
                    match (local_time, remote_time) {
                        (Some(l), Some(r)) if l > r => SyncAction::Upload,
                        (Some(l), Some(r)) if r > l => SyncAction::Download,
                        _ => SyncAction::AskUser,
                    }
                }
            },
            _ => decide_sync_action(
                &comparison.status,
                &config.sync_direction,
                comparison.previously_synced,
                config.preserve_remote_deletes,
            ),
        };
        if comparison.is_dir && matches!(action, SyncAction::DeleteLocal | SyncAction::DeleteRemote)
        {
            return SyncAction::Skip;
        }
        action
    }

    /// Note which files a previous cycle left unsettled (see
    /// `left_unsettled`).
    fn remember_unsettled(&self, index: Option<&SyncIndex>) {
        let unsettled = index
            .map(|index| {
                index
                    .files
                    .iter()
                    .filter(|(_, entry)| entry.modified == Some(UNSETTLED_BASELINE))
                    .map(|(path, _)| path.clone())
                    .collect()
            })
            .unwrap_or_default();
        if let Ok(mut left) = self.left_unsettled.lock() {
            *left = unsettled;
        }
    }

    /// How many files would have a delete propagated this cycle.
    fn count_pending_deletes(&self, config: &CloudConfig, comparisons: &[FileComparison]) -> usize {
        comparisons
            .iter()
            .filter(|c| {
                matches!(
                    self.resolve_action(config, c),
                    SyncAction::DeleteLocal | SyncAction::DeleteRemote
                )
            })
            .count()
    }

    /// Whether the delete-propagation safety gate should trip this cycle. Trips
    /// when a prior baseline existed AND either the scan that produced the
    /// listings was incomplete, or a whole side is empty, or the pending deletes
    /// exceed half of the prior baseline: a mass disappearance is far more likely
    /// a transient/partial listing failure than a deliberate user delete. The
    /// floor keeps tiny folders from tripping on a legitimate "delete most of a
    /// 3-file folder".
    ///
    /// `scan_incomplete` is the only signal that does not reason about counts: a
    /// scanner that swallowed a listing error returns a map that LOOKS complete,
    /// so the files it could not see are indistinguishable from files the user
    /// deleted. Any count-based heuristic is downstream of a lie, hence the
    /// unconditional trip. (CLAUDE-AV-B3-11)
    fn delete_safety_trips(
        prior_count: usize,
        local_empty: bool,
        remote_empty: bool,
        pending_deletes: usize,
        scan_incomplete: bool,
    ) -> bool {
        const MASS_DELETE_FLOOR: usize = 10;
        if prior_count == 0 {
            return false;
        }
        if scan_incomplete {
            return true;
        }
        if local_empty || remote_empty {
            return true;
        }
        prior_count >= MASS_DELETE_FLOOR && pending_deletes * 2 > prior_count
    }

    /// Persist the post-sync baseline so the NEXT cycle can tell a deleted file
    /// (was baselined, now gone on one side) from a genuinely new file (never
    /// baselined). See [`Self::post_sync_baseline`] for how it is computed.
    fn save_post_sync_index(
        &self,
        local: &str,
        remote: &str,
        comparisons: &[FileComparison],
        result: &SyncOperationResult,
        config: &CloudConfig,
        prior_index: Option<&SyncIndex>,
    ) {
        // Only persist on a clean run (no errors) to avoid advancing the
        // "last known good" snapshot on a partial/broken cycle. The one thing
        // such a cycle still records is the sentinel of a file that kept
        // changing while it uploaded: without it the next cycle reads the
        // remote's last send, possibly torn, as the newer copy.
        if !result.errors.is_empty() {
            self.save_unsettled_into(local, remote, comparisons, prior_index);
            return;
        }
        let idx = SyncIndex {
            // Carried over from #854 when the two branches met here: the index
            // this writes is a v2 index, and a freshly written one has nothing
            // unverified in it, because `unverified_keys` marks only the keys a
            // migration rewrote on read.
            version: SYNC_INDEX_VERSION,
            last_sync: Utc::now(),
            local_path: local.to_string(),
            remote_path: remote.to_string(),
            files: self.post_sync_baseline(comparisons, config, prior_index),
            unverified_keys: Default::default(),
        };
        if let Err(e) = save_sync_index(&idx) {
            tracing::warn!("Failed to save AeroCloud sync index: {}", e);
        } else {
            tracing::debug!(
                "Saved AeroCloud sync index for pair ({} tracked files)",
                idx.files.len()
            );
        }
    }

    /// Record in the prior index, and in nothing else, the sentinel of each
    /// file of this cycle that kept changing while it uploaded.
    fn save_unsettled_into(
        &self,
        local: &str,
        remote: &str,
        comparisons: &[FileComparison],
        prior_index: Option<&SyncIndex>,
    ) {
        let unsettled = self
            .unsettled
            .lock()
            .map(|unsettled| unsettled.clone())
            .unwrap_or_default();
        if unsettled.is_empty() {
            return;
        }
        let mut idx = prior_index.cloned().unwrap_or_else(|| SyncIndex {
            version: SYNC_INDEX_VERSION,
            last_sync: Utc::now(),
            local_path: local.to_string(),
            remote_path: remote.to_string(),
            files: HashMap::new(),
            unverified_keys: Default::default(),
        });
        // The prior index was migrated when it was loaded, and the keys the
        // migration could not vouch for are not saved with it: written back
        // they would be trusted, and a file never synced would read as synced
        // (its local copy deleted the next cycle). Left out, they read as
        // never synced, as the compare reads them now.
        let unverified = std::mem::take(&mut idx.unverified_keys);
        idx.files.retain(|path, _| !unverified.contains(path));
        for path in unsettled {
            let size = comparisons
                .iter()
                .find(|c| c.relative_path == path)
                .and_then(|c| c.local_info.as_ref())
                .map_or(0, |info| info.size);
            idx.files.insert(
                path,
                SyncIndexEntry {
                    size,
                    modified: Some(UNSETTLED_BASELINE),
                    is_dir: false,
                },
            );
        }
        if let Err(e) = save_sync_index(&idx) {
            tracing::warn!("Failed to save AeroCloud sync index: {}", e);
        }
    }

    /// The baseline to persist after a clean cycle. The comparator OMITS
    /// Identical files, so the baseline is carried FORWARD from the prior index
    /// and only the changed files (the `comparisons`) are applied as deltas:
    /// deletes remove the entry, synced files upsert the source-of-truth side,
    /// and every action that yields no entry (unresolved conflicts, a one-sided
    /// Skip) leaves the prior entry untouched. Rebuilding from `comparisons`
    /// alone would drop every Identical file and silently wipe the baseline,
    /// and removing an entry on anything but a delete would turn a file that
    /// was synced back into a new one.
    fn post_sync_baseline(
        &self,
        comparisons: &[FileComparison],
        config: &CloudConfig,
        prior_index: Option<&SyncIndex>,
    ) -> HashMap<String, SyncIndexEntry> {
        let mut index_files: HashMap<String, SyncIndexEntry> =
            prior_index.map(|i| i.files.clone()).unwrap_or_default();
        for c in comparisons {
            let action = self.resolve_action(config, c);
            if matches!(action, SyncAction::DeleteLocal | SyncAction::DeleteRemote) {
                index_files.remove(&c.relative_path);
            } else if let Some(mut entry) = Self::baseline_entry_for(
                &action,
                config.sync_direction,
                c.local_info.as_ref(),
                c.remote_info.as_ref(),
                c.is_dir,
            ) {
                // The next cycle reads the local side against this time with
                // the local clock, so it must be the time the transfer left on
                // the local file: an upload is stamped with the server's time
                // on the provider path (kept, the scan's time would read as a
                // change on both sides, a conflict every cycle), and a download
                // from a backend that lists no time (FTP `LIST` dates) keeps
                // the file's own (without it, the local side would be compared
                // by size alone). It is the time read when the transfer ended,
                // not now: a same-size edit made later in the cycle must stay a
                // change the next cycle sees.
                if matches!(action, SyncAction::Upload | SyncAction::Download) && !c.is_dir {
                    if let Some(landed) = self
                        .landed
                        .lock()
                        .ok()
                        .and_then(|landed| landed.get(&c.relative_path).copied())
                    {
                        entry.modified = Some(landed.modified);
                        if let Some(size) = landed.size {
                            entry.size = size;
                        }
                    }
                }
                index_files.insert(c.relative_path.clone(), entry);
            }
        }
        index_files
    }

    /// Pick the baseline `SyncIndexEntry` for a file that stayed in sync this
    /// cycle, recording the SOURCE-of-truth side so the next cycle sees it
    /// unchanged instead of a spurious conflict:
    /// - `Upload` converged remote onto local  -> record local
    /// - `Download` converged local onto remote -> record remote
    /// - `Skip` records the AUTHORITATIVE side for the direction: local for
    ///   send-only (LocalToRemote), remote for receive-only (RemoteToLocal),
    ///   either for Bidirectional (Skip there means Identical). Recording the
    ///   authoritative side is what makes a tolerated one-sided edit in a
    ///   directional folder stay tolerated across cycles instead of being
    ///   reverted the following cycle by baseline bookkeeping.
    /// - `Skip` of a file present on ONE side only records nothing. In a
    ///   one-way folder that Skip is the decision to leave a never-synced
    ///   file alone (a local-only file under receive-only, a remote-only one
    ///   under send-only). Recording it would make the next cycle read the
    ///   same file as previously synced and, in mirror mode, delete it: the
    ///   file spared on cycle 1 would be removed on cycle 2. An entry that
    ///   already exists is left as it is by the caller, so a file that WAS
    ///   synced keeps its baseline. Directories are exempt: they are never
    ///   deleted here, and a kept one must stay tracked.
    ///
    /// Returns `None` for actions that must not advance the baseline
    /// (`AskUser`, `KeepBoth`); deletes are handled by the caller.
    fn baseline_entry_for(
        action: &SyncAction,
        direction: CompareDirection,
        local_info: Option<&FileInfo>,
        remote_info: Option<&FileInfo>,
        is_dir: bool,
    ) -> Option<SyncIndexEntry> {
        let info = match action {
            SyncAction::Upload => local_info,
            SyncAction::Download => remote_info,
            SyncAction::Skip if !is_dir && (local_info.is_none() || remote_info.is_none()) => {
                return None;
            }
            SyncAction::Skip => match direction {
                CompareDirection::RemoteToLocal => remote_info.or(local_info),
                _ => local_info.or(remote_info),
            },
            _ => None,
        };
        match info {
            Some(fi) => Some(SyncIndexEntry {
                size: fi.size,
                modified: fi.modified,
                is_dir,
            }),
            // A kept directory carries no size/mtime but must stay tracked so it
            // is not treated as new (and re-created) every cycle.
            None if is_dir && matches!(action, SyncAction::Skip) => Some(SyncIndexEntry {
                size: 0,
                modified: None,
                is_dir: true,
            }),
            None => None,
        }
    }

    /// Scan both sides, load the prior index, build the index-aware comparison
    /// set, and apply the mass-delete safety gate. Shared source of truth for
    /// the real executor ([`Self::perform_full_sync_with_provider`]) and the
    /// read-only preview ([`Self::preview_full_sync_with_provider`]) so a
    /// `--dry-run` plan is derived from byte-identical inputs to a real run.
    async fn build_sync_plan_with_provider<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        config: &CloudConfig,
        ensure_remote: bool,
    ) -> Result<SyncPlan, String> {
        // Probe the remote folder. A real run creates it if missing (check first:
        // some providers like FileLu create duplicates if mkdir is called on an
        // existing folder). A preview (`ensure_remote == false`) must NOT mutate
        // the target: if the folder is absent it is treated as empty instead.
        let remote_present = provider.cd(&config.remote_folder).await.is_ok();
        if !remote_present && ensure_remote {
            // One `mkdir` creates ONE level, so a remote folder whose parents do
            // not exist yet failed here and then failed every upload under it
            // with "No such file". Reported from real use, setting up a pair
            // whose remote folder was two levels deep. `ensure_remote_dir` walks
            // the chain top-down and absorbs the "already exists" errors; it is
            // the helper the manual sync and the DAG path already use, so this
            // stops being the one place with a weaker rule.
            crate::sync::ensure_remote_dir(provider, &config.remote_folder).await;
            if let Err(e) = provider.cd(&config.remote_folder).await {
                tracing::warn!(
                    "Failed to create remote folder {}: {}",
                    config.remote_folder,
                    e
                );
            }
        }

        // Get file listings
        // CLAUDE-AV-B3-16: local scan reports completeness (see scan_local_folder).
        let (mut local_files, local_complete) = self.scan_local_folder(config).await?;
        let (mut remote_files, remote_complete) = if remote_present || ensure_remote {
            match self
                .scan_remote_folder_with_provider(provider, config)
                .await
            {
                Ok(scanned) => scanned,
                Err(e) => {
                    // Propagate OAuth 1.0a token revocation to the UI before returning.
                    // Without this, 4shared failures during scan only surface as a generic
                    // sync error with no actionable recovery path.
                    self.notify_reauth_if_token_revoked(&config.protocol_type, &e);
                    return Err(e);
                }
            }
        } else {
            // Preview against a not-yet-created remote: nothing is there, so every
            // local file reads as new. No mkdir, no scan, no mutation. The empty
            // map is the TRUTH about that remote, not a failed listing, so it
            // counts as a complete scan.
            (HashMap::new(), true)
        };

        let local_str = config.local_folder.to_string_lossy().to_string();
        let remote_str = config.remote_folder.clone();
        Self::bound_local_scan_paths(&local_str, &mut local_files, &mut remote_files);

        // Load prior sync index (if any) for delete propagation + conflict detection.
        let index = self.load_index(config);
        self.remember_unsettled(index.as_ref());
        let prior_count = index.as_ref().map_or(0, |i| i.files.len());
        let local_len = local_files.len();
        let remote_len = remote_files.len();

        // Enable checksum comparison when provider supplies content hashes (e.g. FileLu)
        let has_checksums = remote_files.values().any(|f| f.checksum.is_some());

        // Build comparison (index-aware). Skip size comparison when the provider does NOT
        // report an exact logical size (a deferred-size crypt overlay, e.g.
        // legacy AeroCrypt v1/v2): the local plaintext size never equals the
        // on-wire ciphertext size, so comparing them would flag every unchanged
        // file as different and re-sync it every cycle. Such a provider is
        // compared on timestamp + the AEAD tag. rclone-crypt / AeroCrypt v3 and
        // every non-crypt provider report exact sizes and keep the size check.
        let options = CompareOptions {
            compare_timestamp: true,
            compare_size: provider.reports_exact_size(),
            compare_checksum: has_checksums,
            exclude_patterns: config.exclude_patterns.clone(),
            direction: config.sync_direction,
            // The scans read `.aeroignore`; the compare reads the same rule.
            aeroignore: crate::sync_ignore::AeroIgnore::load(&config.local_folder)
                .map(std::sync::Arc::new),
            modify_window: crate::sync_core::mtime::ModifyWindow::against_provider(
                None, &*provider,
            ),
            ..Default::default()
        };
        if !options.modify_window.compares_times() {
            log_size_only_window("AeroCloud", config, options.modify_window);
        }

        let mut comparisons = build_comparison_results_with_index(
            local_files,
            remote_files,
            &options,
            index.as_ref(),
        );

        // A provider whose successful listing can omit a stored object
        // (`listing_is_authoritative() == false`, ImageKit) cannot authorise
        // deleting a local file by that file's absence: the same rule
        // `sync::remote_listing_delete_guard` applies on the shared path.
        if !provider.listing_is_authoritative() {
            for c in &mut comparisons {
                if c.status == SyncStatus::LocalOnly {
                    c.previously_synced = false;
                }
            }
        }

        // Safety gate: refuse to propagate deletes when a mass disappearance
        // looks like a transient/partial listing failure (a whole side empty,
        // or deletes exceeding half the baseline) rather than a real user delete.
        // CLAUDE-AV-B3-16: either side incomplete trips the gate.
        let pending_deletes = self.count_pending_deletes(config, &comparisons);
        if Self::delete_safety_trips(
            prior_count,
            local_len == 0,
            remote_len == 0,
            pending_deletes,
            !remote_complete || !local_complete,
        ) {
            tracing::warn!(
                "AeroCloud safety gate (provider): {} pending deletes vs {} baselined (L:{} R:{}, local scan complete: {}, remote scan complete: {}). Disabling delete propagation this cycle to prevent accidental wipe.",
                pending_deletes,
                prior_count,
                local_len,
                remote_len,
                local_complete,
                remote_complete
            );
            for c in &mut comparisons {
                if c.status == SyncStatus::LocalOnly || c.status == SyncStatus::RemoteOnly {
                    c.previously_synced = false;
                }
            }
        }

        Ok(SyncPlan {
            comparisons,
            index,
            local_str,
            remote_str,
        })
    }

    /// Read-only `--dry-run` preview: scan and compare both sides exactly like a
    /// real run, then tally the action each file WOULD receive without touching
    /// the provider (no upload/download/delete) and without persisting the index.
    /// Reuses [`Self::resolve_action`], the same decision the executor runs, so
    /// the preview and a subsequent real sync agree.
    pub async fn preview_full_sync_with_provider<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
    ) -> Result<SyncOperationResult, String> {
        let config = self.config.read().await.clone();

        if !config.enabled {
            return Err("AeroCloud is not enabled".to_string());
        }

        let start_time = std::time::Instant::now();

        let SyncPlan { comparisons, .. } = self
            .build_sync_plan_with_provider(provider, &config, false)
            .await?;

        let mut result = SyncOperationResult {
            uploaded: 0,
            downloaded: 0,
            deleted: 0,
            skipped: 0,
            skipped_details: Vec::new(),
            conflicts: 0,
            errors: Vec::new(),
            duration_secs: 0,
            file_details: Vec::new(),
        };

        for comparison in &comparisons {
            // Mirror the executor: a traversal-crafted path is an error there, so
            // the preview reports it as one rather than tallying its intended action.
            if validate_relative_path(&comparison.relative_path).is_err() {
                result.errors.push(format!(
                    "{}: invalid relative path",
                    comparison.relative_path
                ));
                continue;
            }
            match self.resolve_action(&config, comparison) {
                SyncAction::AskUser => result.conflicts += 1,
                action => Self::record_sync_action(&mut result, comparison, &action),
            }
        }

        result.duration_secs = start_time.elapsed().as_secs();
        Ok(result)
    }

    /// Perform a full sync using any StorageProvider (multi-protocol support)
    /// This is the new unified sync method that works with FTP, WebDAV, S3, etc.
    pub async fn perform_full_sync_with_provider<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
    ) -> Result<SyncOperationResult, String> {
        let config = self.config.read().await.clone();

        if !config.enabled {
            return Err("AeroCloud is not enabled".to_string());
        }

        let start_time = std::time::Instant::now();

        // Update status to syncing
        self.set_status(CloudSyncStatus::Syncing {
            current_file: "Scanning files...".to_string(),
            progress: 0.0,
            files_done: 0,
            files_total: 0,
        })
        .await;

        // Scan both sides, build the index-aware comparison set, and apply the
        // mass-delete safety gate (shared with the `--dry-run` preview so both
        // derive from byte-identical inputs).
        let SyncPlan {
            comparisons,
            index,
            local_str,
            remote_str,
        } = self
            .build_sync_plan_with_provider(provider, &config, true)
            .await?;

        let total_files = comparisons.len() as u32;
        let mut result = SyncOperationResult {
            uploaded: 0,
            downloaded: 0,
            deleted: 0,
            skipped: 0,
            skipped_details: Vec::new(),
            conflicts: 0,
            errors: Vec::new(),
            duration_secs: 0,
            file_details: Vec::new(),
        };

        // Process each comparison
        // P1-7: Throttle status updates to every 100 files or 500ms to reduce lock contention
        let mut last_status_update = std::time::Instant::now();
        let status_interval = std::time::Duration::from_millis(500);
        self.forget_landed();
        for (index, comparison) in comparisons.iter().enumerate() {
            // Update progress (throttled)
            let is_last = index == comparisons.len() - 1;
            if index % 100 == 0 || is_last || last_status_update.elapsed() >= status_interval {
                self.set_status(CloudSyncStatus::Syncing {
                    current_file: comparison.relative_path.clone(),
                    progress: (index as f64 / total_files.max(1) as f64) * 100.0,
                    files_done: index as u32,
                    files_total: total_files,
                })
                .await;
                last_status_update = std::time::Instant::now();
            }

            match self
                .process_comparison_with_provider(provider, &config, comparison)
                .await
            {
                Ok(action) => match action {
                    SyncAction::AskUser => {
                        result.conflicts += 1;
                        // Add to conflicts list (capped at 10K to prevent unbounded growth)
                        let mut conflicts = self.conflicts.write().await;
                        if conflicts.len() < 10_000 {
                            conflicts.push(FileConflict {
                                relative_path: comparison.relative_path.clone(),
                                local_modified: comparison
                                    .local_info
                                    .as_ref()
                                    .and_then(|i| i.modified),
                                remote_modified: comparison
                                    .remote_info
                                    .as_ref()
                                    .and_then(|i| i.modified),
                                local_size: comparison
                                    .local_info
                                    .as_ref()
                                    .map(|i| i.size)
                                    .unwrap_or(0),
                                remote_size: comparison
                                    .remote_info
                                    .as_ref()
                                    .map(|i| i.size)
                                    .unwrap_or(0),
                                status: comparison.status.clone(),
                            });
                        }
                    }
                    _ => Self::record_sync_action(&mut result, comparison, &action),
                },
                Err(e) => {
                    result
                        .errors
                        .push(format!("{}: {}", comparison.relative_path, e));

                    // Detect token revocation (e.g. 4shared OAuth 1.0a) and notify frontend.
                    // OAuth 1.0a tokens cannot be refreshed: abort sync and prompt user.
                    if self.notify_reauth_if_token_revoked(&config.protocol_type, &e) {
                        result
                            .errors
                            .push("Sync aborted: re-authorization required".to_string());
                        break;
                    }
                }
            }
        }

        result.duration_secs = start_time.elapsed().as_secs();

        // Update last_sync in memory, drop the async guard, then RMW the on-disk
        // config under the shared file lock. Holding self.config across the
        // blocking filesystem write would pin the async worker for the whole I/O.
        let now = Utc::now();
        {
            let mut cfg = self.config.write().await;
            cfg.last_sync = Some(now);
        }
        if let Err(e) = crate::cloud_config::with_cloud_config_mut(|disk| {
            disk.last_sync = Some(now);
            Ok(())
        }) {
            tracing::warn!("Failed to persist cloud last_sync: {e}");
        }

        // Persist the post-sync baseline (index) for delete propagation on next cycles.
        self.save_post_sync_index(
            &local_str,
            &remote_str,
            &comparisons,
            &result,
            &config,
            index.as_ref(),
        );

        // After the save, which a cycle with errors skips: the baseline of a
        // file that kept changing while it uploaded is what makes the next
        // cycle read it as a conflict.
        for path in self.take_unsettled() {
            result.errors.push(format!(
                "{path}: it kept changing while it uploaded; the next cycle reads it as a conflict, asked about under PreferNewer or PreferRemote until one cycle runs under PreferLocal (sends the local copy) or KeepBoth"
            ));
        }

        // Update status
        if result.conflicts > 0 {
            self.set_status(CloudSyncStatus::HasConflicts {
                count: result.conflicts,
            })
            .await;
        } else if !result.errors.is_empty() {
            self.set_status(CloudSyncStatus::Error {
                message: format!("{} errors during sync", result.errors.len()),
            })
            .await;
        } else {
            self.set_status(CloudSyncStatus::Idle {
                last_sync: Some(Utc::now()),
                next_sync: None,
            })
            .await;
        }

        // Emit sync complete event
        if let Some(app) = &self.app_handle {
            let _ = app.emit("cloud_sync_complete", &result);
        }

        Ok(result)
    }

    fn record_sync_action(
        result: &mut SyncOperationResult,
        comparison: &FileComparison,
        action: &SyncAction,
    ) {
        match action {
            SyncAction::Upload => {
                result.uploaded += 1;
                if !comparison.is_dir {
                    result.file_details.push(SyncedFileDetail {
                        path: comparison.relative_path.clone(),
                        direction: "upload".to_string(),
                        size: comparison.local_info.as_ref().map(|i| i.size).unwrap_or(0),
                    });
                }
            }
            SyncAction::Download => {
                result.downloaded += 1;
                if !comparison.is_dir {
                    result.file_details.push(SyncedFileDetail {
                        path: comparison.relative_path.clone(),
                        direction: "download".to_string(),
                        size: comparison.remote_info.as_ref().map(|i| i.size).unwrap_or(0),
                    });
                }
            }
            SyncAction::KeepBoth => {
                result.downloaded += 1;
                if !comparison.is_dir {
                    result.file_details.push(SyncedFileDetail {
                        path: comparison.relative_path.clone(),
                        direction: "download".to_string(),
                        size: comparison.remote_info.as_ref().map(|i| i.size).unwrap_or(0),
                    });
                }
            }
            SyncAction::DeleteLocal | SyncAction::DeleteRemote => result.deleted += 1,
            SyncAction::Skip => {
                result.skipped += 1;
                result.skipped_details.push(SkippedEntry {
                    path: comparison.relative_path.clone(),
                    reason: comparison.sync_reason.clone(),
                    is_dir: comparison.is_dir,
                });
            }
            SyncAction::AskUser => {}
        }
    }

    /// A local link was skipped, not deleted. Bound both sides before comparing
    /// so its remote twin cannot authorize a delete or a download through the
    /// link. The shared bound also covers links in a file's parent directories
    /// and unreadable local prefixes. Entries outside these paths still sync;
    /// the existing baseline carry-forward retains entries we did not compare.
    fn bound_local_scan_paths(
        local_root: &str,
        locals: &mut HashMap<String, FileInfo>,
        remotes: &mut HashMap<String, FileInfo>,
    ) {
        let bound = crate::sync_core::ScanBound::for_sync(
            local_root,
            locals.keys().map(String::as_str),
            remotes.keys().map(String::as_str),
            &crate::sync_core::ScanBoundaries::default(),
            crate::sync_core::ScanBoundaries::default(),
        );
        locals.retain(|path, _| !bound.covers(path));
        remotes.retain(|path, _| !bound.covers(path));
    }

    /// Scan local folder and build file info map.
    ///
    /// Returns the local index plus whether the scan saw the WHOLE tree. A
    /// `false` completeness flag means at least one entry could not be stated
    /// (permission flake, transient FS error), a mid-listing iterator error
    /// was swallowed, or the 100K index cap truncated the walk: the map then
    /// understates the local side and must never drive delete propagation.
    /// Mirrors `scan_remote_folder_with_provider`'s completeness contract.
    /// (CLAUDE-AV-B3-16)
    async fn scan_local_folder(
        &self,
        config: &CloudConfig,
    ) -> Result<(HashMap<String, FileInfo>, bool), String> {
        let mut files = HashMap::new();
        let mut complete = true;
        let base_path = &config.local_folder;

        // Absent root stays a complete empty map: the empty-side safety gate
        // (local_empty with a prior baseline) already protects delete
        // propagation, matching the pre-existing first-run / not-yet-created
        // local-folder path. Do not reclassify it as incomplete.
        if !base_path.exists() {
            return Ok((files, true));
        }

        // Load .aeroignore from sync root (if present)
        let aeroignore = crate::sync_ignore::AeroIgnore::load(base_path);
        // An invalid configured exclude fails the cycle instead of being dropped.
        let excludes = crate::sync::compile_excludes(&config.exclude_patterns)?;

        // Recursive scan with a completeness flag: unstattable entries used to
        // be `Err(_) => continue`, which made a baselined file that became
        // unstattable look RemoteOnly and could drive DeleteRemote.
        // (CLAUDE-AV-B3-16)
        fn scan_recursive(
            base: &PathBuf,
            current: &PathBuf,
            files: &mut HashMap<String, FileInfo>,
            complete: &mut bool,
            exclude: &crate::sync_exclude::ExcludeMatcher,
            excluded_folders: &[String],
            aeroignore: Option<&crate::sync_ignore::AeroIgnore>,
        ) -> Result<(), String> {
            let entries = std::fs::read_dir(current)
                .map_err(|e| format!("Failed to read directory: {}", e))?;

            for entry_result in entries {
                // CLAUDE-AV-B3-16: was `entries.flatten()`, which silently
                // dropped mid-listing iterator errors and understated the tree.
                let entry = match entry_result {
                    Ok(e) => e,
                    Err(_) => {
                        *complete = false;
                        continue;
                    }
                };
                let path = entry.path();

                // Use symlink_metadata to avoid following symlinks (CF-007)
                let metadata = match path.symlink_metadata() {
                    Ok(m) => m,
                    Err(_) => {
                        // CLAUDE-AV-B3-16: an unstattable entry is invisible to
                        // the comparator. Keep skipping it (do not invent a
                        // fake FileInfo) but mark the scan incomplete so the
                        // delete safety gate refuses to act on the lie.
                        *complete = false;
                        continue;
                    }
                };

                // Skip symlinks entirely to prevent symlink-following attacks
                if metadata.file_type().is_symlink() {
                    continue;
                }

                // G111: the key has to use `/` on every platform. The remote
                // scanner below builds its own keys with `format!("{}/{}", ..)`,
                // and `build_comparison_results_with_index` unions the two sets
                // of keys verbatim, so a native `sub\b.txt` here would never
                // meet the `sub/b.txt` that comes back from the server: the same
                // copy reads as local-only and remote-only at once.
                //
                // This key is not only compared, it is *used*: the upload builds
                // the destination as `format!("{}/{}", remote_folder, key)`, so
                // the native form would put the file at `/remote/sub\b.txt` on
                // the server. `excluded_folders` also matches it with
                // `starts_with("{folder}/")`, so a selective-sync rule on a
                // nested folder never fired on Windows.
                //
                // (`should_exclude` is NOT part of the argument: it already
                // splits on both separators and normalizes multi-segment
                // patterns, so ignore rules were never affected.)
                //
                // The sibling scanners (`aeroftp_cli.rs`, and the two in
                // `lib.rs` that feed the same comparison) have always done this;
                // this one was the outlier.
                let relative = normalize_relative_key(
                    &path
                        .strip_prefix(base)
                        .map_err(|e| e.to_string())?
                        .to_string_lossy(),
                );

                let is_dir = metadata.is_dir();

                // Check exclusions: .aeroignore first (with negation), then config patterns
                let decision =
                    crate::sync_ignore::scan_decision(aeroignore, &relative, is_dir, exclude);
                if decision == crate::sync_ignore::ScanDecision::Skip {
                    continue;
                }

                // Selective sync: skip directories listed in excluded_folders
                if is_dir
                    && excluded_folders.iter().any(|ef| {
                        let ef_norm = ef.trim_matches('/');
                        relative == ef_norm || relative.starts_with(&format!("{}/", ef_norm))
                    })
                {
                    continue;
                }
                if decision == crate::sync_ignore::ScanDecision::WalkOnly {
                    scan_recursive(
                        base,
                        &path,
                        files,
                        complete,
                        exclude,
                        excluded_folders,
                        aeroignore,
                    )?;
                    continue;
                }

                let modified = metadata.modified().ok().map(DateTime::<Utc>::from);

                let size = if is_dir { 0 } else { metadata.len() };

                // P1-6: Cap file index at 100K to prevent unbounded memory growth.
                // A truncated scan is an INCOMPLETE scan: report it so the delete
                // gate refuses to act on it. Mirrors the remote scanner.
                // (CLAUDE-AV-B3-16)
                if files.len() >= 100_000 {
                    tracing::warn!("Local file index cap reached (100K), truncating scan");
                    *complete = false;
                    return Ok(());
                }

                files.insert(
                    relative.clone(),
                    FileInfo {
                        name: entry.file_name().to_string_lossy().to_string(),
                        path: path.to_string_lossy().to_string(),
                        size,
                        modified,
                        is_dir,
                        checksum_alg: None,
                        checksum: None,
                    },
                );

                if is_dir {
                    scan_recursive(
                        base,
                        &path,
                        files,
                        complete,
                        exclude,
                        excluded_folders,
                        aeroignore,
                    )?;
                }
            }

            Ok(())
        }

        scan_recursive(
            base_path,
            base_path,
            &mut files,
            &mut complete,
            &excludes,
            &config.excluded_folders,
            aeroignore.as_ref(),
        )?;
        Ok((files, complete))
    }

    /// Scan remote folder using any StorageProvider (multi-protocol support)
    /// Returns the remote index plus whether the scan saw the WHOLE tree. A
    /// `false` completeness flag means at least one directory was skipped (a
    /// failed cd/list, the depth limit, the index cap), so the map understates
    /// the remote and must never drive delete propagation. (CLAUDE-AV-B3-11)
    async fn scan_remote_folder_with_provider<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        config: &CloudConfig,
    ) -> Result<(HashMap<String, FileInfo>, bool), String> {
        let mut files = HashMap::new();
        let mut complete = true;
        let base_path = &config.remote_folder;
        // Load .aeroignore from local sync root (applies to remote paths too)
        let aeroignore = crate::sync_ignore::AeroIgnore::load(&config.local_folder);
        let excludes = crate::sync::compile_excludes(&config.exclude_patterns)?;

        // Stack-based recursive scan with depth tracking
        // (base_path, relative_prefix, depth)
        let mut stack: Vec<(String, String, u32)> = vec![(base_path.clone(), String::new(), 0)];
        // Track visited absolute paths to prevent infinite loops caused by
        // servers that list the current directory itself as a child entry.
        let mut visited = std::collections::HashSet::new();
        visited.insert(base_path.clone());
        const MAX_DEPTH: u32 = 64;

        while let Some((current_path, relative_prefix, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                tracing::warn!("Remote scan depth limit reached at {}", current_path);
                complete = false;
                continue;
            }

            // Navigate to directory
            if provider.cd(&current_path).await.is_err() {
                complete = false;
                continue;
            }

            // List files using provider
            let entries = match provider.list(".").await {
                Ok(list) => list,
                Err(_) => {
                    complete = false;
                    continue;
                }
            };

            for entry in entries {
                let relative_path = if relative_prefix.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", relative_prefix, entry.name)
                };

                // Check exclusions: .aeroignore first, then config patterns
                let decision = crate::sync_ignore::scan_decision(
                    aeroignore.as_ref(),
                    &relative_path,
                    entry.is_dir,
                    &excludes,
                );
                if decision == crate::sync_ignore::ScanDecision::Skip {
                    continue;
                }

                // A walk-only directory is not listed; its children decide alone.
                if decision == crate::sync_ignore::ScanDecision::Keep {
                    // P1-6: Cap file index at 100K to prevent unbounded memory growth.
                    // A truncated scan is an INCOMPLETE scan: report it so the delete
                    // gate refuses to act on it. (CLAUDE-AV-B3-11)
                    if files.len() >= 100_000 {
                        tracing::warn!("Remote file index cap reached (100K), truncating scan");
                        return Ok((files, false));
                    }

                    files.insert(
                        relative_path.clone(),
                        FileInfo {
                            name: entry.name.clone(),
                            path: format!("{}/{}", current_path, entry.name),
                            size: entry.size,
                            modified: entry
                                .modified
                                .as_deref()
                                .and_then(crate::parse_remote_datetime),
                            is_dir: entry.is_dir,
                            // Use provider-supplied content hash if available (e.g. FileLu).
                            // Enables hash-based comparison for providers that don't preserve mtime.
                            checksum_alg: None,
                            checksum: entry.metadata.get("content_hash").cloned(),
                        },
                    );
                }

                if entry.is_dir {
                    // Selective sync: skip excluded folders (don't descend)
                    let is_excluded = config.excluded_folders.iter().any(|ef| {
                        let ef_norm = ef.trim_matches('/');
                        relative_path == ef_norm
                            || relative_path.starts_with(&format!("{}/", ef_norm))
                    });
                    if is_excluded {
                        continue;
                    }

                    let child_path = format!("{}/{}", current_path, entry.name);
                    if visited.insert(child_path.clone()) {
                        stack.push((child_path, relative_path, depth + 1));
                    } else {
                        tracing::warn!("Skipping already-visited directory: {}", child_path);
                    }
                }
            }
        }

        Ok((files, complete))
    }

    /// Process a single file comparison using any StorageProvider
    async fn process_comparison_with_provider<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        config: &CloudConfig,
        comparison: &FileComparison,
    ) -> Result<SyncAction, String> {
        // Validate relative_path against traversal attacks (CF-004)
        validate_relative_path(&comparison.relative_path)?;

        // Resolve via the shared decision so the executed action and the
        // recorded post-sync baseline are always derived the same way.
        let action = self.resolve_action(config, comparison);
        // Whether the transfer left the local file as the baseline may
        // record it (false for a file edited while it was uploaded).
        let mut landed = true;

        // Execute action using provider methods
        match &action {
            SyncAction::Upload => {
                let remote_path = format!(
                    "{}/{}",
                    config.remote_folder.trim_end_matches('/'),
                    comparison.relative_path
                );

                if comparison.is_dir {
                    // Create remote directory
                    if let Err(e) = provider.mkdir(&remote_path).await {
                        // Directory might already exist, log but don't fail
                        tracing::debug!("mkdir {} (may exist): {}", remote_path, e);
                    }
                } else if let Some(local_info) = &comparison.local_info {
                    // Ensure parent directory exists on remote (check first to avoid duplicates)
                    if let Some(parent) = std::path::Path::new(&comparison.relative_path).parent() {
                        if !parent.as_os_str().is_empty() {
                            let parent_path = format!(
                                "{}/{}",
                                config.remote_folder.trim_end_matches('/'),
                                parent.to_string_lossy()
                            );
                            if provider.cd(&parent_path).await.is_err() {
                                // Recursive for the same reason as the remote
                                // root above: a file at `a/b/c.txt` needs both
                                // `a` and `a/b`, and one mkdir only ever made
                                // the last one.
                                crate::sync::ensure_remote_dir(provider, &parent_path).await;
                            }
                        }
                    }

                    // Sent again while it changes as it goes up, a few times:
                    // what the remote holds must be a version of the file the
                    // baseline can name. After the stat, the local file is
                    // read again and compared with what was read right before
                    // the upload (size, time, inode, change time), with no
                    // round trip between that reading and the stamp.
                    let local_path = std::path::Path::new(&local_info.path);
                    landed = false;
                    let mut sends = 0;
                    // One bound for all the sends of this file in this cycle.
                    let quiet_deadline = tokio::time::Instant::now() + self.send_quiet_bound;
                    loop {
                        sends += 1;
                        Self::wait_until_quiet(local_path, self.send_quiet_window, quiet_deadline)
                            .await;
                        let sent = Self::local_file_state(local_path);
                        tracing::info!(
                            "AeroCloud: uploading local '{}' ({} bytes) to remote '{}'",
                            local_info.path,
                            sent.map_or(local_info.size, |state| state.len),
                            remote_path
                        );
                        provider
                            .upload(&local_info.path, &remote_path, None)
                            .await
                            .map_err(|e| format!("Upload failed: {}", e))?;

                        // After upload, stat the remote file to get the server-assigned mtime,
                        // then apply it to the local file so both sides match.
                        // This prevents ping-pong re-sync on all providers (SFTP, FTP, WebDAV, S3, cloud APIs).
                        let uploaded = provider.stat(&remote_path).await;
                        if let Err(e) = &uploaded {
                            tracing::debug!(
                                "Could not stat remote file after upload (non-fatal): {}",
                                e
                            );
                        }
                        let server_time = uploaded
                            .as_ref()
                            .ok()
                            .and_then(|entry| entry.modified.as_deref())
                            .and_then(crate::parse_remote_datetime)
                            .and_then(|dt| DateTime::<Utc>::from_timestamp(dt.timestamp(), 0));
                        let Some(sent) = sent.filter(|sent| {
                            Self::local_file_state(local_path).as_ref() == Some(sent)
                        }) else {
                            if sends < UPLOAD_SENDS_WHILE_EDITED {
                                tracing::info!(
                                    "AeroCloud: '{}' changed while it uploaded; sending it again",
                                    comparison.relative_path
                                );
                                continue;
                            }
                            // The remote holds one of the sends. The baseline
                            // names no time either side has, so the next cycle
                            // reads both as changed, a conflict, whatever time
                            // the server gives the remote (the scan's time
                            // could read identical within the 2 s window), and
                            // the cycle reports the file.
                            tracing::warn!(
                                "AeroCloud: '{}' kept changing while it uploaded; left as a conflict",
                                comparison.relative_path
                            );
                            self.note_landed_as(
                                &comparison.relative_path,
                                Landed {
                                    size: None,
                                    modified: UNSETTLED_BASELINE,
                                },
                            );
                            if let Ok(mut unsettled) = self.unsettled.lock() {
                                unsettled.push(comparison.relative_path.clone());
                            }
                            break;
                        };
                        if provider.reports_exact_size()
                            && uploaded.as_ref().is_ok_and(|entry| {
                                !crate::providers::ftp_listing::size_is_unreadable(entry)
                                    && entry.size != sent.len
                            })
                        {
                            // The remote is not what was sent: another client
                            // wrote it between the end of the upload and the
                            // stat (an overlay that reports the size on the
                            // wire, compressed or encrypted, never matches, and
                            // is not asked, nor is an FTP size that could not
                            // be read). The local file is not stamped with a time
                            // that belongs to that write, and the baseline
                            // records what was sent: the next cycle reads the
                            // remote as changed.
                            tracing::info!(
                                "AeroCloud: '{}' changed on the remote right after the upload",
                                comparison.relative_path
                            );
                            if let Some(modified) = sent.modified {
                                self.note_landed_as(
                                    &comparison.relative_path,
                                    Landed {
                                        size: Some(sent.len),
                                        modified,
                                    },
                                );
                            }
                            break;
                        }
                        let stamped = server_time.filter(|dt| {
                            filetime::set_file_mtime(
                                local_path,
                                filetime::FileTime::from_unix_time(dt.timestamp(), 0),
                            )
                            .is_ok()
                        });
                        // A remote that gives no time, or a stamp that could
                        // not be applied, leaves the time the file had when it
                        // was sent: the remote's own time then differs from it,
                        // and the next cycle downloads the same content once
                        // more (a local edit made before that cycle reads as a
                        // conflict).
                        if let Some(modified) = stamped.or(sent.modified) {
                            self.note_landed_as(
                                &comparison.relative_path,
                                Landed {
                                    size: Some(sent.len),
                                    modified,
                                },
                            );
                        }
                        break;
                    }
                }
            }
            SyncAction::Download => {
                let local_path = config.local_folder.join(&comparison.relative_path);

                if comparison.is_dir {
                    // Create local directory
                    if let Err(e) = std::fs::create_dir_all(&local_path) {
                        tracing::warn!(
                            "Failed to create directory {}: {}",
                            local_path.display(),
                            e
                        );
                    }
                } else if let Some(remote_info) = &comparison.remote_info {
                    // Archive existing file before overwrite (versioning)
                    if local_path.exists() {
                        let versioning = crate::sync_versioning::SyncVersioning::new(
                            &config.local_folder,
                            config.versioning_strategy.clone(),
                        );
                        if versioning.is_enabled() {
                            if let Err(e) = versioning.archive(&local_path) {
                                tracing::warn!("Versioning archive failed: {}", e);
                            }
                        }
                    }

                    // Ensure parent directory exists
                    if let Some(parent) = local_path.parent() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            tracing::warn!(
                                "Failed to create directory {}: {}",
                                parent.display(),
                                e
                            );
                        }
                    }

                    provider
                        .download(&remote_info.path, &local_path.to_string_lossy(), None)
                        .await
                        .map_err(|e| format!("Download failed: {}", e))?;
                    // After download, preserve remote mtime on local file
                    // so next sync sees them as identical.
                    if let Some(ref mtime) = remote_info.modified {
                        crate::preserve_remote_mtime_dt(&local_path, Some(*mtime));
                    }
                }
            }
            SyncAction::KeepBoth if !comparison.is_dir => {
                let local_path = config.local_folder.join(&comparison.relative_path);
                // Rename local file with Dropbox-style conflict suffix to preserve both versions
                if local_path.exists() {
                    let conflict_path = local_path.with_file_name(conflict_rename(&local_path));
                    std::fs::rename(&local_path, &conflict_path).map_err(|e| {
                        format!("Failed to preserve local copy before download: {}", e)
                    })?;
                }
                // Download remote version to original path
                if let Some(remote_info) = &comparison.remote_info {
                    if let Some(parent) = local_path.parent() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            tracing::warn!(
                                "Failed to create directory {}: {}",
                                parent.display(),
                                e
                            );
                        }
                    }
                    provider
                        .download(&remote_info.path, &local_path.to_string_lossy(), None)
                        .await
                        .map_err(|e| format!("KeepBoth download failed: {}", e))?;
                }
            }
            SyncAction::DeleteRemote if !comparison.is_dir => {
                let remote_path = format!(
                    "{}/{}",
                    config.remote_folder.trim_end_matches('/'),
                    comparison.relative_path
                );
                provider
                    .delete(&remote_path)
                    .await
                    .map_err(|e| format!("Delete propagation (remote) failed: {}", e))?;
                tracing::info!("AeroCloud: delete propagated to remote '{}'", remote_path);
            }
            SyncAction::DeleteLocal if !comparison.is_dir => {
                let local_path = config.local_folder.join(&comparison.relative_path);
                if local_path.exists() {
                    archive_before_propagated_delete(config, &local_path);
                    std::fs::remove_file(&local_path)
                        .map_err(|e| format!("Delete propagation (local) failed: {}", e))?;
                    tracing::info!(
                        "AeroCloud: delete propagated to local '{}'",
                        local_path.display()
                    );
                }
            }
            _ => {}
        }

        if landed
            && matches!(action, SyncAction::Upload | SyncAction::Download)
            && !comparison.is_dir
        {
            self.note_landed(config, &comparison.relative_path);
        }
        Ok(action)
    }
}

/// Unified helper for one AeroCloud config sync (connect + optional overlay stack +
/// ensure remote + full or preview sync + disconnect).
///
/// This is the single implementation of the previously duplicated sequence:
/// - `lib.rs:perform_background_sync_inner` (background/manual)
/// - `bin/aeroftp_cli.rs:cmd_aerocloud_sync` (one-shot CLI)
///
/// Later multi-pair (and overlay stack work) will call this once per pair.
/// cd/mkdir/ensure is handled inside the plan builders; callers no longer
/// duplicate it.
///
/// The overlay stack is composed here from config state: crypt is resolved from
/// the saved profile first, then AeroCompress wraps it as the outer layer when
/// `config.compress_enabled` is true.
///
/// `store` is passed explicitly (from_cache() or opened vault) so CLI and GUI
/// paths both work. `config` must have protocol_type resolved (CLI does the
/// vault lookup before calling because `set` can run vault-less).
pub async fn sync_one_config(
    config: CloudConfig,
    store: Option<&credential_store::CredentialStore>,
    app: Option<AppHandle>,
    dry_run: bool,
) -> Result<SyncOperationResult, String> {
    if !config.enabled {
        return Err("AeroCloud is not enabled".to_string());
    }
    if config.server_profile.is_empty() {
        return Err(
            "AeroCloud has no server profile. Set one with the GUI or `aeroftp aerocloud set --profile <name>`."
                .to_string(),
        );
    }

    // Fail-closed on a locked vault (audit A-H1). The per-profile crypt-overlay
    // binding lives in the credential store; without it we cannot prove the
    // profile is not bound to a zero-knowledge overlay. Proceeding would skip
    // the crypt wrap while the compress layer (gated only on config) still runs,
    // uploading AECP-wrapped plaintext to a remote the user configured for
    // encryption and mis-reading prior ciphertext back as legacy. Direct-auth
    // providers already fail without the vault; OAuth providers can connect from
    // the token cache, so this guard is the one place that closes the gap. Refuse
    // before connecting so a locked vault never opens a session at all.
    let store = store.ok_or_else(|| {
        "vault is locked: refusing AeroCloud sync (cannot verify the profile's \
         encryption binding). Unlock the vault and retry."
            .to_string()
    })?;

    let mut provider = cloud_provider_factory::create_cloud_provider(&config)
        .await
        .map_err(|e| format!("failed to connect provider: {e}"))?;

    provider = crypt_overlay_provider::build_aerocloud_overlay_stack(
        provider,
        &config.server_profile,
        store,
    )
    .await
    .map_err(|e| format!("overlay stack refused the sync: {e}"))?;
    if config.compress_enabled {
        validate_compress_level(config.compress_level)?;
        provider = Box::new(CompressOverlayProvider::new(
            provider,
            config.compress_level,
        ));
    }

    // NOTE: explicit cd/mkdir that used to sit between wrap and service here
    // (and in the old inner) has been removed: build_sync_plan_with_provider
    // (used by both preview and perform) centralizes ensure_remote.

    let mut svc = CloudService::new();
    if let Some(handle) = app {
        svc.set_app_handle(handle);
    }
    svc.init(config.clone()).await;

    let sync_result = if dry_run {
        svc.preview_full_sync_with_provider(&mut *provider).await
    } else {
        svc.perform_full_sync_with_provider(&mut *provider).await
    }?;

    let _ = provider.disconnect().await;

    Ok(sync_result)
}

/// State that a folder pair is compared by size only: a warning the first
/// time in this process, debug afterwards, so a folder synced every minute does
/// not repeat the same warning in the log every cycle.
fn log_size_only_window(
    label: &str,
    config: &CloudConfig,
    window: crate::sync_core::mtime::ModifyWindow,
) {
    let pair = format!(
        "{}\u{0}{}",
        config.local_folder.display(),
        config.remote_folder
    );
    if first_size_only_notice(pair) {
        tracing::warn!("{label}: {}", window.describe());
    } else {
        tracing::debug!("{label}: {}", window.describe());
    }
}

/// Whether this is the first size-only notice for `pair` in this process.
fn first_size_only_notice(pair: String) -> bool {
    static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    WARNED
        .get_or_init(Default::default)
        .lock()
        .map(|mut warned| warned.insert(pair))
        .unwrap_or(true)
}

impl Default for CloudService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod baseline_tests {
    use super::*;

    fn fi(size: u64, mtime_secs: i64) -> FileInfo {
        FileInfo {
            name: "f".to_string(),
            path: "/f".to_string(),
            size,
            modified: Some(DateTime::<Utc>::from_timestamp(mtime_secs, 0).unwrap()),
            is_dir: false,
            checksum: None,
            checksum_alg: None,
        }
    }

    fn dir_info() -> FileInfo {
        let mut d = fi(0, 0);
        d.is_dir = true;
        d
    }

    fn cmp(
        status: SyncStatus,
        local: Option<FileInfo>,
        remote: Option<FileInfo>,
        previously_synced: bool,
        is_dir: bool,
    ) -> FileComparison {
        FileComparison {
            relative_path: "f.txt".to_string(),
            status,
            local_info: local,
            remote_info: remote,
            is_dir,
            sync_reason: String::new(),
            previously_synced,
        }
    }

    fn cfg(direction: CompareDirection, preserve: bool, strategy: ConflictStrategy) -> CloudConfig {
        CloudConfig {
            sync_direction: direction,
            preserve_remote_deletes: preserve,
            conflict_strategy: strategy,
            ..Default::default()
        }
    }

    // ---- baseline_entry_for: record the source-of-truth side ----

    // Download must record the REMOTE side (the content that landed locally),
    // not the pre-download local side, else next cycle both sides differ from
    // the baseline and the file surfaces as a spurious Conflict.
    #[test]
    fn download_records_remote_side_not_stale_local() {
        let local = fi(10, 1000);
        let remote = fi(20, 2000);
        let entry = CloudService::baseline_entry_for(
            &SyncAction::Download,
            CompareDirection::Bidirectional,
            Some(&local),
            Some(&remote),
            false,
        )
        .expect("download should record a baseline");
        assert_eq!(entry.size, 20);
        assert_eq!(
            entry.modified,
            Some(DateTime::<Utc>::from_timestamp(2000, 0).unwrap())
        );
    }

    #[test]
    fn upload_records_local_side() {
        let local = fi(30, 3000);
        let remote = fi(10, 1000);
        let entry = CloudService::baseline_entry_for(
            &SyncAction::Upload,
            CompareDirection::Bidirectional,
            Some(&local),
            Some(&remote),
            false,
        )
        .expect("upload should record a baseline");
        assert_eq!(entry.size, 30);
    }

    // Receive-only Skip (a tolerated local edit) must follow the REMOTE
    // authoritative side, so the local edit stays tolerated next cycle instead
    // of being reverted by baseline bookkeeping.
    #[test]
    fn skip_receive_only_records_remote_authoritative_side() {
        let local = fi(30, 3000);
        let remote = fi(10, 1000);
        let entry = CloudService::baseline_entry_for(
            &SyncAction::Skip,
            CompareDirection::RemoteToLocal,
            Some(&local),
            Some(&remote),
            false,
        )
        .unwrap();
        assert_eq!(
            entry.size, 10,
            "receive-only baseline follows the remote side"
        );
    }

    #[test]
    fn skip_send_only_records_local_authoritative_side() {
        let local = fi(30, 3000);
        let remote = fi(99, 9000);
        let entry = CloudService::baseline_entry_for(
            &SyncAction::Skip,
            CompareDirection::LocalToRemote,
            Some(&local),
            Some(&remote),
            false,
        )
        .unwrap();
        assert_eq!(entry.size, 30, "send-only baseline follows the local side");
    }

    #[test]
    fn askuser_keepboth_deletes_do_not_advance_baseline() {
        let local = fi(1, 1);
        let remote = fi(2, 2);
        for action in [
            SyncAction::AskUser,
            SyncAction::KeepBoth,
            SyncAction::DeleteLocal,
            SyncAction::DeleteRemote,
        ] {
            assert!(
                CloudService::baseline_entry_for(
                    &action,
                    CompareDirection::Bidirectional,
                    Some(&local),
                    Some(&remote),
                    false
                )
                .is_none(),
                "action {:?} must not advance baseline",
                action
            );
        }
    }

    /// A one-way mirror must not baseline a file it chose to leave alone:
    /// otherwise the file spared on the first cycle reads as previously synced
    /// on the second and is deleted. Both directions, since the send-only case
    /// deletes on the server, where nothing archives it.
    #[test]
    fn a_skipped_one_sided_file_gets_no_baseline() {
        let file = fi(7, 700);
        for (direction, local, remote) in [
            (CompareDirection::RemoteToLocal, Some(&file), None),
            (CompareDirection::LocalToRemote, None, Some(&file)),
        ] {
            assert!(
                CloudService::baseline_entry_for(
                    &SyncAction::Skip,
                    direction,
                    local,
                    remote,
                    false
                )
                .is_none(),
                "{direction:?}: a never-synced one-sided file must not enter the baseline"
            );
        }
    }

    /// The consequence, one level up: a one-sided Skip adds nothing, but it must
    /// not remove what is there either. A file synced earlier and now left on
    /// one side under preserve keeps its entry, so turning preserve off later
    /// still propagates its delete; only a delete removes an entry.
    #[test]
    fn a_one_sided_skip_neither_adds_nor_removes_a_baseline() {
        let svc = CloudService::new();
        let entry = |size| SyncIndexEntry {
            size,
            modified: None,
            is_dir: false,
        };
        let mut prior = SyncIndex::new("/l".to_string(), "/r".to_string());
        prior.files.insert("synced.txt".to_string(), entry(1));
        prior.files.insert("gone.txt".to_string(), entry(2));
        let mut synced = cmp(SyncStatus::RemoteOnly, None, Some(fi(1, 1)), true, false);
        synced.relative_path = "synced.txt".to_string();
        let mut never = cmp(SyncStatus::RemoteOnly, None, Some(fi(3, 3)), false, false);
        never.relative_path = "never.txt".to_string();
        let mut gone = cmp(SyncStatus::LocalOnly, Some(fi(2, 2)), None, true, false);
        gone.relative_path = "gone.txt".to_string();

        // Send-only with preserve: both remote-only files are Skipped.
        let preserve = cfg(
            CompareDirection::LocalToRemote,
            true,
            ConflictStrategy::AskUser,
        );
        let files =
            svc.post_sync_baseline(&[synced.clone(), never.clone()], &preserve, Some(&prior));
        assert!(
            files.contains_key("synced.txt"),
            "a Skip must not drop a synced file's entry"
        );
        assert!(
            !files.contains_key("never.txt"),
            "a Skip must not baseline a never-synced file"
        );

        // Bidirectional: the local-only previously synced file is a delete.
        let bidi = cfg(
            CompareDirection::Bidirectional,
            false,
            ConflictStrategy::AskUser,
        );
        let files = svc.post_sync_baseline(&[gone], &bidi, Some(&prior));
        assert!(
            !files.contains_key("gone.txt"),
            "a delete removes the entry"
        );
        assert!(
            files.contains_key("synced.txt"),
            "untouched entries carry forward"
        );
    }

    /// Major 1 (re-review of #949): a download recorded the REMOTE side's time
    /// as the baseline, and a backend listing no comparable time (FTP with
    /// `LIST` dates) gives none. The local side, compared against the baseline
    /// with the local clock, then had no time to compare and went by size
    /// alone: a same-size local edit of every file that came from the server
    /// went unseen, and the next remote change overwrote it. With no remote
    /// time the baseline takes the downloaded file's own.
    #[test]
    fn a_download_without_a_remote_time_records_the_local_file_time() {
        let root = tempfile::tempdir().expect("local root");
        let file = root.path().join("f.txt");
        std::fs::write(&file, b"payload").unwrap();
        let landed = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(landed)
            .unwrap();
        let mut remote = fi(7, 0);
        remote.modified = None;
        let downloaded = cmp(
            SyncStatus::RemoteNewer,
            Some(fi(3, 1)),
            Some(remote),
            true,
            false,
        );
        let mut config = cfg(
            CompareDirection::Bidirectional,
            false,
            ConflictStrategy::AskUser,
        );
        config.local_folder = root.path().to_path_buf();
        let svc = CloudService::new();
        // The download ends; a same-size edit lands later in the cycle.
        svc.note_landed(&config, "f.txt");
        stamp(&file, 1_750_000_000);
        let files = svc.post_sync_baseline(&[downloaded], &config, None);
        let entry = files.get("f.txt").expect("the download is recorded");
        assert_eq!(entry.size, 7);
        assert_eq!(
            entry.modified,
            DateTime::<Utc>::from_timestamp(1_700_000_000, 0),
            "the time the download left, of the local clock, not the later edit's"
        );
    }

    /// m4 (third review of #949): after an upload the provider path stamps
    /// the local file with the server's time, and the baseline kept the time
    /// the file had before: the next cycle saw both sides differ from it and
    /// raised a conflict every cycle (AskUser asking again, KeepBoth making
    /// copies, PreferLocal uploading again). The baseline takes the local
    /// file's time as the upload left it, not as it is when the cycle ends
    /// (CodeRabbit on #949: a same-size edit made in between would be
    /// recorded as synced).
    #[test]
    fn an_upload_records_the_local_time_after_the_stamp() {
        let root = tempfile::tempdir().expect("local root");
        let file = root.path().join("f.txt");
        std::fs::write(&file, b"payload").unwrap();
        let stamped = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(stamped)
            .unwrap();
        let uploaded = cmp(
            SyncStatus::LocalNewer,
            Some(fi(7, 1_700_000_000)),
            Some(fi(3, 1)),
            true,
            false,
        );
        let mut config = cfg(
            CompareDirection::Bidirectional,
            false,
            ConflictStrategy::AskUser,
        );
        config.local_folder = root.path().to_path_buf();
        let svc = CloudService::new();
        svc.note_landed(&config, "f.txt");
        stamp(&file, 1_850_000_000);
        let files = svc.post_sync_baseline(&[uploaded], &config, None);
        let entry = files.get("f.txt").expect("the upload is recorded");
        assert_eq!(entry.size, 7);
        assert_eq!(
            entry.modified,
            DateTime::<Utc>::from_timestamp(1_800_000_000, 0),
            "the time after the stamp, not the scan's nor the later edit's"
        );
    }

    fn stamp(file: &std::path::Path, secs: u64) {
        std::fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    }

    /// What the stamping provider's `stat` gives as the file's time.
    #[derive(Default, Clone, Copy, Debug)]
    enum StatTime {
        /// One server time for every file: a server that stamps its own.
        #[default]
        Fixed,
        /// The local file's time as it is when the remote is read: SFTP, and
        /// FTP with MFMT, stamp the remote from the local file.
        LocalFile,
        /// That time plus one second: a server's own time in step with the
        /// local clock, inside the 2 s window.
        LocalFilePlusOne,
    }

    /// Accepts every upload, records what it sent, and reports the file with
    /// the time `stat_time` says: the provider path stamps that time on the
    /// local file after an upload.
    #[derive(Default)]
    struct StampingProvider {
        /// Saves the local file again (same size, a new time) during this
        /// many uploads, the first ones.
        edits_during_upload: usize,
        /// Those saves put the file's old time back: only its change time
        /// moves.
        edits_keep_time: bool,
        /// Saves this local file again, same size, while the remote is read
        /// after the first upload.
        edit_during_stat: Option<std::path::PathBuf>,
        stat_time: StatTime,
        /// The size `stat` reports instead of the size sent: another client
        /// wrote the file right after the upload, or an overlay that reports
        /// the size on the wire.
        stat_size: Option<u64>,
        /// Reports sizes that are not the plain size (an overlay).
        inexact_size: bool,
        /// Refuses the upload of the local file with this name.
        refused: Option<&'static str>,
        /// Reports the size as FTP does when its listing gives none: 0, and
        /// the marker that says so.
        size_unreadable: bool,
        /// What each upload sent, in order.
        sent: Vec<Vec<u8>>,
        /// The local file of the last upload.
        local: Option<std::path::PathBuf>,
    }

    #[async_trait::async_trait]
    impl StorageProvider for StampingProvider {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> crate::providers::ProviderType {
            crate::providers::ProviderType::Sftp
        }
        fn display_name(&self) -> String {
            "stamping".to_string()
        }
        fn reports_exact_size(&self) -> bool {
            !self.inexact_size
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(&mut self, _path: &str) -> Result<Vec<ProviderRemoteEntry>, ProviderError> {
            Ok(Vec::new())
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok("/".to_string())
        }
        async fn cd(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            _remote_path: &str,
            _local_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("download".to_string()))
        }
        async fn download_to_bytes(
            &mut self,
            _remote_path: &str,
        ) -> Result<Vec<u8>, ProviderError> {
            Err(ProviderError::NotSupported("download_to_bytes".to_string()))
        }
        async fn upload(
            &mut self,
            local_path: &str,
            _remote_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            if self.refused.is_some_and(|name| local_path.ends_with(name)) {
                return Err(ProviderError::PermissionDenied(local_path.to_string()));
            }
            self.sent.push(std::fs::read(local_path).unwrap());
            self.local = Some(std::path::PathBuf::from(local_path));
            let n = self.sent.len();
            if n <= self.edits_during_upload {
                let local = std::path::Path::new(local_path);
                if self.edits_keep_time {
                    // Past the clock tick of the file's own last change.
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    let time = std::fs::metadata(local).unwrap().modified().unwrap();
                    std::fs::write(local, format!("EDIT{n:02}!")).unwrap();
                    std::fs::File::options()
                        .write(true)
                        .open(local)
                        .unwrap()
                        .set_modified(time)
                        .unwrap();
                } else {
                    std::fs::write(local, format!("EDIT{n:02}!")).unwrap();
                    stamp(local, 1_750_000_000 + n as u64);
                }
            }
            Ok(())
        }
        async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("delete".to_string()))
        }
        async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rmdir".to_string()))
        }
        async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rmdir_recursive".to_string()))
        }
        async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rename".to_string()))
        }
        async fn stat(&mut self, path: &str) -> Result<ProviderRemoteEntry, ProviderError> {
            if let Some(local) = self.edit_during_stat.take() {
                std::fs::write(&local, b"EDITED!").unwrap();
                stamp(&local, 1_750_000_000);
            }
            let size = self
                .stat_size
                .or_else(|| self.sent.last().map(|bytes| bytes.len() as u64))
                .unwrap_or(7);
            let local_time = || {
                let local = self.local.as_ref().expect("stat after an upload");
                DateTime::<Utc>::from(std::fs::metadata(local).unwrap().modified().unwrap())
            };
            let time = match self.stat_time {
                StatTime::Fixed => server_time(),
                StatTime::LocalFile => local_time(),
                StatTime::LocalFilePlusOne => local_time() + chrono::Duration::seconds(1),
            };
            let mut entry = ProviderRemoteEntry::file("f.txt".to_string(), path.to_string(), size);
            entry.modified = Some(time.format("%Y-%m-%d %H:%M:%S").to_string());
            if self.size_unreadable {
                entry.size = 0;
                entry.metadata.insert(
                    crate::providers::ftp_listing::SIZE_UNREADABLE.to_string(),
                    "1".to_string(),
                );
            }
            Ok(entry)
        }
        async fn size(&mut self, _path: &str) -> Result<u64, ProviderError> {
            Ok(7)
        }
        async fn exists(&mut self, _path: &str) -> Result<bool, ProviderError> {
            Ok(true)
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("stamping".to_string())
        }
    }

    /// The time `StampingProvider` reports for every file.
    fn server_time() -> DateTime<Utc> {
        chrono::NaiveDateTime::parse_from_str("2027-01-15 08:00:00", "%Y-%m-%d %H:%M:%S")
            .unwrap()
            .and_utc()
    }

    /// Upload `f.txt` (as the scan saw it: its size on disk, 1_700_000_000)
    /// through `provider`, then classify the next cycle: the local file as it
    /// is on disk, the remote as the provider lists it, against the baseline
    /// this cycle wrote.
    async fn upload_then_next_cycle(
        provider: &mut StampingProvider,
        root: &std::path::Path,
    ) -> (HashMap<String, SyncIndexEntry>, SyncStatus) {
        let file = root.join("f.txt");
        let mut local = fi(std::fs::metadata(&file).unwrap().len(), 1_700_000_000);
        local.path = file.to_string_lossy().into_owned();
        let uploaded = cmp(
            SyncStatus::LocalNewer,
            Some(local),
            Some(fi(3, 1)),
            true,
            false,
        );
        let mut config = cfg(
            CompareDirection::Bidirectional,
            false,
            ConflictStrategy::AskUser,
        );
        config.local_folder = root.to_path_buf();
        config.remote_folder = "/remote".to_string();
        let svc = CloudService::new();
        svc.process_comparison_with_provider(provider, &config, &uploaded)
            .await
            .expect("the upload succeeds");
        let baseline = svc.post_sync_baseline(&[uploaded], &config, None);
        let meta = std::fs::metadata(&file).unwrap();
        let mut now_local = fi(meta.len(), 0);
        now_local.modified = Some(DateTime::<Utc>::from(meta.modified().unwrap()));
        // The remote as the next cycle lists it.
        let listed = provider.stat("/remote/f.txt").await.unwrap();
        let mut remote = fi(listed.size, 0);
        remote.modified = listed
            .modified
            .as_deref()
            .and_then(crate::parse_remote_datetime);
        let index = SyncIndex {
            version: SYNC_INDEX_VERSION,
            last_sync: Utc::now(),
            local_path: root.to_string_lossy().into_owned(),
            remote_path: "/remote".to_string(),
            files: baseline.clone(),
            unverified_keys: Default::default(),
        };
        let next = build_comparison_results_with_index(
            HashMap::from([("f.txt".to_string(), now_local)]),
            HashMap::from([("f.txt".to_string(), remote)]),
            &CompareOptions {
                compare_size: provider.reports_exact_size(),
                ..CompareOptions::default()
            },
            Some(&index),
        );
        let status = next
            .iter()
            .find(|c| c.relative_path == "f.txt")
            .map(|c| c.status.clone())
            .unwrap_or(SyncStatus::Identical);
        (baseline, status)
    }

    /// Major 1 (fourth review of #949), m2 of the fifth and Major 1 of the
    /// sixth: a same-size edit saved while the file uploaded went unseen. The
    /// remote held the copy before the edit, stamped with a time the baseline
    /// then recorded: the server's own (the fourth review), or on SFTP and on
    /// FTP with MFMT the local file's, read after the upload, which is the
    /// edit's (the sixth), so the next cycle read the pair as identical. A
    /// file that changed while it went up is sent again in the same cycle,
    /// and the remote holds the edit, whatever time the server gives it.
    ///
    /// The edit may change the size too (the scan read 5 bytes).
    #[tokio::test]
    async fn an_edit_during_an_upload_is_sent_again() {
        for stat_time in [
            StatTime::Fixed,
            StatTime::LocalFile,
            StatTime::LocalFilePlusOne,
        ] {
            for scanned in [&b"payload"[..], &b"12345"[..]] {
                let root = tempfile::tempdir().expect("local root");
                let file = root.path().join("f.txt");
                std::fs::write(&file, scanned).unwrap();
                stamp(&file, 1_700_000_000);
                let mut provider = StampingProvider {
                    edits_during_upload: 1,
                    stat_time,
                    ..Default::default()
                };
                let (_, next) = upload_then_next_cycle(&mut provider, root.path()).await;
                let case = format!("{stat_time:?}, {} bytes scanned", scanned.len());
                assert_eq!(provider.sent.len(), 2, "{case}");
                assert_eq!(
                    provider.sent[1], b"EDIT01!",
                    "the remote holds the edit ({case})"
                );
                assert_eq!(next, SyncStatus::Identical, "{case}");
            }
        }
    }

    /// Nit of the fifth verification: a same-size save that leaves the file's
    /// time where it was (within its tick, or put back) passes a check of
    /// size and time. The upload reads the file's inode and change time too.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_edit_that_keeps_size_and_time_is_sent_again() {
        let root = tempfile::tempdir().expect("local root");
        let file = root.path().join("f.txt");
        std::fs::write(&file, b"payload").unwrap();
        stamp(&file, 1_700_000_000);
        let mut provider = StampingProvider {
            edits_during_upload: 1,
            edits_keep_time: true,
            ..Default::default()
        };
        upload_then_next_cycle(&mut provider, root.path()).await;
        assert_eq!(provider.sent.len(), 2, "the save was not seen");
        assert_eq!(provider.sent[1], b"EDIT01!");
    }

    /// A file that keeps changing while it goes up is sent a bounded number
    /// of times, then left: the next cycle reads a conflict, not a remote copy
    /// recorded as the synced state (which, on SFTP, reads identical).
    #[tokio::test]
    async fn a_file_that_keeps_changing_while_it_uploads_is_left_as_a_conflict() {
        for stat_time in [
            StatTime::Fixed,
            StatTime::LocalFile,
            StatTime::LocalFilePlusOne,
        ] {
            let root = tempfile::tempdir().expect("local root");
            let file = root.path().join("f.txt");
            std::fs::write(&file, b"payload").unwrap();
            stamp(&file, 1_700_000_000);
            let mut provider = StampingProvider {
                edits_during_upload: usize::MAX,
                stat_time,
                ..Default::default()
            };
            let (baseline, next) = upload_then_next_cycle(&mut provider, root.path()).await;
            assert_eq!(
                provider.sent.len(),
                3,
                "three sends, then it is left ({stat_time:?})"
            );
            assert_eq!(
                baseline.get("f.txt").and_then(|entry| entry.modified),
                Some(DateTime::<Utc>::UNIX_EPOCH),
                "a time neither side has ({stat_time:?})"
            );
            assert_eq!(next, SyncStatus::Conflict, "{stat_time:?}");
        }
    }

    /// Minor 2 (verification of the sixth round): with the scan's time as the
    /// baseline, a burst of same-size edits right after the scan, and a
    /// server time within 2 s of it, the next cycle read identical over the
    /// last send. The baseline now names a time neither side has, and the
    /// cycle reports the file, after saving the baseline (a cycle with
    /// errors saves none, and the conflict would be lost with it).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_file_that_keeps_changing_is_reported_after_its_baseline_is_saved() {
        let _env = crate::test_env::lock();
        let data = tempfile::tempdir().unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", data.path());
        let outcome = std::panic::catch_unwind(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(async {
                    let local = tempfile::tempdir().unwrap();
                    std::fs::write(local.path().join("f.txt"), b"payload").unwrap();
                    let config = CloudConfig {
                        enabled: true,
                        local_folder: local.path().to_path_buf(),
                        remote_folder: "/keeps-changing".into(),
                        sync_direction: CompareDirection::Bidirectional,
                        ..Default::default()
                    };
                    let mut provider = StampingProvider {
                        edits_during_upload: usize::MAX,
                        ..Default::default()
                    };
                    let svc = CloudService::new();
                    svc.init(config.clone()).await;
                    let result = svc
                        .perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert_eq!(provider.sent.len(), 3);
                    assert_eq!(
                        result.errors,
                        vec![
                            "f.txt: it kept changing while it uploaded; the next cycle reads it as a conflict, asked about under PreferNewer or PreferRemote until one cycle runs under PreferLocal (sends the local copy) or KeepBoth"
                                .to_string()
                        ]
                    );
                    let baseline = svc.load_index(&config).expect("the baseline is saved");
                    assert_eq!(
                        baseline.files.get("f.txt").and_then(|entry| entry.modified),
                        Some(DateTime::<Utc>::UNIX_EPOCH)
                    );
                })
        });
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// Minor 1 (verification of the sixth round): a save inside the tick of
    /// the file's timestamps moves none of them, so an upload reads its file
    /// only once it has been left alone for the quiet window (300 ms here),
    /// its change time included, and waits no longer than the bound.
    #[tokio::test]
    async fn an_upload_waits_for_its_file_to_be_left_alone() {
        let window = std::time::Duration::from_millis(300);
        let at_least = window - std::time::Duration::from_millis(50);
        let root = tempfile::tempdir().expect("local root");
        let file = root.path().join("f.txt");
        std::fs::write(&file, b"payload").unwrap();
        let mut local = fi(7, 1_700_000_000);
        local.path = file.to_string_lossy().into_owned();
        let uploaded = cmp(
            SyncStatus::LocalNewer,
            Some(local),
            Some(fi(3, 1)),
            true,
            false,
        );
        let mut config = cfg(
            CompareDirection::Bidirectional,
            false,
            ConflictStrategy::AskUser,
        );
        config.local_folder = root.path().to_path_buf();
        config.remote_folder = "/remote".to_string();
        let mut svc = CloudService::new();
        svc.send_quiet_window = window;
        let started = std::time::Instant::now();
        svc.process_comparison_with_provider(&mut StampingProvider::default(), &config, &uploaded)
            .await
            .expect("the upload succeeds");
        assert!(
            started.elapsed() >= at_least,
            "a file written just now was sent after {:?}",
            started.elapsed()
        );

        // Left alone past the window: no wait.
        std::fs::write(&file, b"payload").unwrap();
        tokio::time::sleep(window + std::time::Duration::from_millis(50)).await;
        let started = std::time::Instant::now();
        CloudService::wait_until_quiet(
            &file,
            window,
            tokio::time::Instant::now() + SEND_QUIET_BOUND,
        )
        .await;
        assert!(started.elapsed() < std::time::Duration::from_millis(100));

        // Written without pause: the bound ends the wait.
        std::fs::write(&file, b"payload").unwrap();
        let started = std::time::Instant::now();
        CloudService::wait_until_quiet(
            &file,
            std::time::Duration::from_secs(60),
            tokio::time::Instant::now() + window,
        )
        .await;
        let waited = started.elapsed();
        assert!(
            waited >= at_least && waited < std::time::Duration::from_secs(5),
            "{waited:?}"
        );

        // An old time put back on a file written just now: only the change
        // time says so.
        #[cfg(unix)]
        {
            tokio::time::sleep(window + std::time::Duration::from_millis(50)).await;
            std::fs::write(&file, b"payload").unwrap();
            stamp(&file, 1_700_000_000);
            let started = std::time::Instant::now();
            CloudService::wait_until_quiet(
                &file,
                window,
                tokio::time::Instant::now() + SEND_QUIET_BOUND,
            )
            .await;
            assert!(started.elapsed() >= at_least, "{:?}", started.elapsed());
        }
    }

    /// Nit of the verification of the final round, after #956: an FTP stat
    /// whose size could not be read reports 0 with a marker, and the check for
    /// another client's write read that 0 as a different size: no stamp, and a
    /// redundant download the next cycle. A size that says nothing is not
    /// compared.
    #[tokio::test]
    async fn an_unreadable_remote_size_is_not_read_as_another_write() {
        let root = tempfile::tempdir().expect("local root");
        let file = root.path().join("f.txt");
        std::fs::write(&file, b"payload").unwrap();
        stamp(&file, 1_700_000_000);
        let mut provider = StampingProvider {
            size_unreadable: true,
            ..Default::default()
        };
        let (baseline, _) = upload_then_next_cycle(&mut provider, root.path()).await;
        assert_eq!(
            baseline.get("f.txt").and_then(|entry| entry.modified),
            Some(server_time()),
            "the server's time, stamped on the local file"
        );
    }

    /// m1 (verification of 34bf4fdf): the prior index a cycle with errors
    /// adds its sentinels to was migrated when it was loaded, and the keys the
    /// migration could not vouch for live in `unverified_keys`, which is not
    /// saved: written back, they became trusted, and a file whose upload had
    /// failed read as already synced and was deleted locally the next cycle.
    /// They are left out, read as never synced.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_cycle_with_errors_does_not_trust_keys_a_migration_could_not_vouch_for() {
        let _env = crate::test_env::lock();
        let data = tempfile::tempdir().unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", data.path());
        let outcome = std::panic::catch_unwind(|| {
            let entry = |size| SyncIndexEntry {
                size,
                modified: None,
                is_dir: false,
            };
            let prior = SyncIndex {
                version: SYNC_INDEX_VERSION,
                last_sync: Utc::now(),
                local_path: "/l/unverified".to_string(),
                remote_path: "/unverified".to_string(),
                files: HashMap::from([
                    ("sub/b.txt".to_string(), entry(3)),
                    ("kept.txt".to_string(), entry(4)),
                ]),
                unverified_keys: std::collections::HashSet::from(["sub/b.txt".to_string()]),
            };
            let svc = CloudService::new();
            svc.unsettled.lock().unwrap().push("f.txt".to_string());
            svc.save_unsettled_into("/l/unverified", "/unverified", &[], Some(&prior));
            let saved = load_sync_index("/l/unverified", "/unverified")
                .unwrap()
                .expect("the sentinel is recorded");
            assert!(
                !saved.files.contains_key("sub/b.txt"),
                "a key the migration could not vouch for was saved as trusted"
            );
            assert!(saved.files.contains_key("kept.txt"));
            assert_eq!(
                saved.files.get("f.txt").and_then(|entry| entry.modified),
                Some(UNSETTLED_BASELINE)
            );
        });
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// F1 (verification of the final round): a time ahead of the clock was
    /// left out, so a file whose only times were a little ahead was never
    /// waited for. FAT rounds the last write up to its 2 s field, and a
    /// share's clock can run ahead (its change time comes from the server
    /// too): two same-size saves a second apart both read 12 s, and the
    /// second one was recorded as synced. Up to 2 s ahead is a change now;
    /// further ahead says nothing about the last write.
    #[test]
    fn a_time_just_ahead_of_the_clock_reads_as_a_change_now() {
        let now = std::time::SystemTime::now();
        let ahead = |secs| Some(now + std::time::Duration::from_secs(secs));
        let behind = |secs| Some(now - std::time::Duration::from_secs(secs));
        assert_eq!(
            CloudService::quiet_age(now, [ahead(1), ahead(2)]),
            Some(std::time::Duration::ZERO)
        );
        assert_eq!(
            CloudService::quiet_age(now, [behind(5), ahead(1)]),
            Some(std::time::Duration::ZERO)
        );
        assert_eq!(CloudService::quiet_age(now, [ahead(60), None]), None);
        assert_eq!(
            CloudService::quiet_age(now, [behind(5), ahead(60)]),
            Some(std::time::Duration::from_secs(5))
        );
    }

    /// F4 (verification of the final round): a file written without pause
    /// (a log, a database, a disk image) waited the whole bound before each
    /// of its three sends, in series, and held up every file after it. The
    /// bound is the file's, once per cycle.
    #[tokio::test]
    async fn a_file_written_without_pause_waits_the_bound_once() {
        let root = tempfile::tempdir().expect("local root");
        let file = root.path().join("f.txt");
        std::fs::write(&file, b"payload").unwrap();
        let mut local = fi(7, 1_700_000_000);
        local.path = file.to_string_lossy().into_owned();
        let uploaded = cmp(
            SyncStatus::LocalNewer,
            Some(local),
            Some(fi(3, 1)),
            true,
            false,
        );
        let mut config = cfg(
            CompareDirection::Bidirectional,
            false,
            ConflictStrategy::AskUser,
        );
        config.local_folder = root.path().to_path_buf();
        config.remote_folder = "/remote".to_string();
        let mut svc = CloudService::new();
        svc.send_quiet_window = std::time::Duration::from_secs(60);
        svc.send_quiet_bound = std::time::Duration::from_millis(300);
        let mut provider = StampingProvider {
            edits_during_upload: usize::MAX,
            ..Default::default()
        };
        let started = std::time::Instant::now();
        svc.process_comparison_with_provider(&mut provider, &config, &uploaded)
            .await
            .expect("the upload succeeds");
        let took = started.elapsed();
        assert_eq!(provider.sent.len(), 3);
        assert!(
            took >= std::time::Duration::from_millis(250)
                && took < std::time::Duration::from_millis(700),
            "three sends of a file written without pause took {took:?}"
        );
    }

    /// F3 (verification of the final round): a cycle in which another file
    /// failed saved no index, so the sentinel of a file that kept changing
    /// was lost and the next cycle read the partial send as the newer copy.
    /// A cycle with errors still records the sentinels, and only them.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_cycle_with_errors_keeps_the_sentinel_of_a_file_that_kept_changing() {
        let _env = crate::test_env::lock();
        let data = tempfile::tempdir().unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", data.path());
        let outcome = std::panic::catch_unwind(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(async {
                    let local = tempfile::tempdir().unwrap();
                    std::fs::write(local.path().join("f.txt"), b"payload").unwrap();
                    std::fs::write(local.path().join("refused.txt"), b"refused").unwrap();
                    let config = CloudConfig {
                        enabled: true,
                        local_folder: local.path().to_path_buf(),
                        remote_folder: "/with-errors".into(),
                        sync_direction: CompareDirection::Bidirectional,
                        ..Default::default()
                    };
                    let mut provider = StampingProvider {
                        edits_during_upload: usize::MAX,
                        refused: Some("refused.txt"),
                        ..Default::default()
                    };
                    let svc = CloudService::new();
                    svc.init(config.clone()).await;
                    let result = svc
                        .perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert_eq!(result.errors.len(), 2, "{:?}", result.errors);
                    let baseline = svc.load_index(&config).expect("the sentinel is recorded");
                    assert_eq!(
                        baseline.files.get("f.txt").and_then(|entry| entry.modified),
                        Some(DateTime::<Utc>::UNIX_EPOCH)
                    );
                    assert!(
                        !baseline.files.contains_key("refused.txt"),
                        "a cycle with errors advanced another file's baseline"
                    );
                })
        });
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// Major of the sixth verification: an overlay that reports the size on
    /// the wire (AeroCompress, a legacy AeroCrypt: 13 bytes of header at
    /// least) never gives back the size sent, and the check for another
    /// client's write fired on every upload through it: no stamp, and the
    /// remote read as changed the next cycle (a download, and a conflict for
    /// an edit made meanwhile). The check is left to exact-size providers,
    /// as the comparison leaves sizes to them.
    #[tokio::test]
    async fn an_upload_through_an_overlay_that_reports_the_size_on_the_wire_is_in_sync() {
        let root = tempfile::tempdir().expect("local root");
        let file = root.path().join("f.txt");
        std::fs::write(&file, b"payload").unwrap();
        stamp(&file, 1_700_000_000);
        let mut provider = StampingProvider {
            inexact_size: true,
            stat_size: Some(7 + 13),
            ..Default::default()
        };
        let (_, next) = upload_then_next_cycle(&mut provider, root.path()).await;
        assert_eq!(next, SyncStatus::Identical);
    }

    /// m1 (verification of the fourth round): a save made while the remote
    /// is read after the upload is caught before the stamp, and sent again.
    #[tokio::test]
    async fn an_edit_while_the_remote_is_read_is_sent_again() {
        let root = tempfile::tempdir().expect("local root");
        let file = root.path().join("f.txt");
        std::fs::write(&file, b"payload").unwrap();
        stamp(&file, 1_700_000_000);
        let mut provider = StampingProvider {
            edit_during_stat: Some(file.clone()),
            ..Default::default()
        };
        let (_, next) = upload_then_next_cycle(&mut provider, root.path()).await;
        assert_eq!(provider.sent.len(), 2);
        assert_eq!(provider.sent[1], b"EDITED!", "the remote holds the edit");
        assert_eq!(next, SyncStatus::Identical);
    }

    /// Minor 3 (verification of the fifth round): another client's write
    /// between the end of the upload and the stat is not the synced state.
    /// The stat's size is not the size sent: the local file is not stamped
    /// with that write's time, and the next cycle reads the remote as changed.
    #[tokio::test]
    async fn a_remote_written_right_after_the_upload_is_read_as_changed() {
        let root = tempfile::tempdir().expect("local root");
        let file = root.path().join("f.txt");
        std::fs::write(&file, b"payload").unwrap();
        stamp(&file, 1_700_000_000);
        let mut provider = StampingProvider {
            stat_size: Some(9),
            ..Default::default()
        };
        let (baseline, next) = upload_then_next_cycle(&mut provider, root.path()).await;
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
            "the local file keeps its own time"
        );
        let entry = baseline.get("f.txt").expect("the upload is recorded");
        assert_eq!(
            (entry.size, entry.modified),
            (7, DateTime::<Utc>::from_timestamp(1_700_000_000, 0)),
            "what was sent"
        );
        assert_eq!(next, SyncStatus::RemoteNewer);
    }

    /// Through the provider executor: an upload is stamped with the server's
    /// time, the time is noted as the transfer ends, and the baseline records
    /// it even when the file is edited before the cycle is over.
    #[tokio::test]
    async fn an_upload_through_the_provider_path_records_the_stamp() {
        let root = tempfile::tempdir().expect("local root");
        let file = root.path().join("f.txt");
        std::fs::write(&file, b"payload").unwrap();
        stamp(&file, 1_700_000_000);
        let mut local = fi(7, 1_700_000_000);
        local.path = file.to_string_lossy().into_owned();
        let uploaded = cmp(
            SyncStatus::LocalNewer,
            Some(local),
            Some(fi(3, 1)),
            true,
            false,
        );
        let mut config = cfg(
            CompareDirection::Bidirectional,
            false,
            ConflictStrategy::AskUser,
        );
        config.local_folder = root.path().to_path_buf();
        config.remote_folder = "/remote".to_string();
        let svc = CloudService::new();
        let action = svc
            .process_comparison_with_provider(&mut StampingProvider::default(), &config, &uploaded)
            .await
            .expect("the upload succeeds");
        assert!(matches!(action, SyncAction::Upload));
        stamp(&file, 1_900_000_000);
        let files = svc.post_sync_baseline(&[uploaded], &config, None);
        let server_time =
            chrono::NaiveDateTime::parse_from_str("2027-01-15 08:00:00", "%Y-%m-%d %H:%M:%S")
                .unwrap()
                .and_utc();
        assert_eq!(
            files.get("f.txt").and_then(|entry| entry.modified),
            Some(server_time),
            "the server's time, stamped on the local file after the upload"
        );
    }

    /// The size-only window is stated once per folder pair and process, not
    /// every cycle of a folder synced every minute.
    #[test]
    fn a_size_only_window_is_noticed_once_per_pair() {
        let pair = || "/l/notice-once\u{0}/r/notice-once".to_string();
        assert!(first_size_only_notice(pair()));
        assert!(!first_size_only_notice(pair()));
        assert!(first_size_only_notice("/l/other\u{0}/r/other".to_string()));
    }

    #[test]
    fn kept_directory_is_tracked() {
        let entry = CloudService::baseline_entry_for(
            &SyncAction::Skip,
            CompareDirection::Bidirectional,
            None,
            None,
            true,
        )
        .unwrap();
        assert!(entry.is_dir);
    }

    // ---- resolve_action: executor / baseline consistency ----

    // Finding 4: in a directional folder a Conflict must follow the configured
    // conflict strategy (AskUser here), NOT decide_sync_action's "local wins".
    #[test]
    fn resolve_action_conflict_uses_strategy_not_directional_upload() {
        let svc = CloudService::new();
        let c = cmp(
            SyncStatus::Conflict,
            Some(fi(1, 1)),
            Some(fi(2, 2)),
            true,
            false,
        );
        let config = cfg(
            CompareDirection::LocalToRemote,
            true,
            ConflictStrategy::AskUser,
        );
        assert_eq!(svc.resolve_action(&config, &c), SyncAction::AskUser);
    }

    // Finding 5: a directory that would be delete-propagated is downgraded to
    // Skip so it is neither executed nor counted as a delete.
    #[test]
    fn resolve_action_directory_delete_downgraded_to_skip() {
        let svc = CloudService::new();
        let c = cmp(SyncStatus::RemoteOnly, None, Some(dir_info()), true, true);
        let config = cfg(
            CompareDirection::Bidirectional,
            true,
            ConflictStrategy::AskUser,
        );
        assert_eq!(svc.resolve_action(&config, &c), SyncAction::Skip);
    }

    #[test]
    fn resolve_action_file_delete_still_propagates() {
        let svc = CloudService::new();
        let c = cmp(SyncStatus::RemoteOnly, None, Some(fi(5, 5)), true, false);
        let config = cfg(
            CompareDirection::Bidirectional,
            true,
            ConflictStrategy::AskUser,
        );
        assert_eq!(svc.resolve_action(&config, &c), SyncAction::DeleteRemote);
    }

    /// G111 aftermath: a remote object whose name holds a literal backslash
    /// (what the pre-fix scanner uploaded to a Unix server) is neither
    /// downloaded, which would overwrite the nested file it aliases on Windows,
    /// nor deleted, whether or not it is in the baseline. Every direction, and
    /// mirror mode included.
    #[test]
    #[cfg(windows)]
    fn a_remote_name_with_a_backslash_is_left_alone_on_windows() {
        let svc = CloudService::new();
        for direction in [
            CompareDirection::Bidirectional,
            CompareDirection::RemoteToLocal,
            CompareDirection::LocalToRemote,
        ] {
            for previously_synced in [false, true] {
                let mut c = cmp(
                    SyncStatus::RemoteOnly,
                    None,
                    Some(fi(5, 5)),
                    previously_synced,
                    false,
                );
                c.relative_path = "sub\\b.txt".to_string();
                let config = cfg(direction, false, ConflictStrategy::AskUser);
                assert_eq!(
                    svc.resolve_action(&config, &c),
                    SyncAction::Skip,
                    "{direction:?}, previously_synced={previously_synced}"
                );
            }
        }
    }

    // ---- delete_safety_trips: mass-wipe guard ----

    #[test]
    fn safety_trips_on_empty_side_and_mass_delete_but_not_normal() {
        // Whole side empty with a prior baseline -> trip.
        assert!(CloudService::delete_safety_trips(20, true, false, 0, false));
        // More than half of a >= floor baseline pending delete -> trip.
        assert!(CloudService::delete_safety_trips(
            20, false, false, 11, false
        ));
        // A few deletes out of many -> no trip.
        assert!(!CloudService::delete_safety_trips(
            20, false, false, 3, false
        ));
        // Tiny folder below the floor deleting most -> no trip (deliberate).
        assert!(!CloudService::delete_safety_trips(
            3, false, false, 3, false
        ));
        // No prior baseline (first sync) -> never trip.
        assert!(!CloudService::delete_safety_trips(0, true, true, 0, false));
    }

    #[test]
    fn safety_trips_on_incomplete_scan_regardless_of_counts() {
        // An incomplete scan makes every count a lie: the files the scanner
        // could not see are indistinguishable from files the user deleted. Trip
        // even when the counts look perfectly benign (a single delete out of a
        // healthy baseline, neither side empty). (CLAUDE-AV-B3-11)
        assert!(CloudService::delete_safety_trips(20, false, false, 1, true));
        // Same shape WITHOUT the incomplete flag stays a normal, allowed delete:
        // proves the trip comes from completeness, not from the counts.
        assert!(!CloudService::delete_safety_trips(
            20, false, false, 1, false
        ));
        // Below the mass-delete floor too: the floor must not excuse a blind scan.
        assert!(CloudService::delete_safety_trips(3, false, false, 1, true));
        // No prior baseline still wins: nothing was ever baselined, so there is
        // nothing to delete and nothing to protect.
        assert!(!CloudService::delete_safety_trips(0, false, false, 0, true));
    }

    // ---- CLAUDE-AV-B3-16: local scan completeness ----

    #[tokio::test]
    async fn local_scan_complete_on_clean_readable_tree() {
        // A fully readable tree must stay complete: the new completeness flag
        // must not change delete behaviour for clean full local scans.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), b"a").expect("write a");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub/b.txt"), b"b").expect("write b");

        let svc = CloudService::new();
        let config = CloudConfig {
            local_folder: dir.path().to_path_buf(),
            ..Default::default()
        };
        let (files, complete) = svc
            .scan_local_folder(&config)
            .await
            .expect("scan should succeed");

        assert!(complete, "a clean readable tree is a complete local scan");
        assert!(files.contains_key("a.txt"));
        assert!(files.contains_key("sub/b.txt"));
        // G111: the key shape is the contract, not an implementation detail.
        //
        // Asserted only on Windows, and against the filesystem rather than
        // against the code: a backslash cannot appear in a Windows file name,
        // so "no key carries one" is true by construction and stays red if
        // `normalize_relative_key` is ever reduced to the identity. The first
        // version of this check read `*k == normalize_relative_key(k)`, which
        // compared the function under test with itself and would have survived
        // exactly that mutation. Off Windows there is nothing to state: the
        // native separator is already `/`, and a backslash there is a legal
        // character that this scanner must not touch.
        #[cfg(windows)]
        assert!(
            files.keys().all(|k| !k.contains('\\')),
            "scan keys must not carry the native separator, got {:?}",
            files.keys().collect::<Vec<_>>()
        );
        // Call-site wiring: both sides complete must NOT trip on a single delete.
        let local_complete = complete;
        let remote_complete = true;
        assert!(!CloudService::delete_safety_trips(
            20,
            false,
            false,
            1,
            !remote_complete || !local_complete,
        ));
    }

    /// G111, the defect itself rather than the shape of the key.
    ///
    /// `build_comparison_results_with_index` unions the two key sets verbatim,
    /// so if the local scan keeps the platform separator the same file arrives
    /// twice: once as `sub\b.txt` (local only, an upload) and once as
    /// `sub/b.txt` (remote only, a download). Asserting on the union is what
    /// pins the bug; asserting on the key alone only pins its spelling.
    #[tokio::test]
    async fn a_nested_file_meets_its_remote_twin_as_a_single_comparison() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub/b.txt"), b"b").expect("write b");

        let svc = CloudService::new();
        let config = CloudConfig {
            local_folder: dir.path().to_path_buf(),
            ..Default::default()
        };
        let (local_files, _complete) = svc
            .scan_local_folder(&config)
            .await
            .expect("scan should succeed");

        // What every remote scanner produces: keys joined with `/`.
        let mut remote_files: HashMap<String, FileInfo> = HashMap::new();
        remote_files.insert("sub/b.txt".to_string(), fi(1, 1_700_000_000));

        let comparisons = build_comparison_results_with_index(
            local_files,
            remote_files,
            &CompareOptions::default(),
            None,
        );

        let nested: Vec<&String> = comparisons
            .iter()
            .map(|c| &c.relative_path)
            .filter(|p| p.ends_with("b.txt"))
            .collect();
        assert_eq!(
            nested.len(),
            1,
            "one file on both sides must compare once, got {:?}",
            nested
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn local_scan_reports_incomplete_when_entry_unstattable() {
        // Classic Unix: directory mode r-- (0o400) lets readdir list names, but
        // path resolution for lstat/symlink_metadata needs search (x). That is
        // exactly the pre-fix silent-drop path: the entry vanishes from the map
        // and a baselined remote file would read as RemoteOnly / DeleteRemote.
        // (CLAUDE-AV-B3-16)
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("secret.txt"), b"x").expect("write");

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o400))
            .expect("chmod r--");

        let svc = CloudService::new();
        let config = CloudConfig {
            local_folder: dir.path().to_path_buf(),
            ..Default::default()
        };
        let result = svc.scan_local_folder(&config).await;

        // Always restore so TempDir cleanup can remove the tree.
        let _ = std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700));

        let (files, complete) = result.expect("scan should not hard-error");
        assert!(
            !complete,
            "an unstattable local entry must mark the scan incomplete"
        );
        assert!(
            !files.contains_key("secret.txt"),
            "unstattable entry stays out of the map (no fake FileInfo)"
        );
        // Call-site equivalent: !remote_complete || !local_complete with remote
        // clean and local incomplete must trip the gate (benign counts).
        let local_complete = complete;
        let remote_complete = true;
        assert!(CloudService::delete_safety_trips(
            20,
            false,
            false,
            1,
            !remote_complete || !local_complete,
        ));
    }

    #[tokio::test]
    async fn local_scan_absent_root_is_complete_empty_map() {
        // Pre-existing contract: a not-yet-created local folder is a complete
        // empty listing (first-run path). Delete safety still trips via the
        // local_empty count path when a prior baseline exists; do not reclassify
        // as scan_incomplete. (CLAUDE-AV-B3-16)
        let dir = tempfile::tempdir().expect("tempdir");
        let gone = dir.path().join("never-existed");

        let svc = CloudService::new();
        let config = CloudConfig {
            local_folder: gone,
            ..Default::default()
        };
        let (files, complete) = svc
            .scan_local_folder(&config)
            .await
            .expect("absent root is Ok");

        assert!(files.is_empty());
        assert!(
            complete,
            "absent local root stays a complete empty map (empty-side gate covers it)"
        );
    }
}

// SECVAL-B (2026-09-19), lead 5: AeroCloud on a provider whose successful
// listing can omit a stored object (ImageKit: `listing_is_authoritative()` is
// false). Run with XDG_CONFIG_HOME pointing at a scratch folder: the sync index
// and cloud config are written under the data root.
#[cfg(test)]
mod secval_b_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Stores what is uploaded, lists every stored object EXCEPT `omit`
    /// (the ImageKit Media Library behaviour measured on 2026-08-01).
    struct OmittingProvider {
        cwd: String,
        stored: HashMap<String, (u64, String)>,
        omit: Vec<String>,
        deletes: Arc<AtomicUsize>,
    }

    fn parent_and_name(p: &str) -> (String, String) {
        let p = p.trim_end_matches('/');
        match p.rfind('/') {
            Some(0) => ("/".to_string(), p[1..].to_string()),
            Some(i) => (p[..i].to_string(), p[i + 1..].to_string()),
            None => ("/".to_string(), p.to_string()),
        }
    }

    #[async_trait::async_trait]
    impl StorageProvider for OmittingProvider {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> crate::providers::ProviderType {
            crate::providers::ProviderType::ImageKit
        }
        fn display_name(&self) -> String {
            "omitting".to_string()
        }
        fn listing_is_authoritative(&self) -> bool {
            false
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(&mut self, _path: &str) -> Result<Vec<ProviderRemoteEntry>, ProviderError> {
            let mut out = Vec::new();
            let mut directories = std::collections::HashSet::new();
            for (full, (size, mtime)) in &self.stored {
                if let Some(rest) = full.strip_prefix(&format!("{}/", self.cwd)) {
                    if let Some((name, _)) = rest.split_once('/') {
                        if directories.insert(name.to_string()) {
                            out.push(ProviderRemoteEntry::directory(
                                name.to_string(),
                                format!("{}/{}", self.cwd, name),
                            ));
                        }
                        continue;
                    }
                }
                let (parent, name) = parent_and_name(full);
                if parent == self.cwd && !self.omit.contains(&name) {
                    let mut e = ProviderRemoteEntry::directory(name.clone(), full.clone());
                    e.is_dir = false;
                    e.size = *size;
                    e.modified = Some(mtime.clone());
                    out.push(e);
                }
            }
            Ok(out)
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok(self.cwd.clone())
        }
        async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
            self.cwd = path.trim_end_matches('/').to_string();
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            _remote_path: &str,
            _local_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("download".to_string()))
        }
        async fn download_to_bytes(
            &mut self,
            _remote_path: &str,
        ) -> Result<Vec<u8>, ProviderError> {
            Err(ProviderError::NotSupported("download_to_bytes".to_string()))
        }
        async fn upload(
            &mut self,
            local_path: &str,
            remote_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            let meta =
                std::fs::metadata(local_path).map_err(|e| ProviderError::Other(e.to_string()))?;
            let mtime: DateTime<Utc> = meta.modified().unwrap().into();
            self.stored
                .insert(remote_path.to_string(), (meta.len(), mtime.to_rfc3339()));
            Ok(())
        }
        async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
            self.deletes.fetch_add(1, Ordering::SeqCst);
            self.stored.remove(path);
            Ok(())
        }
        async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rename".to_string()))
        }
        async fn stat(&mut self, path: &str) -> Result<ProviderRemoteEntry, ProviderError> {
            let (size, mtime) = self
                .stored
                .get(path)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(path.to_string()))?;
            let (_, name) = parent_and_name(path);
            let mut e = ProviderRemoteEntry::directory(name, path.to_string());
            e.is_dir = false;
            e.size = size;
            e.modified = Some(mtime);
            Ok(e)
        }
        async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
            self.stored
                .get(path)
                .map(|(s, _)| *s)
                .ok_or_else(|| ProviderError::NotFound(path.to_string()))
        }
        async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
            Ok(self.stored.contains_key(path))
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("omitting".to_string())
        }
    }

    // The AeroCloud stack wraps the provider in AeroCompress (compress_enabled)
    // and/or a crypt overlay; the wrapper must carry the inner provider's answer.
    #[test]
    fn lead5_overlay_keeps_the_inner_listing_authority() {
        let inner = OmittingProvider {
            cwd: "/".into(),
            stored: HashMap::new(),
            omit: vec![],
            deletes: Arc::new(AtomicUsize::new(0)),
        };
        assert!(!inner.listing_is_authoritative());
        let wrapped = CompressOverlayProvider::new(Box::new(inner), 3);
        assert!(
            !wrapped.listing_is_authoritative(),
            "CompressOverlayProvider reports the trait default `true` over a non-authoritative provider"
        );
    }

    // Linux only: the sync index lives under the AeroFTP data root, which
    // `XDG_CONFIG_HOME` moves on Linux only, and the property under test does
    // not depend on the platform. Synchronous, so the shared environment lock
    // (a std mutex) is never held across an `.await`.
    #[cfg(target_os = "linux")]
    #[test]
    fn lead5_unlisted_object_does_not_delete_the_local_file() {
        let _env = crate::test_env::lock();
        let data = tempfile::tempdir().unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", data.path());
        let outcome = std::panic::catch_unwind(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(lead5_body())
        });
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_symlink_keeps_remote_file_and_prior_baseline() {
        let _env = crate::test_env::lock();
        let data = tempfile::tempdir().unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", data.path());
        let outcome = std::panic::catch_unwind(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(async {
                    for (directory_link, root_link) in [(false, false), (true, false), (true, true)]
                    {
                        let root = tempfile::tempdir().unwrap();
                        let local = root.path().join("local");
                        if root_link {
                            // A root explicitly chosen through a symlink stays
                            // usable; only links below that root bound the run.
                            let backing = root.path().join("backing");
                            std::fs::create_dir(&backing).unwrap();
                            std::os::unix::fs::symlink(&backing, &local).unwrap();
                        } else {
                            std::fs::create_dir(&local).unwrap();
                        }
                        let linked_file = if directory_link {
                            "linked/data.txt"
                        } else {
                            "linked.txt"
                        };
                        if directory_link {
                            std::fs::create_dir(local.join("linked")).unwrap();
                        }
                        for name in ["keep.txt", linked_file, "deleted.txt"] {
                            std::fs::write(local.join(name), name).unwrap();
                        }
                        let config = CloudConfig {
                            enabled: true,
                            local_folder: local.clone(),
                            remote_folder: "/symlink-review".into(),
                            sync_direction: CompareDirection::Bidirectional,
                            ..Default::default()
                        };
                        let deletes = Arc::new(AtomicUsize::new(0));
                        let mut provider = OmittingProvider {
                            cwd: "/".into(),
                            stored: HashMap::new(),
                            omit: vec![],
                            deletes: deletes.clone(),
                        };
                        let svc = CloudService::new();
                        svc.init(config.clone()).await;
                        let first = svc
                            .perform_full_sync_with_provider(&mut provider)
                            .await
                            .unwrap();
                        assert!(first.errors.is_empty(), "{:?}", first.errors);
                        assert_eq!(first.file_details.len(), 3);

                        // The bytes still exist behind a link; the scan intentionally
                        // does not follow it. This is not a user deletion.
                        let target = root.path().join("relocated.txt");
                        let link_path = local.join(if directory_link {
                            "linked"
                        } else {
                            linked_file
                        });
                        std::fs::rename(&link_path, &target).unwrap();
                        std::os::unix::fs::symlink(&target, &link_path).unwrap();
                        std::fs::remove_file(local.join("deleted.txt")).unwrap();
                        let second = svc
                            .perform_full_sync_with_provider(&mut provider)
                            .await
                            .unwrap();
                        assert!(second.errors.is_empty(), "{:?}", second.errors);
                        assert!(
                            provider
                                .stored
                                .contains_key(&format!("/symlink-review/{linked_file}")),
                            "AeroCloud deleted the remote twin of a skipped local symlink"
                        );
                        assert_eq!(second.deleted, 1, "only the real deletion is propagated");
                        assert_eq!(deletes.load(Ordering::SeqCst), 1);
                        let target_file = if directory_link {
                            target.join("data.txt")
                        } else {
                            target
                        };
                        assert_eq!(std::fs::read(&target_file).unwrap(), linked_file.as_bytes());
                        let baseline = svc.load_index(&config).unwrap();
                        assert!(baseline.files.contains_key(linked_file));
                        assert!(!baseline.files.contains_key("deleted.txt"));

                        // Without a baseline this same unseen path reads as a new
                        // remote file. It must not cause a download through the link.
                        let mut empty_baseline = baseline;
                        empty_baseline.files.clear();
                        save_sync_index(&empty_baseline).unwrap();
                        let fresh = svc
                            .perform_full_sync_with_provider(&mut provider)
                            .await
                            .unwrap();
                        assert!(fresh.errors.is_empty(), "{:?}", fresh.errors);
                        assert_eq!(fresh.downloaded, 0);
                        assert_eq!(std::fs::read(&target_file).unwrap(), linked_file.as_bytes());
                    }
                })
        });
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// F2 (verification of the final round): a file left unsettled (it kept
    /// changing through every send) reads as a conflict next cycle, and
    /// PreferNewer settled it by time: on a backend that sets its own time
    /// the remote, the last send, possibly torn, looked newer and was
    /// downloaded over the complete local file; PreferRemote did the same.
    /// Such a conflict is asked; PreferLocal still sends the local file.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_conflict_on_an_unsettled_file_is_asked_not_settled_by_time() {
        let _env = crate::test_env::lock();
        let data = tempfile::tempdir().unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", data.path());
        let outcome = std::panic::catch_unwind(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(async {
                    for (strategy, asked) in [
                        (ConflictStrategy::PreferNewer, true),
                        (ConflictStrategy::PreferRemote, true),
                        (ConflictStrategy::PreferLocal, false),
                    ] {
                        let local = tempfile::tempdir().unwrap();
                        let file = local.path().join("f.txt");
                        std::fs::write(&file, b"payload").unwrap();
                        std::fs::File::options()
                            .write(true)
                            .open(&file)
                            .unwrap()
                            .set_modified(
                                std::time::UNIX_EPOCH
                                    + std::time::Duration::from_secs(1_750_000_000),
                            )
                            .unwrap();
                        let config = CloudConfig {
                            enabled: true,
                            local_folder: local.path().to_path_buf(),
                            remote_folder: "/unsettled".into(),
                            sync_direction: CompareDirection::Bidirectional,
                            conflict_strategy: strategy.clone(),
                            ..Default::default()
                        };
                        save_sync_index(&SyncIndex {
                            version: SYNC_INDEX_VERSION,
                            last_sync: Utc::now(),
                            local_path: local.path().to_string_lossy().into_owned(),
                            remote_path: "/unsettled".to_string(),
                            files: HashMap::from([(
                                "f.txt".to_string(),
                                SyncIndexEntry {
                                    size: 7,
                                    modified: Some(DateTime::<Utc>::UNIX_EPOCH),
                                    is_dir: false,
                                },
                            )]),
                            unverified_keys: Default::default(),
                        })
                        .unwrap();
                        let remote_time = DateTime::<Utc>::from_timestamp(1_760_000_000, 0)
                            .unwrap()
                            .to_rfc3339();
                        let mut provider = OmittingProvider {
                            cwd: "/".into(),
                            stored: HashMap::from([(
                                "/unsettled/f.txt".to_string(),
                                (7, remote_time),
                            )]),
                            omit: vec![],
                            deletes: Arc::new(AtomicUsize::new(0)),
                        };
                        let svc = CloudService::new();
                        svc.init(config).await;
                        let plan = svc
                            .preview_full_sync_with_provider(&mut provider)
                            .await
                            .unwrap();
                        if asked {
                            assert_eq!((plan.conflicts, plan.downloaded), (1, 0), "{strategy:?}");
                        } else {
                            assert_eq!(plan.uploaded, 1, "{strategy:?}");
                        }
                    }
                })
        });
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    #[cfg(target_os = "linux")]
    async fn lead5_body() {
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("AeroCloud");
        std::fs::create_dir_all(&local).unwrap();
        for i in 1..=5 {
            std::fs::write(local.join(format!("photo{i}.jpg")), format!("jpeg {i}")).unwrap();
        }
        let config = CloudConfig {
            enabled: true,
            local_folder: local.clone(),
            remote_folder: format!("/secval-{}", std::process::id()),
            protocol_type: "imagekit".to_string(),
            sync_direction: CompareDirection::Bidirectional,
            ..CloudConfig::default()
        };
        eprintln!(
            "defaults: direction={:?} versioning={:?}",
            config.sync_direction, config.versioning_strategy
        );
        let deletes = Arc::new(AtomicUsize::new(0));
        let mut provider = OmittingProvider {
            cwd: "/".into(),
            stored: HashMap::new(),
            omit: vec!["photo3.jpg".to_string()],
            deletes: deletes.clone(),
        };
        let svc = CloudService::new();
        svc.init(config.clone()).await;

        let r1 = svc
            .perform_full_sync_with_provider(&mut provider)
            .await
            .unwrap();
        eprintln!(
            "cycle 1: uploaded={} deleted={} errors={:?}",
            r1.uploaded, r1.deleted, r1.errors
        );
        assert_eq!(r1.uploaded, 5);
        assert!(provider.stored.len() == 5, "the object IS stored remotely");

        let r2 = svc
            .perform_full_sync_with_provider(&mut provider)
            .await
            .unwrap();
        let still_there = local.join("photo3.jpg").exists();
        let archived = local.join(".aeroversions").exists();
        eprintln!(
            "cycle 2: uploaded={} deleted={} errors={:?}; local photo3.jpg exists={still_there}; .aeroversions present={archived}; remote deletes={}",
            r2.uploaded,
            r2.deleted,
            r2.errors,
            deletes.load(Ordering::SeqCst)
        );
        assert!(
            still_there,
            "AeroCloud deleted a local file because a non-authoritative listing omitted it"
        );
    }
}
