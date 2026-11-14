// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroCloud Sync Service
// Background synchronization between local and remote folders
// Supports multi-protocol providers: FTP, WebDAV, S3, etc.
// NOTE: Some items prepared for Phase 5+ background sync loop
#![allow(dead_code)]
#![allow(unused_imports)]

use crate::cloud_config::{
    self, validate_compress_level, CloudConfig, CloudSyncStatus, ConflictStrategy,
};
use crate::cloud_pairs;
use crate::cloud_provider_factory;
use crate::compress_overlay_provider::CompressOverlayProvider;
use crate::credential_store;
use crate::crypt_overlay_provider;
use crate::providers::{
    ProviderError, ProviderType, RemoteEntry as ProviderRemoteEntry, StorageProvider,
};
use crate::sync::{
    build_comparison_results_with_index, classify_with_summary, decide_sync_action,
    normalize_relative_key, save_sync_index, validate_relative_path, CloudRemoteObservation,
    CloudTransferReceipt, CompareDirection, CompareOptions, FileComparison, FileInfo, SyncAction,
    SyncIndex, SyncIndexEntry, SyncStatus, SYNC_INDEX_VERSION, UNSETTLED_BASELINE,
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

mod bulk;
mod incremental;
mod root_identity;
pub use incremental::{
    prepare_incremental, run_pair_incremental_cycle, IncrementalHints, IncrementalPlan,
    IncrementalPrep, LocalChanges, REMOTE_WORK_LIMIT,
};

/// Minimum file count for the count-based mass-deletion safeguards.
const MASS_DELETE_FLOOR: usize = 10;

/// Bound a local safety probe while allowing it to stop at the first syncable file.
const LOCAL_SOURCE_PROBE_LIMIT: usize = 100_000;

/// Identity of the directory scanned, independent of ordinary child changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct LocalRootIdentity {
    /// A mount instance or native directory token prevents dev/inode reuse.
    generation: Option<[u8; 32]>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(not(unix))]
    created: std::time::SystemTime,
}

impl LocalRootIdentity {
    /// Fail closed when the root or its replacement cannot be identified.
    fn read(root: &Path) -> Option<Self> {
        let file = root_identity::open_directory(root)?;
        Self::from_file(&file)
    }

    fn from_file(file: &std::fs::File) -> Option<Self> {
        let metadata = file.metadata().ok()?;
        if !metadata.is_dir() {
            return None;
        }
        let generation = root_identity::guard(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(Self {
                generation,
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            Some(Self {
                generation,
                created: metadata.created().ok()?,
            })
        }
    }
}

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
    /// A never-synced destination file needs an explicit AeroSync decision.
    #[serde(default)]
    pub needs_review: bool,
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

/// What the remote walk of a cycle saw: its files, whether it saw the whole
/// tree, and by name what it did not see.
struct RemoteScan {
    files: HashMap<String, FileInfo>,
    complete: bool,
    boundaries: crate::sync_core::ScanBoundaries,
}

/// The scanned + compared sync plan produced by `build_sync_plan_with_provider`.
/// Internal seam shared by the real executor and the `--dry-run` preview.
struct SyncPlan {
    inventory_complete: bool,
    update_policy: UpdatePolicy,
    /// A refused destructive cycle is reported in preview and execution, and
    /// does not advance the last known good baseline.
    delete_refusal: Option<String>,
    inspection_errors: Vec<String>,
    remote_observations: HashMap<String, CloudRemoteObservation>,
    comparisons: Vec<FileComparison>,
    /// Identical files with two spellings. Not executed. The baseline records
    /// them so the watcher can stat the remote object.
    mirrored: Vec<FileComparison>,
    index: Option<SyncIndex>,
    local_str: String,
    remote_str: String,
    /// Baseline paths gone from both sides, forgotten when the cycle ends
    /// clean (see [`CloudService::gone_on_both`]).
    gone_on_both: Vec<String>,
}

/// Timestamp and deletion authority from this cycle's actual scans. The
/// preview, bulk path, executor and baseline all consume the same policy.
#[derive(Clone, Copy, Default)]
struct UpdatePolicy {
    window: crate::sync_core::mtime::ModifyWindow,
    allow_orphan_deletes: bool,
    root_identity: Option<LocalRootIdentity>,
}

impl UpdatePolicy {
    /// One-way Update never writes back to the source and never decides
    /// which copy is newer from size, conflict preferences or sync history.
    fn action(self, config: &CloudConfig, row: &FileComparison) -> SyncAction {
        let upload = config.sync_direction == CompareDirection::LocalToRemote;
        let (source, destination) = if upload {
            (row.local_info.as_ref(), row.remote_info.as_ref())
        } else {
            (row.remote_info.as_ref(), row.local_info.as_ref())
        };
        let transfer = if upload {
            SyncAction::Upload
        } else {
            SyncAction::Download
        };
        match (source, destination) {
            (None, Some(_))
                if row.previously_synced
                    && !config.preserve_remote_deletes
                    && self.allow_orphan_deletes
                    && !row.is_dir =>
            {
                if upload {
                    SyncAction::DeleteRemote
                } else {
                    SyncAction::DeleteLocal
                }
            }
            (Some(_), None) => transfer,
            (Some(source), Some(destination)) => {
                if source.is_dir != destination.is_dir {
                    return SyncAction::AskUser;
                }
                if source.is_dir || row.status == SyncStatus::Identical {
                    return SyncAction::Skip;
                }
                match self.window.order(
                    source.modified.map(|time| time.timestamp()),
                    destination.modified.map(|time| time.timestamp()),
                ) {
                    Some(std::cmp::Ordering::Less) => SyncAction::Skip,
                    Some(_) => transfer,
                    None => SyncAction::AskUser,
                }
            }
            _ => SyncAction::Skip,
        }
    }

    /// A file this device never synced is not evidence of a source deletion.
    /// Keep it out of the baseline and report the explicit manual decision.
    fn untracked_destination(config: &CloudConfig, row: &FileComparison) -> bool {
        if !config.update_only
            || config.preserve_remote_deletes
            || row.previously_synced
            || row.is_dir
            || row.name_match_blocked
        {
            return false;
        }
        match config.sync_direction {
            CompareDirection::LocalToRemote => {
                row.local_info.is_none() && row.remote_info.is_some()
            }
            CompareDirection::RemoteToLocal => {
                row.remote_info.is_none() && row.local_info.is_some()
            }
            CompareDirection::Bidirectional => false,
        }
    }
}

/// Which paths the scans of a pair list, by the rules `scan_local_folder` and
/// `scan_remote_folder_with_provider` apply: `.aeroignore`, the exclude list,
/// the excluded folders. Local links and unreadable local folders are the
/// bound's part (see `CloudService::bound_local_scan_paths`). The full sync
/// forgets by it what is gone from both sides, and the incremental pass picks
/// by it the paths it syncs, so the two read the same rules.
struct ScanScope {
    aeroignore: Option<crate::sync_ignore::AeroIgnore>,
    excludes: crate::sync_exclude::ExcludeMatcher,
    excluded_folders: Vec<String>,
}

impl ScanScope {
    fn new(config: &CloudConfig) -> Result<Self, String> {
        Ok(Self {
            aeroignore: crate::sync_ignore::AeroIgnore::load(&config.local_folder),
            // An invalid configured exclude fails the cycle, as it fails the scan.
            excludes: crate::sync::compile_excludes(&config.exclude_patterns)?,
            excluded_folders: config
                .excluded_folders
                .iter()
                .map(|folder| folder.trim_matches('/').to_string())
                .filter(|folder| !folder.is_empty())
                .collect(),
        })
    }

    fn decision(&self, key: &str, is_dir: bool) -> crate::sync_ignore::ScanDecision {
        crate::sync_ignore::scan_decision(self.aeroignore.as_ref(), key, is_dir, &self.excludes)
    }

    /// Whether `key` is an excluded folder or sits under one.
    fn excluded_folder(&self, key: &str) -> bool {
        self.excluded_folders.iter().any(|folder| {
            key == folder
                || key
                    .strip_prefix(folder.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    }

    /// Whether the scans walk down to `key`: no folder above it is left out.
    fn reaches(&self, key: &str) -> bool {
        key.match_indices('/').all(|(end, _)| {
            let folder = &key[..end];
            self.decision(folder, true) != crate::sync_ignore::ScanDecision::Skip
                && !self.excluded_folder(folder)
        })
    }

    /// Whether both scans list `key` itself. The remote scan lists an
    /// excluded folder itself (it only does not walk into it) and the local
    /// scan does not, so the stricter local rule is the one both meet.
    fn lists(&self, key: &str, is_dir: bool) -> bool {
        validate_relative_path(key).is_ok()
            && self.reaches(key)
            && self.decision(key, is_dir) == crate::sync_ignore::ScanDecision::Keep
            && !(is_dir && self.excluded_folder(key))
    }

    /// Whether the scans walk into the folder `key`.
    fn walks(&self, key: &str) -> bool {
        validate_relative_path(key).is_ok()
            && self.reaches(key)
            && self.decision(key, true) != crate::sync_ignore::ScanDecision::Skip
            && !self.excluded_folder(key)
    }
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
    /// When each upload of the cycle ended, for the baseline's `uploaded_at`.
    uploads_ended: Arc<std::sync::Mutex<HashMap<String, DateTime<Utc>>>>,
    /// Runtime protections must not become successful transfers in the baseline.
    withheld_updates: Arc<std::sync::Mutex<HashMap<String, (String, bool)>>>,
    cloud_receipts: Arc<std::sync::Mutex<HashMap<String, CloudTransferReceipt>>>,
    // Outer None is allowed only for anonymous test/injected providers; a real unresolved account fails closed.
    connection_identity: Option<Option<String>>,
    revoked_receipts: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    receipt_root_cache:
        std::sync::Mutex<Option<(PathBuf, LocalRootIdentity, Option<serde_json::Value>)>>,
    #[cfg(test)]
    native_root_reads: std::sync::atomic::AtomicUsize,
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
    remote_size: Option<u64>,
    remote_modified: Option<DateTime<Utc>>,
    destination_side: CompareDirection,
}

/// A local file's state as an upload reads it (see
/// `CloudService::local_file_state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalFileState {
    len: u64,
    modified: Option<DateTime<Utc>>,
    identity: (u64, i64, i64),
}

/// The size `entry` gives of the file it describes, when it can be compared
/// with a local file: none for an FTP size that could not be read, and on
/// Proton Drive none without the size the uploader claimed, since the listed
/// size is then the stored, encrypted one, which never matches.
fn comparable_remote_size(provider_type: ProviderType, entry: &ProviderRemoteEntry) -> Option<u64> {
    if crate::providers::ftp_listing::size_is_unreadable(entry) {
        return None;
    }
    if let Some(claimed) = entry
        .metadata
        .get("claimed_size")
        .and_then(|size| size.parse().ok())
    {
        return Some(claimed);
    }
    (provider_type != ProviderType::Proton).then_some(entry.size)
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
            uploads_ended: Arc::default(),
            withheld_updates: Arc::default(),
            cloud_receipts: Arc::default(),
            connection_identity: None,
            revoked_receipts: Arc::default(),
            receipt_root_cache: std::sync::Mutex::new(None),
            #[cfg(test)]
            native_root_reads: std::sync::atomic::AtomicUsize::new(0),
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
            .ok()
            .and_then(|meta| {
                meta.modified()
                    .ok()
                    .map(|time| (meta.len(), DateTime::<Utc>::from(time)))
            });
        if let Some((size, modified)) = on_disk {
            self.note_landed_as(
                relative_path,
                Landed {
                    destination_side: CompareDirection::RemoteToLocal,
                    remote_size: None,
                    remote_modified: None,
                    size: Some(size),
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

    /// Note when an upload ended, read as it returned: the steps after it
    /// (a stat, a listing) must not widen what reads as our own stamp.
    fn note_upload_ended(&self, relative_path: &str, ended: DateTime<Utc>) {
        if let Ok(mut uploads) = self.uploads_ended.lock() {
            uploads.insert(relative_path.to_string(), ended);
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
        if let Ok(mut cache) = self.receipt_root_cache.lock() {
            *cache = None;
        }
        if let Ok(mut landed) = self.landed.lock() {
            landed.clear();
        }
        if let Ok(mut uploads) = self.uploads_ended.lock() {
            uploads.clear();
        }
        if let Ok(mut withheld) = self.withheld_updates.lock() {
            withheld.clear();
        }
        if let Ok(mut revoked) = self.revoked_receipts.lock() {
            revoked.clear();
        }
    }

    /// Give the manual recovery route that the active direction policy allows.
    fn unsettled_message(config: &CloudConfig, path: &str) -> String {
        if config.update_only && config.sync_direction != CompareDirection::Bidirectional {
            format!("{path}: source changed during upload; left unsettled. Review both copies in AeroSync and explicitly resolve the transfer before resuming One-way Update")
        } else {
            format!("{path}: it kept changing while it uploaded; the next cycle reads it as a conflict, asked about under PreferNewer or PreferRemote until one cycle runs under PreferLocal (sends the local copy) or KeepBoth")
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
        crate::sync::load_sync_index_for_profile(
            &local,
            &remote,
            config.server_profile_id.as_deref(),
        )
        .ok()
        .flatten()
    }

    /// Load explicit transfer proofs; legacy baseline entries grant no Cloud delete authority.
    fn remember_cloud_receipts(&self, index: Option<&SyncIndex>) {
        if let Ok(mut receipts) = self.cloud_receipts.lock() {
            *receipts = index
                .map(|index| {
                    index
                        .cloud_receipts
                        .iter()
                        .filter(|(key, _)| !index.unverified_keys.contains(*key))
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
        }
    }

    /// Open and identify the same directory the cycle verified. Native volume
    /// and file IDs survive remounts; a mount fallback requires a nonreused token.
    fn receipt_root_key(
        config: &CloudConfig,
        expected: Option<LocalRootIdentity>,
    ) -> Option<serde_json::Value> {
        let expected = expected?;
        // A transient native probe failure must not turn a dev/inode-only
        // scan guard into a usable persisted identity on a later probe.
        expected.generation?;
        let file = root_identity::open_directory(&config.local_folder)?;
        if LocalRootIdentity::from_file(&file) != Some(expected) {
            return None;
        }
        root_identity::native(&file).or_else(|| root_identity::mount_bound(&file))
    }

    fn cached_receipt_root_key(
        &self,
        config: &CloudConfig,
        expected: Option<LocalRootIdentity>,
    ) -> Option<serde_json::Value> {
        let expected = expected?;
        if let Ok(cache) = self.receipt_root_cache.lock() {
            if let Some((_, _, key)) = cache
                .as_ref()
                .filter(|(path, identity, _)| path == &config.local_folder && *identity == expected)
            {
                return key.clone();
            }
        }
        #[cfg(test)]
        self.native_root_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let key = Self::receipt_root_key(config, Some(expected));
        if let Ok(mut cache) = self.receipt_root_cache.lock() {
            *cache = Some((config.local_folder.clone(), expected, key.clone()));
        }
        key
    }

    /// Bind proofs to the actual profile, pair and mounted root without persisting credentials.
    #[cfg(test)]
    fn receipt_scope(config: &CloudConfig) -> String {
        Self::receipt_scope_at_root(config, LocalRootIdentity::read(&config.local_folder))
    }

    #[cfg(test)]
    fn receipt_scope_at_root(
        config: &CloudConfig,
        root_identity: Option<LocalRootIdentity>,
    ) -> String {
        Self::receipt_scope_with_key(config, Self::receipt_root_key(config, root_identity))
    }

    fn receipt_scope_with_key(config: &CloudConfig, root: Option<serde_json::Value>) -> String {
        use sha2::Digest;
        let profile = config
            .server_profile_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .unwrap_or(&config.server_profile);
        let identity = serde_json::json!({
            "profile": profile,
            "protocol": config.protocol_type,
            "local": config.local_folder, "remote": config.remote_folder,
            "root_identity": root,
            "compress": config.compress_enabled,
        });
        format!(
            "{:x}",
            sha2::Sha256::digest(identity.to_string().as_bytes())
        )
    }

    /// Add the factory's resolved target; unknown authenticated accounts cannot authorize deletion.
    fn effective_receipt_scope(&self, config: &CloudConfig) -> Option<String> {
        let key =
            self.cached_receipt_root_key(config, LocalRootIdentity::read(&config.local_folder))?;
        self.bind_receipt_scope(config, Self::receipt_scope_with_key(config, Some(key)))
    }

    /// Reuse a logical remote clock only when raw LIST and the local baseline
    /// are unchanged. This cache cannot create or substitute transfer proof.
    fn cached_remote_observation<'a>(
        index: Option<&'a SyncIndex>,
        scope: Option<&str>,
        row: &FileComparison,
    ) -> Option<&'a CloudRemoteObservation> {
        let index = index?;
        let remote = row.remote_info.as_ref()?;
        let local = row.local_info.as_ref()?;
        let local_key = row
            .local_relative_path
            .as_deref()
            .unwrap_or(&row.relative_path);
        let baseline_key = row.baseline_key.as_deref().unwrap_or(local_key);
        if index.unverified_keys.contains(baseline_key) {
            return None;
        }
        if index
            .files
            .get(baseline_key)
            .is_some_and(|baseline| baseline.modified == Some(UNSETTLED_BASELINE))
        {
            return None;
        }
        let remote_key = row
            .remote_relative_path
            .as_deref()
            .unwrap_or(&row.relative_path);
        let prior = index.cloud_remote_metadata.get(remote_key)?;
        // Same-spelling Identical rows need not exist in comparator history.
        // The observation's verified local snapshot is separate bookkeeping.
        if prior.local_key != local_key
            || prior.local.is_dir
            || prior.local.modified.is_none()
            || prior.local.modified == Some(UNSETTLED_BASELINE)
            || prior.local.size != local.size
            || prior.local.modified != local.modified
        {
            return None;
        }
        let listed = &prior.listed;
        (Some(prior.scope.as_str()) == scope
            && listed.path == remote.path
            && listed.name == remote.name
            && listed.size == remote.size
            && listed.modified == remote.modified
            && listed.is_dir == remote.is_dir
            && listed.checksum == remote.checksum
            && listed.checksum_alg == remote.checksum_alg)
            .then_some(prior)
    }

    /// Mutation attempts retire metadata observations even if the write failed.
    fn retain_remote_observations(
        &self,
        mut observations: HashMap<String, CloudRemoteObservation>,
        rows: &[FileComparison],
    ) -> HashMap<String, CloudRemoteObservation> {
        let Ok(revoked) = self.revoked_receipts.lock() else {
            return HashMap::new();
        };
        for row in rows {
            let local = row
                .local_relative_path
                .as_deref()
                .unwrap_or(&row.relative_path);
            if revoked.contains(local)
                || row
                    .baseline_key
                    .as_ref()
                    .is_some_and(|key| revoked.contains(key))
            {
                observations.remove(
                    row.remote_relative_path
                        .as_deref()
                        .unwrap_or(&row.relative_path),
                );
            }
        }
        observations
    }

    fn bind_receipt_scope(&self, config: &CloudConfig, base: String) -> Option<String> {
        use sha2::Digest;
        match &self.connection_identity {
            Some(Some(identity)) => Some(format!(
                "{:x}",
                sha2::Sha256::digest(format!("{base}:{identity}").as_bytes())
            )),
            Some(None) => None,
            None if config.server_profile.is_empty() => Some(base),
            None => None,
        }
    }

    /// Match both actual path identities and the side this transfer established.
    fn cloud_receipt_for(
        &self,
        config: &CloudConfig,
        row: &FileComparison,
    ) -> Option<CloudTransferReceipt> {
        let local = row
            .local_relative_path
            .as_deref()
            .unwrap_or(&row.relative_path);
        let remote = row
            .remote_relative_path
            .as_deref()
            .unwrap_or(&row.relative_path);
        let scope = self.effective_receipt_scope(config)?;
        let receipts = self.cloud_receipts.lock().ok()?;
        let matches = |receipt: &&CloudTransferReceipt| {
            receipt.remote_key == remote
                && receipt.destination_side == config.sync_direction
                && receipt.scope == scope
        };
        receipts
            .get(local)
            .filter(matches)
            .or_else(|| {
                if row.local_info.is_none() {
                    row.baseline_key
                        .as_ref()
                        .and_then(|key| receipts.get(key))
                        .filter(matches)
                } else {
                    None
                }
            })
            .cloned()
    }

    /// Keep comparator history distinct from the authority used by background Update.
    fn apply_cloud_authority(&self, config: &CloudConfig, rows: &mut [FileComparison]) {
        if config.update_only && config.sync_direction != CompareDirection::Bidirectional {
            for row in rows {
                // When the remote source is absent, the generic classifier may project its
                // local spelling. Only the exact counterpart recorded by this transfer is probed.
                if row.remote_info.is_none() && row.local_info.is_some() {
                    let local = row
                        .local_relative_path
                        .as_deref()
                        .unwrap_or(&row.relative_path);
                    let projected = row
                        .remote_relative_path
                        .as_deref()
                        .unwrap_or(&row.relative_path);
                    if projected == local {
                        let recorded = self
                            .cloud_receipts
                            .lock()
                            .ok()
                            .and_then(|receipts| receipts.get(local).cloned());
                        if let Some(receipt) = recorded.filter(|receipt| {
                            receipt.destination_side == config.sync_direction
                                && Some(receipt.scope.clone())
                                    == self.effective_receipt_scope(config)
                        }) {
                            row.remote_relative_path = Some(receipt.remote_key);
                        }
                    }
                }
                row.previously_synced = self.cloud_receipt_for(config, row).is_some();
            }
        }
    }

    /// Compare metadata with the destination actually left by the last stabilized transfer.
    fn receipt_matches(
        config: &CloudConfig,
        receipt: &CloudTransferReceipt,
        size: u64,
        modified: Option<DateTime<Utc>>,
    ) -> bool {
        !receipt.destination.is_dir
            && receipt.destination.modified.is_some()
            && receipt.destination.modified != Some(UNSETTLED_BASELINE)
            && receipt.destination.size == size
            && if Self::is_pre_utc_fix_filelu_receipt(config, receipt) {
                // Its time is the old reading, never a real instant: an exact
                // match would accept a same-size file uploaded 4 or 5 hours
                // before the one AeroCloud wrote (moved in, or restored from a
                // version or the trash, keeps its upload time).
                Self::pre_utc_fix_filelu_receipt(config, receipt, modified)
            } else {
                receipt.destination.modified == modified
            }
    }

    /// Whether `receipt` is a FileLu upload receipt saved before FileLu times
    /// were read as US Eastern: unmarked, on the remote side, for a FileLu
    /// pair. Its time is the old reading of FileLu's clock, so it is compared
    /// only through [`Self::pre_utc_fix_filelu_receipt`].
    fn is_pre_utc_fix_filelu_receipt(config: &CloudConfig, receipt: &CloudTransferReceipt) -> bool {
        config.protocol_type == "filelu"
            && !receipt.utc_remote_time
            && receipt.destination_side == CompareDirection::LocalToRemote
    }

    /// Only for FileLu upload receipts written before FileLu times were read
    /// as US Eastern (4.2.6 and earlier): such a receipt holds the old reading,
    /// 4 or 5 hours early, of the very time FileLu still lists. Without this
    /// every such file would read as changed on the server, and a local
    /// deletion would never reach it. The first saved cycle rewrites those
    /// receipts with the real time ([`Self::refresh_pre_utc_fix_filelu_receipts`]),
    /// so this branch only serves receipts that cycle has not seen yet.
    ///
    /// A receipt marked `utc_remote_time` (minted or migrated since the fix)
    /// never matches this way: its time is already the real one, and reading
    /// the listed time back 4 or 5 hours would let a same-size replacement
    /// uploaded exactly that much later pass as the file AeroCloud wrote.
    fn pre_utc_fix_filelu_receipt(
        config: &CloudConfig,
        receipt: &CloudTransferReceipt,
        modified: Option<DateTime<Utc>>,
    ) -> bool {
        Self::is_pre_utc_fix_filelu_receipt(config, receipt)
            && modified
                .and_then(crate::providers::filelu::pre_utc_fix_reading)
                .is_some_and(|old| receipt.destination.modified == Some(old))
    }

    /// Rewrite, with the real time, every FileLu upload receipt written before
    /// FileLu times were read as US Eastern whose file this cycle listed on the
    /// server with the same size, so the old reading does not outlive the
    /// first saved cycle, within the active receipt scope only. Only the time
    /// changes, and the receipt is marked
    /// `utc_remote_time` so the old reading is never tried on it again: a
    /// receipt is never minted, moved or extended here.
    fn refresh_pre_utc_fix_filelu_receipts(
        receipts: &mut HashMap<String, CloudTransferReceipt>,
        rows: &[FileComparison],
        config: &CloudConfig,
        scope: Option<&str>,
    ) {
        // Only receipts of the connection this cycle listed: a cycle under
        // another account or root must not touch them.
        let Some(scope) = scope else {
            return;
        };
        for row in rows.iter().filter(|row| !row.is_dir) {
            let Some(listed) = row.remote_info.as_ref().filter(|info| !info.is_dir) else {
                continue;
            };
            let local = row
                .local_relative_path
                .as_deref()
                .unwrap_or(&row.relative_path);
            let remote = row
                .remote_relative_path
                .as_deref()
                .unwrap_or(&row.relative_path);
            let Some(receipt) = receipts
                .get_mut(local)
                .filter(|receipt| receipt.remote_key == remote && receipt.scope == scope)
            else {
                continue;
            };
            if !receipt.destination.is_dir
                && receipt.destination.size == listed.size
                && Self::pre_utc_fix_filelu_receipt(config, receipt, listed.modified)
            {
                receipt.destination.modified = listed.modified;
                receipt.utc_remote_time = true;
            }
        }
    }

    /// Changed or unverifiable destination-only files require an explicit foreground decision.
    fn changed_destination_reason(
        &self,
        config: &CloudConfig,
        row: &FileComparison,
    ) -> Option<String> {
        if !config.update_only || config.preserve_remote_deletes || row.is_dir {
            return None;
        }
        let destination = match config.sync_direction {
            CompareDirection::LocalToRemote if row.local_info.is_none() => row.remote_info.as_ref(),
            CompareDirection::RemoteToLocal if row.remote_info.is_none() => row.local_info.as_ref(),
            _ => return None,
        }?;
        let receipt = self.cloud_receipt_for(config, row)?;
        (!Self::receipt_matches(config, &receipt, destination.size, destination.modified)).then(||
            "Destination changed or cannot be verified against the last AeroCloud transfer; kept. Review both sides in AeroSync before deciding on deletion.".to_string())
    }

    /// Revoke persisted proof before an attempted mutation; error writeback saves it too.
    /// Keep the cycle's planning snapshot immutable so baseline writeback still
    /// resolves a successfully executed delete as a delete, rather than a Skip.
    fn revoke_cloud_receipt(&self, row: &FileComparison) {
        let local = row
            .local_relative_path
            .as_deref()
            .unwrap_or(&row.relative_path);
        let keys = std::iter::once(local).chain(row.baseline_key.as_deref());
        for key in keys {
            if let Ok(mut revoked) = self.revoked_receipts.lock() {
                revoked.insert(key.to_string());
            }
        }
    }

    /// Mint proof only from completed, stabilized Cloud transfers, never Skip or mirrored rows.
    fn receipts_after(
        &self,
        rows: &[FileComparison],
        config: &CloudConfig,
        prior: Option<&SyncIndex>,
        files: &HashMap<String, SyncIndexEntry>,
        captured_root: Option<LocalRootIdentity>,
    ) -> HashMap<String, CloudTransferReceipt> {
        let mut receipts = prior
            .map(|index| index.cloud_receipts.clone())
            .unwrap_or_default();
        receipts.retain(|key, _| {
            files.contains_key(key)
                && prior.is_none_or(|index| !index.unverified_keys.contains(key))
        });
        if let Ok(revoked) = self.revoked_receipts.lock() {
            for key in revoked.iter() {
                receipts.remove(key);
            }
        }
        Self::refresh_pre_utc_fix_filelu_receipts(
            &mut receipts,
            rows,
            config,
            self.effective_receipt_scope(config).as_deref(),
        );
        // A root swapped after the last successful transfer cannot inherit that
        // transfer's proof. Keep old scopes and persisted revocations, but never
        // mint or rebind a receipt to the replacement directory.
        if captured_root.is_none() || LocalRootIdentity::read(&config.local_folder) != captured_root
        {
            return receipts;
        }
        let Some(root_key) = self.cached_receipt_root_key(config, captured_root) else {
            return receipts;
        };
        let Some(scope) =
            self.bind_receipt_scope(config, Self::receipt_scope_with_key(config, Some(root_key)))
        else {
            return receipts;
        };
        let Ok(landed) = self.landed.lock() else {
            return receipts;
        };
        for row in rows
            .iter()
            .filter(|row| !row.is_dir && !row.name_match_blocked)
        {
            let local = row
                .local_relative_path
                .as_deref()
                .unwrap_or(&row.relative_path);
            let Some(done) = landed.get(&row.relative_path) else {
                continue;
            };
            receipts.remove(local);
            if done.modified == UNSETTLED_BASELINE {
                continue;
            }
            let (destination_side, destination) = match done.destination_side {
                CompareDirection::LocalToRemote => {
                    let Some(size) = done.remote_size else {
                        continue;
                    };
                    (
                        CompareDirection::LocalToRemote,
                        SyncIndexEntry {
                            size,
                            modified: done.remote_modified,
                            is_dir: false,
                            uploaded_at: None,
                        },
                    )
                }
                CompareDirection::RemoteToLocal => {
                    let Some(size) = done.size else {
                        continue;
                    };
                    (
                        CompareDirection::RemoteToLocal,
                        SyncIndexEntry {
                            size,
                            modified: Some(done.modified),
                            is_dir: false,
                            uploaded_at: None,
                        },
                    )
                }
                _ => continue,
            };
            receipts.insert(
                local.to_string(),
                CloudTransferReceipt {
                    scope: scope.clone(),
                    remote_key: row
                        .remote_relative_path
                        .as_deref()
                        .unwrap_or(&row.relative_path)
                        .to_string(),
                    destination_side,
                    destination,
                    utc_remote_time: true,
                },
            );
        }
        receipts
    }

    /// Resolve the action for a comparison EXACTLY as the executor does, so the
    /// post-sync baseline is derived from the same decision that actually ran.
    /// Conflicts/size mismatches go through the configured conflict strategy;
    /// every other status goes through the central `decide_sync_action`.
    /// Directory deletes are downgraded to `Skip`: AeroCloud removes files, not
    /// directories, so counting or executing a dir delete would inflate the
    /// report and orphan the tree.
    fn resolve_action(
        &self,
        config: &CloudConfig,
        comparison: &FileComparison,
        policy: UpdatePolicy,
    ) -> SyncAction {
        if comparison.name_match_blocked {
            return SyncAction::Skip;
        }
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
        if config.update_only && config.sync_direction != CompareDirection::Bidirectional {
            if left_unsettled() {
                return SyncAction::AskUser;
            }
            if self
                .changed_destination_reason(config, comparison)
                .is_some()
            {
                return SyncAction::Skip;
            }
            let orphan = match config.sync_direction {
                CompareDirection::LocalToRemote => {
                    comparison.local_info.is_none() && comparison.remote_info.is_some()
                }
                CompareDirection::RemoteToLocal => {
                    comparison.remote_info.is_none() && comparison.local_info.is_some()
                }
                _ => false,
            };
            if orphan && !comparison.is_dir && self.cloud_receipt_for(config, comparison).is_none()
            {
                return SyncAction::Skip;
            }
            return policy.action(config, comparison);
        }
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
    fn count_pending_deletes(
        &self,
        config: &CloudConfig,
        comparisons: &[FileComparison],
        policy: UpdatePolicy,
    ) -> usize {
        comparisons
            .iter()
            .filter(|c| {
                matches!(
                    self.resolve_action(config, c, policy),
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

    /// Bound deletion of proven, unchanged Cloud destinations against the
    /// complete destination count on every full scan.
    fn update_delete_safety_trips(pending_deletes: usize, destination_files: usize) -> bool {
        pending_deletes >= MASS_DELETE_FLOOR && pending_deletes > destination_files / 2
    }

    /// Persist the post-sync baseline so the NEXT cycle can tell a deleted file
    /// (was baselined, now gone on one side) from a genuinely new file (never
    /// baselined). See [`Self::post_sync_baseline`] for how it is computed.
    fn save_post_sync_index(
        &self,
        plan: &SyncPlan,
        result: &SyncOperationResult,
        config: &CloudConfig,
    ) {
        // Only persist on a clean run (no errors) to avoid advancing the
        // "last known good" snapshot on a partial/broken cycle. The one thing
        // such a cycle still records is the sentinel of a file that kept
        // changing while it uploaded: without it the next cycle reads the
        // remote's last send, possibly torn, as the newer copy.
        if !result.errors.is_empty() {
            self.save_unsettled_into(
                &plan.local_str,
                &plan.remote_str,
                &plan.comparisons,
                plan.index.as_ref(),
                config.server_profile_id.as_deref(),
                Some((config, plan.update_policy.root_identity)),
            );
            return;
        }
        let mut rows = plan.comparisons.clone();
        rows.extend(plan.mirrored.iter().cloned());
        let mut files =
            self.post_sync_baseline(&rows, config, plan.index.as_ref(), plan.update_policy);
        // Gone on both sides: nothing left to track. Kept, the entry would
        // read a file created later at the same path as synced, and deleted.
        for key in &plan.gone_on_both {
            files.remove(key);
        }
        let remote_spellings = crate::sync::spellings_after_baseline(
            &rows,
            plan.index.as_ref().map(|index| &index.remote_spellings),
            &files,
        );
        let cloud_receipts = self.receipts_after(
            &rows,
            config,
            plan.index.as_ref(),
            &files,
            plan.update_policy.root_identity,
        );
        let idx = SyncIndex {
            // The profile this pair syncs with, so that a switch of that
            // profile's client can move the baseline (Requested 79).
            profile_id: config.server_profile_id.clone(),
            // Carried over from #854 when the two branches met here: the index
            // this writes is a v2 index, and a freshly written one has nothing
            // unverified in it, because `unverified_keys` marks only the keys a
            // migration rewrote on read.
            version: SYNC_INDEX_VERSION,
            last_sync: Utc::now(),
            local_path: plan.local_str.clone(),
            remote_path: plan.remote_str.clone(),
            files,
            remote_spellings,
            cloud_receipts,
            cloud_remote_metadata: self
                .retain_remote_observations(plan.remote_observations.clone(), &rows),
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
        profile_id: Option<&str>,
        transfer_context: Option<(&CloudConfig, Option<LocalRootIdentity>)>,
    ) {
        let unsettled = self
            .unsettled
            .lock()
            .map(|unsettled| unsettled.clone())
            .unwrap_or_default();
        let revoked = self
            .revoked_receipts
            .lock()
            .map(|keys| keys.clone())
            .unwrap_or_default();
        if unsettled.is_empty() && revoked.is_empty() {
            return;
        }
        let mut idx = prior_index.cloned().unwrap_or_else(|| SyncIndex {
            profile_id: None,
            version: SYNC_INDEX_VERSION,
            last_sync: Utc::now(),
            local_path: local.to_string(),
            remote_path: remote.to_string(),
            files: HashMap::new(),
            remote_spellings: Default::default(),
            cloud_receipts: Default::default(),
            cloud_remote_metadata: Default::default(),
            unverified_keys: Default::default(),
        });
        // The prior index was migrated when it was loaded, and the keys the
        // migration could not vouch for are not saved with it: written back
        // they would be trusted, and a file never synced would read as synced
        // (its local copy deleted the next cycle). Left out, they read as
        // never synced, as the compare reads them now.
        for key in revoked {
            idx.cloud_receipts.remove(&key);
            let remote = idx.remote_spellings.get(&key).unwrap_or(&key);
            idx.cloud_remote_metadata.remove(remote);
        }
        idx.cloud_remote_metadata =
            self.retain_remote_observations(idx.cloud_remote_metadata, comparisons);
        let unverified = std::mem::take(&mut idx.unverified_keys);
        idx.files.retain(|path, _| !unverified.contains(path));
        idx.cloud_receipts
            .retain(|path, _| !unverified.contains(path));
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
                    uploaded_at: None,
                },
            );
        }
        // A failure elsewhere must not erase proof of transfers that really
        // completed. Advance only those landed identities, never Skip/history.
        if let Some((config, captured_root)) = transfer_context.filter(|(config, _)| {
            config.update_only && config.sync_direction != CompareDirection::Bidirectional
        }) {
            let landed = self
                .landed
                .lock()
                .map(|rows| rows.clone())
                .unwrap_or_default();
            let mut transferred = Vec::new();
            for row in comparisons
                .iter()
                .filter(|row| !row.is_dir && !row.name_match_blocked)
            {
                let Some(done) = landed
                    .get(&row.relative_path)
                    .filter(|done| done.modified != UNSETTLED_BASELINE)
                else {
                    continue;
                };
                let Some(size) = done.size else {
                    continue;
                };
                let key = row
                    .local_relative_path
                    .as_deref()
                    .unwrap_or(&row.relative_path);
                if let Some(old) = row.baseline_key.as_ref().filter(|old| old.as_str() != key) {
                    idx.files.remove(old);
                }
                idx.files.insert(
                    key.to_string(),
                    SyncIndexEntry {
                        size,
                        modified: Some(done.modified),
                        is_dir: false,
                        uploaded_at: None,
                    },
                );
                transferred.push(row.clone());
            }
            idx.remote_spellings = crate::sync::spellings_after_baseline(
                &transferred,
                Some(&idx.remote_spellings),
                &idx.files,
            );
            idx.cloud_receipts =
                self.receipts_after(&transferred, config, Some(&idx), &idx.files, captured_root);
        }
        idx.profile_id = profile_id.map(str::to_string);
        if let Err(e) = save_sync_index(&idx) {
            tracing::warn!("Failed to save AeroCloud sync index: {}", e);
        }
    }

    /// Restore migrated-key authority only after a complete comparison proves both copies identical.
    fn verify_migrated_baseline(
        prior: &mut SyncIndex,
        locals: &HashMap<String, FileInfo>,
        remotes: &HashMap<String, FileInfo>,
        options: &CompareOptions,
        direction: CompareDirection,
    ) {
        for key in prior.unverified_keys.clone() {
            let (Some(local), Some(remote)) = (locals.get(&key), remotes.get(&key)) else {
                continue;
            };
            let checked = classify_with_summary(
                HashMap::from([(key.clone(), local.clone())]),
                HashMap::from([(key.clone(), remote.clone())]),
                options,
                None,
            );
            if checked.summary.examined_count == 1
                && checked.summary.identical_count == 1
                && checked.differences.is_empty()
                && checked.mirrored.is_empty()
            {
                if let Some(entry) = Self::baseline_entry_for(
                    &SyncAction::Skip,
                    direction,
                    Some(local),
                    Some(remote),
                    false,
                ) {
                    prior.files.insert(key.clone(), entry);
                    prior.unverified_keys.remove(&key);
                }
            }
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
    /// was synced back into a new one. A path gone from both sides is in no
    /// comparison: the caller forgets it (see [`Self::gone_on_both`]).
    /// A blocked name match is not a synced file. A file paired with a
    /// directory, or an ambiguous spelling, is skipped with both sides
    /// present, and recording that skip would let a later cycle treat the
    /// survivor as previously synced and delete it. The prior entry stays.
    fn post_sync_baseline(
        &self,
        comparisons: &[FileComparison],
        config: &CloudConfig,
        prior_index: Option<&SyncIndex>,
        policy: UpdatePolicy,
    ) -> HashMap<String, SyncIndexEntry> {
        let mut index_files: HashMap<String, SyncIndexEntry> =
            prior_index.map(|i| i.files.clone()).unwrap_or_default();
        // A migration candidate is not proof of a previous sync. A clean save
        // must not silently convert an unverified one-sided key into authority.
        if let Some(prior) = prior_index {
            index_files.retain(|key, _| !prior.unverified_keys.contains(key));
        }
        let withheld = match self.withheld_updates.lock() {
            Ok(rows) => rows.clone(),
            Err(_) => return index_files,
        };
        // Retire prior identities before any new baseline is written. A
        // download can reuse a deleted local name while another row deletes
        // its old mapped remote counterpart; row order must not erase it.
        for c in comparisons.iter().filter(|c| !c.name_match_blocked) {
            if withheld.contains_key(&c.relative_path) {
                continue;
            }
            let action = self.resolve_action(config, c, policy);
            if matches!(action, SyncAction::DeleteLocal | SyncAction::DeleteRemote) {
                index_files.remove(c.baseline_key.as_deref().unwrap_or(&c.relative_path));
            } else if Self::baseline_entry_for(
                &action,
                config.sync_direction,
                c.local_info.as_ref(),
                c.remote_info.as_ref(),
                c.is_dir,
            )
            .is_some()
            {
                if let Some(key) = c
                    .baseline_key
                    .as_ref()
                    .filter(|key| *key != &c.relative_path)
                {
                    index_files.remove(key);
                }
            }
        }
        for c in comparisons {
            if c.name_match_blocked || withheld.contains_key(&c.relative_path) {
                continue;
            }
            let action = self.resolve_action(config, c, policy);
            if matches!(action, SyncAction::DeleteLocal | SyncAction::DeleteRemote) {
                continue;
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
                if matches!(
                    action,
                    SyncAction::Upload | SyncAction::Download | SyncAction::KeepBoth
                ) && !c.is_dir
                {
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
                    // When this upload ended, not now: a remote edit made later
                    // in the cycle must stay an edit.
                    if matches!(action, SyncAction::Upload) {
                        entry.uploaded_at = self
                            .uploads_ended
                            .lock()
                            .ok()
                            .and_then(|uploads| uploads.get(&c.relative_path).copied());
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
    /// - `KeepBoth` moved the local copy aside under a conflict name and put
    ///   the remote copy at the path -> record remote, as for `Download`.
    ///   Recording nothing left the old baseline, against which the path
    ///   (the remote copy, changed) and the remote both read as changed: the
    ///   next cycle saw the same conflict and set aside another copy, one
    ///   per cycle. The copy set aside is a new local file the next cycle
    ///   sends.
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
    /// (`AskUser`); deletes are handled by the caller.
    fn baseline_entry_for(
        action: &SyncAction,
        direction: CompareDirection,
        local_info: Option<&FileInfo>,
        remote_info: Option<&FileInfo>,
        is_dir: bool,
    ) -> Option<SyncIndexEntry> {
        let info = match action {
            SyncAction::Upload => local_info,
            SyncAction::Download | SyncAction::KeepBoth => remote_info,
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
                uploaded_at: None,
            }),
            // A kept directory carries no size/mtime but must stay tracked so it
            // is not treated as new (and re-created) every cycle.
            None if is_dir && matches!(action, SyncAction::Skip) => Some(SyncIndexEntry {
                size: 0,
                modified: None,
                is_dir: true,
                uploaded_at: None,
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
        // Execution can initialize a missing receive-only destination; preview stays read-only.
        if ensure_remote
            && config.sync_direction == CompareDirection::RemoteToLocal
            && !config.local_folder.exists()
        {
            std::fs::create_dir_all(&config.local_folder).map_err(|error| error.to_string())?;
        }
        let scan_root_identity = LocalRootIdentity::read(&config.local_folder);
        // Probe the remote folder. A real run creates it if missing (check first:
        // some providers like FileLu create duplicates if mkdir is called on an
        // existing folder). A preview (`ensure_remote == false`) must NOT mutate
        // the target: if the folder is absent it is treated as empty instead.
        //
        // A failed `cd` is not proof the folder is absent (a timeout, a denial),
        // and the preview below reads "absent" as a complete, empty remote. So a
        // `cd` error is settled by `exists`, which answers false only for a
        // NotFound; one that cannot answer either stops the plan.
        let remote_present = match provider.cd(&config.remote_folder).await {
            Ok(()) => true,
            Err(cd_error) => match provider.exists(&config.remote_folder).await {
                Ok(present) => present,
                Err(e) => {
                    return Err(format!(
                        "Cannot open remote folder {} ({cd_error}) nor check that it exists: {e}",
                        config.remote_folder
                    ))
                }
            },
        };
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
        let (mut remote_files, remote_complete, remote_boundaries) =
            if remote_present || ensure_remote {
                match self
                    .scan_remote_folder_with_provider(provider, config)
                    .await
                {
                    Ok(scan) => (scan.files, scan.complete, scan.boundaries),
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
                (
                    HashMap::new(),
                    true,
                    crate::sync_core::ScanBoundaries::default(),
                )
            };

        let local_str = config.local_folder.to_string_lossy().to_string();
        let remote_str = config.remote_folder.clone();
        // A remote folder that did not list holds files the plan cannot see:
        // left in, each local file under it reads as LocalOnly and goes up
        // over a remote copy nobody compared it with. Both sides of every
        // such subtree stay out of this cycle (their baseline entries are
        // carried forward), and a scan that missed a part it cannot name
        // refuses the cycle, as the GUI Compare does.
        Self::bound_scan_paths(
            &local_str,
            &mut local_files,
            &mut remote_files,
            remote_boundaries,
        )?;

        // Load prior sync index (if any) for delete propagation + conflict detection.
        let mut index = self.load_index(config);
        self.remember_unsettled(index.as_ref());
        let prior_count = index.as_ref().map_or(0, |i| i.files.len());
        let local_len = local_files.len();
        let remote_len = remote_files.len();
        let local_file_count = local_files.values().filter(|file| !file.is_dir).count();
        let remote_file_count = remote_files.values().filter(|file| !file.is_dir).count();
        // Only a scan that saw the whole tree can say a path is gone.
        let gone_on_both = if local_complete && remote_complete {
            Self::gone_on_both(
                config,
                &local_str,
                &local_files,
                &remote_files,
                index.as_ref(),
            )?
        } else {
            Vec::new()
        };

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

        // Inspect only rows that can change a decision, not every unchanged or
        // never-transferred destination file. LIST remains the global alias context.
        self.remember_cloud_receipts(index.as_ref());
        let mut inspection_errors = Vec::new();
        let mut remote_observations = HashMap::new();
        let mut quarantined = std::collections::HashSet::new();
        if config.update_only && config.sync_direction != CompareDirection::Bidirectional {
            let scope = self.effective_receipt_scope(config);
            let initial = classify_with_summary(
                local_files.clone(),
                remote_files.clone(),
                &options,
                index.as_ref(),
            );
            let mut candidates = initial.differences;
            self.apply_cloud_authority(config, &mut candidates);
            for row in candidates
                .iter()
                .filter(|row| !row.is_dir && !row.name_match_blocked)
            {
                let Some(info) = row.remote_info.as_ref().filter(|info| !info.is_dir) else {
                    continue;
                };
                let relevant = row.local_info.is_some()
                    || config.sync_direction == CompareDirection::RemoteToLocal
                    || (!config.preserve_remote_deletes
                        && self.cloud_receipt_for(config, row).is_some());
                if !relevant {
                    continue;
                }
                let key = row
                    .remote_relative_path
                    .as_deref()
                    .unwrap_or(&row.relative_path);
                if let Some(prior) =
                    Self::cached_remote_observation(index.as_ref(), scope.as_deref(), row)
                {
                    if let Some(snapshot) = remote_files.get_mut(key) {
                        *snapshot = prior.logical.clone();
                    }
                    remote_observations.insert(key.to_string(), prior.clone());
                    continue;
                }
                match incremental::remote_state(provider, &info.path).await {
                    Ok(Some(current)) if !current.is_dir => {
                        let key = row
                            .remote_relative_path
                            .as_deref()
                            .unwrap_or(&row.relative_path);
                        if let Some(snapshot) = remote_files.get_mut(key) {
                            snapshot.size = current.size;
                            snapshot.checksum = current.metadata.get("content_hash").cloned();
                            snapshot.checksum_alg = None;
                            snapshot.modified = current
                                .modified
                                .as_deref()
                                .and_then(crate::parse_remote_datetime);
                            if let (Some(scope), Some(local)) =
                                (scope.as_ref(), row.local_info.as_ref())
                            {
                                remote_observations.insert(
                                    key.to_string(),
                                    CloudRemoteObservation {
                                        scope: scope.clone(),
                                        listed: info.clone(),
                                        logical: snapshot.clone(),
                                        local_key: row
                                            .local_relative_path
                                            .as_deref()
                                            .unwrap_or(&row.relative_path)
                                            .to_string(),
                                        local: SyncIndexEntry {
                                            size: local.size,
                                            modified: local.modified,
                                            is_dir: local.is_dir,
                                            uploaded_at: None,
                                        },
                                    },
                                );
                            }
                        }
                    }
                    outcome => {
                        let reason = match outcome {
                            Err(error) => format!(
                                "Remote metadata unavailable: {error}; file kept for a later scan"
                            ),
                            Ok(None) => {
                                "Remote file disappeared during scan; file kept for a later scan"
                                    .to_string()
                            }
                            _ => "Remote file changed type during scan; file kept for a later scan"
                                .to_string(),
                        };
                        inspection_errors.push(format!("{}: {reason}", row.relative_path));
                        quarantined.insert(row.relative_path.clone());
                        self.withhold_update(row, &reason);
                    }
                }
            }
        }

        // Exact-name migrated keys regain trust only from verified, identical
        // copies on both sides of this full scan, never from a one-sided skip.
        if let Some(prior) = index.as_mut() {
            Self::verify_migrated_baseline(
                prior,
                &local_files,
                &remote_files,
                &options,
                config.sync_direction,
            );
        }
        let report = classify_with_summary(local_files, remote_files, &options, index.as_ref());
        let mut comparisons = report.differences;
        let mirrored = report.mirrored;
        self.remember_cloud_receipts(index.as_ref());
        self.apply_cloud_authority(config, &mut comparisons);
        for row in &mut comparisons {
            if quarantined.contains(&row.relative_path) {
                row.name_match_blocked = true;
            }
        }

        // A provider whose successful listing can omit a stored object
        // (`listing_is_authoritative() == false`, ImageKit) cannot authorise
        // deleting a local file by that file's absence: the same rule
        // `sync::remote_listing_delete_guard` applies on the shared path.
        if !provider.listing_is_authoritative()
            && !(config.update_only && config.sync_direction != CompareDirection::Bidirectional)
        {
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
        let mut update_policy = UpdatePolicy {
            window: options.modify_window,
            root_identity: scan_root_identity,
            // A partial or non-authoritative source cannot authorize orphan
            // removal, including the first run with no prior sync index.
            allow_orphan_deletes: (if config.sync_direction == CompareDirection::LocalToRemote {
                local_file_count > 0
            } else {
                remote_file_count > 0
            }) && local_complete
                && remote_complete
                && inspection_errors.is_empty()
                && (config.sync_direction != CompareDirection::RemoteToLocal
                    || provider.listing_is_authoritative()),
        };
        let pending_deletes = self.count_pending_deletes(config, &comparisons, update_policy);
        let destination_files = if config.sync_direction == CompareDirection::LocalToRemote {
            remote_file_count
        } else {
            local_file_count
        };
        let delete_refusal = (config.update_only
            && config.sync_direction != CompareDirection::Bidirectional
            && Self::update_delete_safety_trips(pending_deletes, destination_files))
        .then(|| {
            format!(
                "AeroCloud safety gate: blocked {pending_deletes} deletes out of {destination_files} destination files (at least {MASS_DELETE_FLOOR} and more than half). To proceed with intentional deletes, open the same source and destination in AeroSync, select Mirror with One-way Update, review the plan and confirm the deletions."
            )
        });
        if let Some(reason) = &delete_refusal {
            tracing::warn!("{}", reason);
            update_policy.allow_orphan_deletes = false;
        }
        if Self::delete_safety_trips(
            prior_count,
            local_len == 0,
            remote_len == 0,
            pending_deletes,
            !remote_complete || !local_complete,
        ) {
            update_policy.allow_orphan_deletes = false;
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
                if !(config.update_only && config.sync_direction != CompareDirection::Bidirectional)
                    && (c.status == SyncStatus::LocalOnly || c.status == SyncStatus::RemoteOnly)
                {
                    c.previously_synced = false;
                }
            }
        }

        Ok(SyncPlan {
            inventory_complete: inspection_errors.is_empty()
                && local_complete
                && remote_complete
                && provider.listing_is_authoritative(),
            update_policy,
            delete_refusal,
            inspection_errors,
            remote_observations,
            comparisons,
            mirrored,
            index,
            local_str,
            remote_str,
            gone_on_both,
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
        self.forget_landed();

        let SyncPlan {
            comparisons,
            update_policy,
            delete_refusal,
            inspection_errors,
            ..
        } = self
            .build_sync_plan_with_provider(provider, &config, false)
            .await?;

        let mut result = SyncOperationResult {
            uploaded: 0,
            downloaded: 0,
            deleted: 0,
            skipped: 0,
            skipped_details: Vec::new(),
            conflicts: 0,
            errors: inspection_errors
                .into_iter()
                .chain(delete_refusal)
                .collect(),
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
            match self.resolve_action(&config, comparison, update_policy) {
                SyncAction::AskUser => result.conflicts += 1,
                action => self.record_sync_action(&mut result, &config, comparison, &action),
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
        self.forget_landed();

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
        let plan = self
            .build_sync_plan_with_provider(provider, &config, true)
            .await?;
        let comparisons = &plan.comparisons;

        let total_files = comparisons.len() as u32;
        let mut result = SyncOperationResult {
            uploaded: 0,
            downloaded: 0,
            deleted: 0,
            skipped: 0,
            skipped_details: Vec::new(),
            conflicts: 0,
            errors: plan
                .inspection_errors
                .iter()
                .cloned()
                .chain(plan.delete_refusal.iter().cloned())
                .collect(),
            duration_secs: 0,
            file_details: Vec::new(),
        };

        // Process each comparison
        // P1-7: Throttle status updates to every 100 files or 500ms to reduce lock contention
        let mut last_status_update = std::time::Instant::now();
        let status_interval = std::time::Duration::from_millis(500);
        let mut ahead = bulk::BulkAhead::new(plan.update_policy);
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
                .next_outcome(provider, &config, comparisons, index, &mut ahead)
                .await
            {
                Ok(action) => match action {
                    SyncAction::AskUser => {
                        result.conflicts += 1;
                        self.push_conflict(comparison).await;
                    }
                    _ => self.record_sync_action(&mut result, &config, comparison, &action),
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
        self.save_post_sync_index(&plan, &result, &config);

        // After the save, which a cycle with errors skips: the baseline of a
        // file that kept changing while it uploaded is what makes the next
        // cycle read it as a conflict.
        for path in self.take_unsettled() {
            result.errors.push(Self::unsettled_message(&config, &path));
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
            let _ = app.emit(
                "cloud_sync_complete",
                Self::completion_payload(
                    &config,
                    &result,
                    plan.inventory_complete && result.errors.is_empty(),
                ),
            );
        }

        Ok(result)
    }

    /// Add a conflict left for the user to the list (capped at 10K to prevent
    /// unbounded growth).
    async fn push_conflict(&self, comparison: &FileComparison) {
        let mut conflicts = self.conflicts.write().await;
        if conflicts.len() < 10_000 {
            conflicts.push(FileConflict {
                relative_path: comparison.relative_path.clone(),
                local_modified: comparison.local_info.as_ref().and_then(|i| i.modified),
                remote_modified: comparison.remote_info.as_ref().and_then(|i| i.modified),
                local_size: comparison.local_info.as_ref().map_or(0, |i| i.size),
                remote_size: comparison.remote_info.as_ref().map_or(0, |i| i.size),
                status: comparison.status.clone(),
            });
        }
    }

    /// Scope review inventory to one pair; partial watcher events cannot clear other warnings.
    fn completion_payload(
        config: &CloudConfig,
        result: &SyncOperationResult,
        complete: bool,
    ) -> serde_json::Value {
        let mut value = serde_json::json!(result);
        value["review_scope"] = serde_json::json!([
            config.server_profile_id,
            config.server_profile,
            config.local_folder,
            config.remote_folder
        ])
        .to_string()
        .into();
        value["review_inventory_complete"] = serde_json::json!(complete);
        value
    }

    /// Record successful work or a named safety skip for GUI and CLI consumers.
    fn record_sync_action(
        &self,
        result: &mut SyncOperationResult,
        config: &CloudConfig,
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
                let changed_reason = self.changed_destination_reason(config, comparison);
                let runtime_reason = self
                    .withheld_updates
                    .lock()
                    .ok()
                    .and_then(|rows| rows.get(&comparison.relative_path).cloned());
                let needs_review = UpdatePolicy::untracked_destination(config, comparison)
                    || changed_reason.is_some()
                    || runtime_reason.as_ref().is_some_and(|(_, review)| *review);
                let reason = if let Some((reason, _)) = runtime_reason {
                    reason
                } else if let Some(reason) = changed_reason {
                    reason
                } else if needs_review {
                    "No AeroCloud transfer receipt can be verified for this destination and the current account/root; file kept. Open the same source and destination in AeroSync, select Mirror with One-way Update, review the plan and confirm any intended deletions.".to_string()
                } else {
                    comparison.sync_reason.clone()
                };
                if needs_review {
                    tracing::warn!("AeroCloud: {}: {}", comparison.relative_path, reason);
                }
                result.skipped_details.push(SkippedEntry {
                    path: comparison.relative_path.clone(),
                    reason,
                    is_dir: comparison.is_dir,
                    needs_review,
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
        // No remote boundaries, so nothing can refuse.
        let _ = Self::bound_scan_paths(
            local_root,
            locals,
            remotes,
            crate::sync_core::ScanBoundaries::default(),
        );
    }

    /// [`Self::bound_local_scan_paths`] plus what the remote scan did not
    /// see: every unseen remote path leaves both sides, with everything under
    /// it. `Err` when the remote scan missed a part it cannot name.
    fn bound_scan_paths(
        local_root: &str,
        locals: &mut HashMap<String, FileInfo>,
        remotes: &mut HashMap<String, FileInfo>,
        remote_boundaries: crate::sync_core::ScanBoundaries,
    ) -> Result<(), String> {
        let bound = crate::sync_core::ScanBound::for_sync(
            local_root,
            locals.keys().map(String::as_str),
            remotes.keys().map(String::as_str),
            &crate::sync_core::ScanBoundaries::default(),
            remote_boundaries,
        );
        if let Some(refusal) = bound.refusal() {
            return Err(format!("AeroCloud sync not run: {refusal}"));
        }
        locals.retain(|path, _| !bound.covers(path));
        remotes.retain(|path, _| !bound.covers(path));
        Ok(())
    }

    /// The baseline paths gone from both sides since the last cycle: deleted
    /// here and on another device in between. The compare reads only the
    /// scanned paths, so no comparison names them, and the carry-forward of
    /// [`Self::post_sync_baseline`] kept them for good: a file created later
    /// at one of them read as synced before and deleted on the other side
    /// (a new local file deleted locally, a new remote one on the remote).
    ///
    /// For complete scans only (the caller's check), and only for paths both
    /// scans would list: a path the rules leave out (`.aeroignore`, the
    /// exclude list, an excluded folder), or one behind a local link or an
    /// unreadable local folder, is absent from both by design and keeps its
    /// entry. Forgetting a path never deletes anything: a file the scans
    /// missed without saying so is at worst copied back where it was deleted,
    /// which is the side the delete safety gate errs on too.
    fn gone_on_both(
        config: &CloudConfig,
        local_root: &str,
        locals: &HashMap<String, FileInfo>,
        remotes: &HashMap<String, FileInfo>,
        index: Option<&SyncIndex>,
    ) -> Result<Vec<String>, String> {
        let Some(index) = index else {
            return Ok(Vec::new());
        };
        let scope = ScanScope::new(config)?;
        let candidates: Vec<&str> = index
            .files
            .iter()
            .filter(|(key, entry)| {
                !locals.contains_key(*key)
                    && !remotes.contains_key(index.remote_spellings.get(*key).unwrap_or(*key))
                    && scope.lists(key, entry.is_dir)
            })
            .map(|(key, _)| key.as_str())
            .collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        // What `bound_local_scan_paths` leaves out, for these paths: a local
        // link or unreadable folder above one, or the path itself a link. The
        // scans' own bound probes only the paths the remote lists.
        let bound = crate::sync_core::ScanBound::for_sync(
            local_root,
            std::iter::empty(),
            candidates.iter().copied(),
            &crate::sync_core::ScanBoundaries::default(),
            crate::sync_core::ScanBoundaries::default(),
        );
        Ok(candidates
            .into_iter()
            .filter(|key| !bound.covers(key))
            .map(str::to_string)
            .collect())
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
    ) -> Result<RemoteScan, String> {
        let mut files = HashMap::new();
        let mut complete = true;
        // What the walk did not see, by name, so the plan stays out of it on
        // both sides; a gap it cannot name refuses the cycle.
        let mut boundaries = crate::sync_core::ScanBoundaries::default();
        let base_path = &config.remote_folder;
        // Load .aeroignore from local sync root (applies to remote paths too)
        let aeroignore = crate::sync_ignore::AeroIgnore::load(&config.local_folder);
        let excludes = crate::sync::compile_excludes(&config.exclude_patterns)?;

        // Stack-based recursive scan with depth tracking
        // (base_path, relative_prefix, depth)
        let mut stack: Vec<(String, String, u32)> = vec![(base_path.clone(), String::new(), 0)];
        // Each folder's path is built from its parent and the name the
        // listing gives, so a name such as `../d1`, `d1/../..` or
        // `up/../../elsewhere` leads back to the folder, above it or out of
        // it while reading as a new path every time. A folder is listed only
        // when `WalkScope` allows it (strictly under the sync folder, not the
        // folder listed or above it, not listed already); one it does not
        // follow hides what it holds, so the scan is incomplete.
        let mut scope = crate::sync_core::WalkScope::new(base_path);
        const MAX_DEPTH: u32 = 64;

        while let Some((current_path, relative_prefix, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                tracing::warn!("Remote scan depth limit reached at {}", current_path);
                complete = false;
                boundaries.unseen(&relative_prefix, "depth_limit", Some(true));
                continue;
            }

            // Navigate to directory
            if provider.cd(&current_path).await.is_err() {
                complete = false;
                boundaries.unseen(&relative_prefix, "list_error", Some(true));
                continue;
            }

            // List files using provider
            let entries = match provider.list(".").await {
                Ok(list) => list,
                Err(_) => {
                    complete = false;
                    boundaries.unseen(&relative_prefix, "list_error", Some(true));
                    continue;
                }
            };

            for entry in entries {
                let relative_path = if relative_prefix.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", relative_prefix, entry.name)
                };
                let child_path = format!("{}/{}", current_path, entry.name);
                use crate::sync_core::WalkStep;
                match scope.locate(&current_path, &child_path) {
                    WalkStep::Follow => {}
                    step => {
                        tracing::warn!(
                            "Remote scan: skipping {}: the listing of {} names it, but {}",
                            child_path,
                            current_path,
                            step.explain()
                        );
                        complete = false;
                        boundaries.unseen(&relative_path, step.reason(), Some(entry.is_dir));
                        continue;
                    }
                }

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
                        boundaries.unbounded.get_or_insert("entry_cap");
                        return Ok(RemoteScan {
                            files,
                            complete: false,
                            boundaries,
                        });
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

                    if scope.admit(&current_path, &child_path) == crate::sync_core::WalkStep::Follow
                    {
                        stack.push((child_path, relative_path, depth + 1));
                    } else {
                        tracing::warn!("Skipping already-visited directory: {}", child_path);
                    }
                }
            }
        }

        Ok(RemoteScan {
            files,
            complete,
            boundaries,
        })
    }

    /// Process a single file comparison using any StorageProvider
    /// What an upload the remote now holds leaves for the baseline, from the
    /// remote entry read right after it (a `stat`, or the listing of its
    /// folder after a bulk) and the local state `sent` that went up, unchanged
    /// since. `exact_sizes` is the provider's type when it reports the size of
    /// what it stores exactly.
    fn settle_upload(
        &self,
        relative_path: &str,
        local_path: &Path,
        sent: LocalFileState,
        uploaded: Option<&ProviderRemoteEntry>,
        exact_sizes: Option<ProviderType>,
    ) {
        if let Some(provider_type) = exact_sizes {
            if uploaded
                .and_then(|entry| comparable_remote_size(provider_type, entry))
                .is_some_and(|size| size != sent.len)
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
                    relative_path
                );
                if let Some(modified) = sent.modified {
                    self.note_landed_as(
                        relative_path,
                        Landed {
                            destination_side: CompareDirection::LocalToRemote,
                            remote_size: None,
                            remote_modified: None,
                            size: Some(sent.len),
                            modified,
                        },
                    );
                }
                return;
            }
        }
        let server_time = uploaded
            .and_then(|entry| entry.modified.as_deref())
            .and_then(crate::parse_remote_datetime)
            .and_then(|dt| DateTime::<Utc>::from_timestamp(dt.timestamp(), 0));
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
                relative_path,
                Landed {
                    destination_side: CompareDirection::LocalToRemote,
                    remote_size: uploaded.map(|entry| entry.size),
                    remote_modified: uploaded
                        .and_then(|entry| entry.modified.as_deref())
                        .and_then(crate::parse_remote_datetime),
                    size: Some(sent.len),
                    modified,
                },
            );
        }
    }

    /// Find a real source file using the full scan's exclusions and link rules.
    /// Unreadable or excessively large probes cannot authorize deletion.
    fn local_source_has_syncable_file(config: &CloudConfig) -> Result<bool, String> {
        let scope = ScanScope::new(config)?;
        let mut folders = vec![(config.local_folder.clone(), String::new())];
        let mut inspected = 0;
        while let Some((folder, prefix)) = folders.pop() {
            for entry in std::fs::read_dir(folder).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                inspected += 1;
                if inspected > LOCAL_SOURCE_PROBE_LIMIT {
                    return Err(
                        "local source probe exceeded its safety bound; run a full scan".into(),
                    );
                }
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                let key = if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                };
                let meta = entry.path().symlink_metadata().map_err(|e| e.to_string())?;
                if meta.file_type().is_symlink() {
                    continue;
                }
                if meta.is_dir() && scope.walks(&key) {
                    folders.push((entry.path(), key));
                } else if meta.is_file() && scope.lists(&key, false) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Record a runtime safety skip without promoting it to a successful sync.
    fn withhold_update(&self, comparison: &FileComparison, reason: &str) -> bool {
        if let Ok(mut rows) = self.withheld_updates.lock() {
            rows.insert(
                comparison.relative_path.clone(),
                (reason.to_string(), false),
            );
        }
        false
    }

    /// A changed destination requires a visible manual decision, including runtime-only changes.
    fn withhold_review_update(&self, row: &FileComparison, reason: &str) -> bool {
        if let Ok(mut rows) = self.withheld_updates.lock() {
            rows.insert(row.relative_path.clone(), (reason.to_string(), true));
        }
        false
    }

    /// Re-read an orphan destination against its actual transferred metadata.
    async fn update_remote_destination_is_unchanged<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        config: &CloudConfig,
        row: &FileComparison,
        path: &str,
    ) -> Result<bool, String> {
        let Some(receipt) = self.cloud_receipt_for(config, row) else {
            return Ok(self.withhold_review_update(
                row,
                "No verified AeroCloud receipt; destination kept for review",
            ));
        };
        let current = incremental::remote_state(provider, path).await?;
        let matches = current.as_ref().is_some_and(|entry| {
            !entry.is_dir
                && Self::receipt_matches(
                    config,
                    &receipt,
                    entry.size,
                    entry
                        .modified
                        .as_deref()
                        .and_then(crate::parse_remote_datetime),
                )
        });
        if !matches {
            return Ok(self.withhold_review_update(row, "Destination changed after the scan or cannot be verified; kept for review in AeroSync"));
        }
        Ok(true)
    }

    /// Re-read the destination immediately before a one-way Update transfer.
    /// This narrows the scan/write race; unconditional provider writes cannot
    /// eliminate a writer arriving between this check and the transfer itself.
    async fn recheck_update_transfer<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        config: &CloudConfig,
        comparison: &FileComparison,
        policy: UpdatePolicy,
    ) -> Result<bool, String> {
        if !config.update_only
            || config.sync_direction == CompareDirection::Bidirectional
            || comparison.is_dir
        {
            return Ok(true);
        }
        let local_rel = comparison
            .local_relative_path
            .as_deref()
            .unwrap_or(&comparison.relative_path);
        let remote_rel = comparison
            .remote_relative_path
            .as_deref()
            .unwrap_or(&comparison.relative_path);
        validate_relative_path(local_rel)?;
        validate_relative_path(remote_rel)?;
        let upload = config.sync_direction == CompareDirection::LocalToRemote;
        if upload && comparison.remote_info.is_none() && !provider.listing_is_authoritative() {
            return Err("Update cannot prove this destination is absent: its listing is not authoritative; review it in AeroSync".into());
        }
        let remote_path = format!(
            "{}/{}",
            config.remote_folder.trim_end_matches('/'),
            remote_rel
        );
        let remote = incremental::remote_state(provider, &remote_path).await?;
        if policy.root_identity.is_none()
            || LocalRootIdentity::read(&config.local_folder) != policy.root_identity
        {
            return Err("Update local root changed or is unavailable; compare again".into());
        }
        incremental::local_source_is_absent(&config.local_folder, local_rel)?;
        let local_path = config.local_folder.join(local_rel);
        let local = match std::fs::symlink_metadata(&local_path) {
            Ok(meta) if meta.is_file() => Some(meta),
            Ok(_) => return Err("Update source/destination changed type; compare again".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("Update cannot inspect local file: {e}")),
        };
        if remote.as_ref().is_some_and(|entry| entry.is_dir) {
            return Err("Update source/destination changed type; compare again".into());
        }
        let local_time = local
            .as_ref()
            .and_then(|meta| meta.modified().ok())
            .map(DateTime::<Utc>::from);
        let remote_time = remote
            .as_ref()
            .and_then(|entry| entry.modified.as_deref())
            .and_then(crate::parse_remote_datetime);
        let (source, destination, destination_present) = if upload {
            if local.is_none() {
                return Err("Update source disappeared; compare again".into());
            }
            (local_time, remote_time, remote.is_some())
        } else {
            let Some(current) = remote.as_ref() else {
                return Err("Update source disappeared; compare again".into());
            };
            if comparison.remote_info.as_ref().is_none_or(|scanned| {
                scanned.modified != remote_time || scanned.size != current.size
            }) {
                return Err("Update source changed since the scan; compare again".into());
            }
            (remote_time, local_time, local.is_some())
        };
        if !destination_present {
            return Ok(true);
        }
        match policy.window.order(
            source.map(|time| time.timestamp()),
            destination.map(|time| time.timestamp()),
        ) {
            Some(std::cmp::Ordering::Less) => Ok(self.withhold_update(
                comparison,
                "Destination became newer after the scan; kept for the next comparison",
            )),
            Some(_) => Ok(true),
            None => Err("Update cannot compare current timestamps; compare again".into()),
        }
    }

    /// Execute one comparison, rechecking Update transfers and destructive
    /// source absence after any earlier asynchronous work in the cycle.
    async fn process_comparison_with_provider<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        config: &CloudConfig,
        comparison: &FileComparison,
        policy: UpdatePolicy,
    ) -> Result<SyncAction, String> {
        // Validate relative_path against traversal attacks (CF-004)
        validate_relative_path(&comparison.relative_path)?;

        let local_rel = comparison
            .local_relative_path
            .as_deref()
            .unwrap_or(&comparison.relative_path);
        let remote_rel = comparison
            .remote_relative_path
            .as_deref()
            .unwrap_or(&comparison.relative_path);
        validate_relative_path(local_rel)?;
        validate_relative_path(remote_rel)?;

        // Resolve via the shared decision so the executed action and the
        // recorded post-sync baseline are always derived the same way.
        let action = self.resolve_action(config, comparison, policy);
        // Whether the transfer left the local file as the baseline may
        // record it (false for a file edited while it was uploaded).
        let mut landed = true;

        // Execute action using provider methods
        match &action {
            SyncAction::Upload => {
                let remote_path = format!(
                    "{}/{}",
                    config.remote_folder.trim_end_matches('/'),
                    remote_rel
                );

                if comparison.is_dir {
                    // Create remote directory
                    if let Err(e) = provider.mkdir(&remote_path).await {
                        // Directory might already exist, log but don't fail
                        tracing::debug!("mkdir {} (may exist): {}", remote_path, e);
                    }
                } else if let Some(local_info) = &comparison.local_info {
                    // Ensure parent directory exists on remote (check first to avoid duplicates)
                    if let Some(parent) = std::path::Path::new(remote_rel).parent() {
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
                        if !self
                            .recheck_update_transfer(provider, config, comparison, policy)
                            .await?
                        {
                            return Ok(SyncAction::Skip);
                        }
                        tracing::info!(
                            "AeroCloud: uploading local '{}' ({} bytes) to remote '{}'",
                            local_info.path,
                            sent.map_or(local_info.size, |state| state.len),
                            remote_path
                        );
                        self.revoke_cloud_receipt(comparison);
                        provider
                            .upload(&local_info.path, &remote_path, None)
                            .await
                            .map_err(|e| format!("Upload failed: {}", e))?;
                        self.note_upload_ended(&comparison.relative_path, Utc::now());

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
                        let Some(sent) = sent.filter(|sent| {
                            Self::local_file_state(local_path).as_ref() == Some(sent)
                        }) else {
                            if sends < UPLOAD_SENDS_WHILE_EDITED
                                && !(config.update_only
                                    && config.sync_direction != CompareDirection::Bidirectional)
                            {
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
                                    destination_side: CompareDirection::LocalToRemote,
                                    remote_size: None,
                                    remote_modified: None,
                                    size: None,
                                    modified: UNSETTLED_BASELINE,
                                },
                            );
                            if let Ok(mut unsettled) = self.unsettled.lock() {
                                unsettled.push(comparison.relative_path.clone());
                            }
                            break;
                        };
                        self.settle_upload(
                            &comparison.relative_path,
                            local_path,
                            sent,
                            uploaded.as_ref().ok(),
                            provider
                                .reports_exact_size()
                                .then(|| provider.provider_type()),
                        );
                        break;
                    }
                }
            }
            SyncAction::Download => {
                let local_path = config.local_folder.join(local_rel);

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
                    if !self
                        .recheck_update_transfer(provider, config, comparison, policy)
                        .await?
                    {
                        return Ok(SyncAction::Skip);
                    }
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

                    self.revoke_cloud_receipt(comparison);
                    provider
                        .download_checked(
                            &remote_info.path,
                            &local_path.to_string_lossy(),
                            (remote_info.size > 0).then_some(remote_info.size),
                            None,
                        )
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
                let local_path = config.local_folder.join(local_rel);
                // Rename local file with Dropbox-style conflict suffix to preserve both versions
                let mut set_aside = None;
                if local_path.exists() {
                    let conflict_path = local_path.with_file_name(conflict_rename(&local_path));
                    std::fs::rename(&local_path, &conflict_path).map_err(|e| {
                        format!("Failed to preserve local copy before download: {}", e)
                    })?;
                    set_aside = Some(conflict_path);
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
                    self.revoke_cloud_receipt(comparison);
                    let downloaded = provider
                        .download_checked(
                            &remote_info.path,
                            &local_path.to_string_lossy(),
                            (remote_info.size > 0).then_some(remote_info.size),
                            None,
                        )
                        .await;
                    if let Err(e) = downloaded {
                        // The path must not read as a local deletion next cycle,
                        // which a two-way sync would carry to the remote copy. A
                        // file that appeared there meanwhile is not replaced.
                        let Some(copy) = &set_aside else {
                            return Err(format!("KeepBoth download failed: {}", e));
                        };
                        let restored = if local_path.exists() {
                            Err("the path holds a file again".to_string())
                        } else {
                            std::fs::rename(copy, &local_path).map_err(|why| why.to_string())
                        };
                        return Err(match restored {
                            Ok(()) => format!("KeepBoth download failed: {}", e),
                            Err(why) => format!(
                                "KeepBoth download failed: {}; the local copy stays at {} ({})",
                                e,
                                copy.display(),
                                why
                            ),
                        });
                    }
                    // The original path now holds the remote copy, as after a
                    // Download: it keeps the remote time, and the baseline
                    // records it (see `baseline_entry_for`).
                    if let Some(ref mtime) = remote_info.modified {
                        crate::preserve_remote_mtime_dt(&local_path, Some(*mtime));
                    }
                }
            }
            SyncAction::DeleteRemote if !comparison.is_dir => {
                let remote_path = format!(
                    "{}/{}",
                    config.remote_folder.trim_end_matches('/'),
                    remote_rel
                );
                if config.update_only
                    && config.sync_direction == CompareDirection::LocalToRemote
                    && !self
                        .update_remote_destination_is_unchanged(
                            provider,
                            config,
                            comparison,
                            &remote_path,
                        )
                        .await?
                {
                    return Ok(SyncAction::Skip);
                }
                // Recheck after the asynchronous destination stat, immediately before deletion.
                if config.update_only && config.sync_direction == CompareDirection::LocalToRemote {
                    if policy.root_identity.is_none()
                        || LocalRootIdentity::read(&config.local_folder) != policy.root_identity
                    {
                        return Err(
                            "AeroCloud local source root changed or is unavailable; delete refused"
                                .into(),
                        );
                    }
                    if !Self::local_source_has_syncable_file(config)? {
                        return Err(
                            "AeroCloud local source has no syncable files; delete refused".into(),
                        );
                    }
                    let source_key = comparison.baseline_key.as_deref().unwrap_or(local_rel);
                    validate_relative_path(source_key)?;
                    if !incremental::local_source_is_absent(&config.local_folder, source_key)?
                        || (source_key != local_rel
                            && !incremental::local_source_is_absent(
                                &config.local_folder,
                                local_rel,
                            )?)
                    {
                        self.withhold_update(
                            comparison,
                            "Source was recreated after the scan; destination kept",
                        );
                        return Ok(SyncAction::Skip);
                    }
                }
                self.revoke_cloud_receipt(comparison);
                provider
                    .delete(&remote_path)
                    .await
                    .map_err(|e| format!("Delete propagation (remote) failed: {}", e))?;
                tracing::info!("AeroCloud: delete propagated to remote '{}'", remote_path);
            }
            SyncAction::DeleteLocal if !comparison.is_dir => {
                if config.update_only && config.sync_direction == CompareDirection::RemoteToLocal {
                    let remote_path = format!(
                        "{}/{}",
                        config.remote_folder.trim_end_matches('/'),
                        remote_rel
                    );
                    if incremental::remote_state(provider, &remote_path)
                        .await?
                        .is_some()
                    {
                        self.withhold_update(
                            comparison,
                            "Source was recreated after the scan; destination kept",
                        );
                        return Ok(SyncAction::Skip);
                    }
                }
                if config.update_only && config.sync_direction == CompareDirection::RemoteToLocal {
                    if policy.root_identity.is_none()
                        || LocalRootIdentity::read(&config.local_folder) != policy.root_identity
                    {
                        return Err(
                            "Update local root changed or is unavailable; delete refused".into(),
                        );
                    }
                    incremental::local_source_is_absent(&config.local_folder, local_rel)?;
                    let current = std::fs::symlink_metadata(config.local_folder.join(local_rel))
                        .map_err(|e| e.to_string())?;
                    let receipt = self.cloud_receipt_for(config, comparison);
                    if !current.is_file()
                        || !receipt.as_ref().is_some_and(|receipt| {
                            Self::receipt_matches(
                                config,
                                receipt,
                                current.len(),
                                current.modified().ok().map(DateTime::<Utc>::from),
                            )
                        })
                    {
                        self.withhold_review_update(comparison, "Destination changed after the scan or cannot be verified; kept for review in AeroSync");
                        return Ok(SyncAction::Skip);
                    }
                }
                let local_path = config.local_folder.join(local_rel);
                if local_path.exists() {
                    self.revoke_cloud_receipt(comparison);
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
            && matches!(
                action,
                SyncAction::Upload | SyncAction::Download | SyncAction::KeepBoth
            )
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
/// The overlay stack is composed by [`open_aerocloud_connection`].
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
    let mut config = config;
    let opened = open_aerocloud_connection(&mut config, store, !dry_run).await?;
    if !dry_run {
        // "Sync now" is a full scan: the background scheduler counts it, so
        // a provider's floor between two scans spans it too. It is recorded
        // under the key of the folder this connect opened, which a `~/...`
        // folder placed (and its ledger state moved) here changed.
        crate::cloud_scan_schedule::ledger().record_full_scan(
            &crate::cloud_scan_schedule::pair_key(&config),
            std::time::Instant::now(),
        );
    }
    sync_opened_config(config, opened, app, dry_run).await
}

async fn sync_opened_config(
    config: CloudConfig,
    opened: ConnectedCloudProvider,
    app: Option<AppHandle>,
    dry_run: bool,
) -> Result<SyncOperationResult, String> {
    let mut provider = opened.provider;
    // A manual "Sync now" connects here, not through the background cycle, so
    // it is the one that must tell the scheduler what the provider said about
    // scanning. Without it the floor (Tigris, Fastly: 30 minutes) is not known
    // when the first scan of a pair is a manual one, and a short configured
    // interval could scan again at once.
    remember_scan_traits(&config, &*provider);

    // NOTE: explicit cd/mkdir that used to sit between wrap and service here
    // (and in the old inner) has been removed: build_sync_plan_with_provider
    // (used by both preview and perform) centralizes ensure_remote.

    let mut svc = CloudService::new();
    svc.connection_identity = Some(opened.identity);
    if let Some(handle) = app {
        svc.set_app_handle(handle);
    }
    svc.init(config.clone()).await;

    let sync_result = if dry_run {
        svc.preview_full_sync_with_provider(&mut *provider).await
    } else {
        svc.perform_full_sync_with_provider(&mut *provider).await
    };

    let _ = provider.disconnect().await;

    sync_result
}

/// Connect the provider of an AeroCloud config and compose its overlay
/// stack: crypt is resolved from the saved profile first, then AeroCompress
/// wraps it as the outer layer when `config.compress_enabled` is true. Every
/// AeroCloud cycle, full or incremental, reaches the remote through this
/// stack, so names an overlay rewrites are rewritten the same way on both.
#[cfg(test)]
pub(crate) async fn open_aerocloud_provider(
    config: &CloudConfig,
    store: Option<&credential_store::CredentialStore>,
) -> Result<Box<dyn StorageProvider>, String> {
    let mut config = config.clone();
    Ok(open_aerocloud_connection(&mut config, store, true)
        .await?
        .provider)
}

pub(crate) struct ConnectedCloudProvider {
    provider: Box<dyn StorageProvider>,
    identity: Option<String>,
}

/// A profile id as `select_aerocloud_profile` reads it: blank means none.
fn pinned_profile_id(id: Option<&str>) -> Option<&str> {
    id.map(str::trim).filter(|id| !id.is_empty())
}

/// The saved profiles a connection's references reach, as
/// `select_aerocloud_profile` resolves them: `connected` is the profile the
/// connection used, `by_name` the one a reference carrying only the config's
/// profile name selects now. Names are not unique, so the two differ when the
/// config is pinned to a homonym that is not the first of its name.
/// `connected` is `None` when no saved profile matched and the connection
/// used the config's own credentials (a legacy config): `by_reference` then
/// says whether the config's reference still selects no profile.
#[derive(Clone)]
pub(crate) struct ProfileResolution {
    pub(crate) connected: Option<String>,
    pub(crate) by_name: Option<String>,
    /// The profile the running config's own reference (its id, else its
    /// name) selects now.
    pub(crate) by_reference: Option<String>,
    /// False without an unlocked vault: no name was resolved, so `by_name`
    /// and `by_reference` say nothing.
    pub(crate) names_read: bool,
}

impl ProfileResolution {
    /// `connected` comes from the connection, which carries the id of the one
    /// profile read that chose it: re-reading the list here could name
    /// another profile if it changed while connecting. Without an unlocked
    /// vault no name resolves, so only an entry pinned to the connected id
    /// can be placed.
    pub(crate) fn for_connection(
        store: Option<&credential_store::CredentialStore>,
        config: &CloudConfig,
        connected: Option<String>,
    ) -> Result<Self, String> {
        let Some(store) = store else {
            return Ok(Self {
                connected,
                by_name: None,
                by_reference: None,
                names_read: false,
            });
        };
        let profiles = crate::user_partitions::mcp_list_active_server_profiles(store)?;
        let id_of = |profile: Option<&serde_json::Value>| {
            profile
                .and_then(|p| p.get("id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        Ok(Self {
            connected,
            by_name: id_of(crate::crypt_overlay_provider::select_aerocloud_profile(
                &profiles,
                None,
                &config.server_profile,
            )),
            by_reference: id_of(crate::crypt_overlay_provider::select_aerocloud_profile(
                &profiles,
                config.server_profile_id.as_deref(),
                &config.server_profile,
            )),
            names_read: true,
        })
    }
}

/// Whether a stored profile reference reaches the profile the running config
/// connected to: a reference pinned by id when it is that id, a reference by
/// name when it is the running config's name and that name selects the
/// connected profile. A homonymous profile's login folder never lands in an
/// entry that resolves elsewhere; that entry's own connect places it.
///
/// A legacy connection, made with the config's own credentials because no
/// saved profile matched, reaches the entries that carry the running config's
/// own reference while that reference still selects no saved profile: they
/// connect with the same credentials. Without the names read nothing is known
/// of that, and nothing is placed.
fn same_server_profile(
    stored_id: Option<&str>,
    stored_name: &str,
    config: &CloudConfig,
    resolution: &ProfileResolution,
) -> bool {
    let Some(connected) = resolution.connected.as_deref() else {
        return resolution.names_read
            && resolution.by_reference.is_none()
            && pinned_profile_id(stored_id)
                == pinned_profile_id(config.server_profile_id.as_deref())
            && stored_name == config.server_profile;
    };
    match pinned_profile_id(stored_id) {
        Some(stored) => stored == connected,
        None => {
            stored_name == config.server_profile && resolution.by_name.as_deref() == Some(connected)
        }
    }
}

/// Where a `~/...` remote folder lands under the login folder on the
/// connected provider, written nowhere: a preview syncs against it without
/// changing the saved target. On a protocol with a login folder (SFTP, SCP,
/// FTP, FTPS) a `/~/...` value names a literal `~` directory written by
/// mistake: it is refused with the correction, never synced. Elsewhere `~`
/// has no meaning and the folder is used as written. `None` when there is
/// nothing to place.
pub(crate) async fn place_login_relative_remote_folder(
    provider: &mut dyn StorageProvider,
    config: &CloudConfig,
) -> Result<Option<String>, String> {
    // The connected provider decides, not the config's protocol field, which a
    // legacy config may leave empty.
    if !matches!(
        provider.provider_type(),
        ProviderType::Sftp | ProviderType::Scp | ProviderType::Ftp | ProviderType::Ftps
    ) {
        return Ok(None);
    }
    let remote = config.remote_folder.as_str();
    if remote == "/~" || remote.starts_with("/~/") {
        return Err(format!(
            "the remote folder '{remote}' names a literal '~' directory: write it as \
             '{}' or an absolute path",
            remote.trim_start_matches('/')
        ));
    }
    if !crate::crypt_overlay_provider::is_login_relative_scope(remote) {
        return Ok(None);
    }
    crate::crypt_overlay_provider::resolve_home_scope(provider, remote)
        .await?
        .ok_or_else(|| format!("the remote folder '{remote}' could not be placed"))
        .map(Some)
}

/// A test's edit between the placement's first read and its locked writes,
/// as another window saving meanwhile would make. Tests that set it run
/// under the scratch-config lock, one at a time.
#[cfg(test)]
static BETWEEN_PLACEMENT_READ_AND_WRITE: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>> =
    std::sync::Mutex::new(None);

/// A test's edit after the caller resolved the profile names and before the
/// placement locks the profile list, as a rename in another window would.
#[cfg(test)]
static BEFORE_PLACEMENT_LOCK: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>> =
    std::sync::Mutex::new(None);

/// Place a `~/...` remote folder under the login folder on the connected
/// provider and persist the absolute value, so the index identity is born on
/// the folder the session opens and never on the `~` form. Refusals as
/// [`place_login_relative_remote_folder`]. Returns the placed absolute path,
/// `None` when the remote folder was already absolute.
///
/// With `store`, the profile names are resolved again while the saved
/// profile list is locked ([`crate::user_partitions::ProfileListLock`], which
/// every writer of the list takes, a rename, a delete, an import or a save
/// alike), and stay locked until the placement is written: a name that moves
/// to another profile meanwhile never carries this login folder there.
/// Under the same lock the saved settings must still name `endpoint`, the
/// endpoint the connection was built for
/// ([`cloud_provider_factory::OpenedCloudProvider::endpoint`]): a profile
/// edited to another host, port, user or starting folder since the connect
/// keeps its entries as they were, and so does a connection whose endpoint
/// is unknown. The session still syncs the folder it placed.
/// Without a store (tests, a locked vault) `resolution` is used as given.
pub(crate) async fn anchor_login_relative_remote_folder(
    provider: &mut dyn StorageProvider,
    config: &CloudConfig,
    resolution: &ProfileResolution,
    store: Option<&credential_store::CredentialStore>,
    endpoint: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(placed) = place_login_relative_remote_folder(provider, config).await? else {
        return Ok(None);
    };
    #[cfg(test)]
    if let Some(edit) = BEFORE_PLACEMENT_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        edit();
    }
    // The lock before the config and pairs write locks, which no holder of
    // those takes after them; nothing below awaits while it is held.
    let (_profiles_locked, resolution) = match store {
        Some(store) => (
            Some(crate::user_partitions::ProfileListLock::acquire_for_path(
                &crate::user_partitions::cli_db_path()?,
            )?),
            ProfileResolution::for_connection(Some(store), config, resolution.connected.clone())?,
        ),
        None => (None, resolution.clone()),
    };
    let resolution = &resolution;
    // Each entry, read now, must still connect to the endpoint the session
    // reached: its own protocol and connection parameters, and the saved
    // profile it selects as it is now. An entry that cannot say (no
    // endpoint from the connect, settings that no longer read) is left as it
    // was.
    let reaches_the_endpoint = |entry: &CloudConfig| {
        let Some(store) = store else {
            return true;
        };
        let reaches = endpoint.is_some()
            && matches!(
                cloud_provider_factory::current_endpoint_fingerprint(store, entry),
                Ok(Some(now)) if Some(now.as_str()) == endpoint
            );
        if !reaches {
            tracing::warn!(
                "AeroCloud: the settings of '{}' changed since this connect: its remote \
                 folder '{}' is placed for this run only",
                entry.server_profile,
                entry.remote_folder
            );
        }
        reaches
    };
    let unresolved = config.remote_folder.clone();
    // Persist on the raw provider's answer, before any overlay wraps it: the
    // `~` form only exists to be placed once. Each file is written only when
    // it holds an entry to place: a strict read decides, so a file that cannot
    // be read is an error and is never rewritten, and a file with nothing to
    // place keeps its bytes. The mutation re-checks under the write lock.
    let config_unplaced = |stored: &CloudConfig| {
        stored.remote_folder == unresolved
            && stored.local_folder == config.local_folder
            && same_server_profile(
                stored.server_profile_id.as_deref(),
                &stored.server_profile,
                config,
                resolution,
            )
            && reaches_the_endpoint(stored)
    };
    // Profile names are not unique: the local folder is part of the pair's
    // identity (`pair_key`), so an homonymous pair on another machine keeps
    // its own `~` form until its own connect places it.
    // A pair names its profile without an id, so it resolves by name: a
    // connection pinned by id may have reached another homonymous profile.
    let pair_unplaced = |pair: &cloud_pairs::CloudPathPair| {
        pair.remote_path == unresolved
            && same_server_profile(None, &pair.server_profile, config, resolution)
            && pair.local_path == config.local_folder
            && reaches_the_endpoint(&pair.to_cloud_config())
    };
    // Both files are read before either is written, so an unreadable one
    // leaves the other untouched too.
    // An entry on the same `~` key that this connect does not place (it
    // reaches another profile or endpoint) keeps syncing under that key.
    let keeps_the_key = |remote: &str, local: &std::path::Path| {
        remote == unresolved && local == config.local_folder.as_path()
    };
    let stored_config = cloud_config::load_cloud_config_strict()?;
    let stored_pairs = cloud_pairs::load_cloud_pairs_config_strict()?;
    let place_config = config_unplaced(&stored_config);
    let place_pairs = stored_pairs.pairs.iter().any(pair_unplaced);
    let mut key_still_used =
        !place_config && keeps_the_key(&stored_config.remote_folder, &stored_config.local_folder);
    if !place_pairs {
        key_still_used |= stored_pairs
            .pairs
            .iter()
            .any(|pair| keeps_the_key(&pair.remote_path, &pair.local_path));
    }
    #[cfg(test)]
    if let Some(edit) = BETWEEN_PLACEMENT_READ_AND_WRITE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        edit();
    }
    let mut persisted = false;
    if place_config {
        persisted |= cloud_config::with_cloud_config_mut(|stored| {
            let unplaced = config_unplaced(stored);
            if unplaced {
                stored.remote_folder = placed.clone();
            } else {
                key_still_used |= keeps_the_key(&stored.remote_folder, &stored.local_folder);
            }
            Ok(unplaced)
        })?;
    }
    if place_pairs {
        persisted |= cloud_pairs::with_cloud_pairs_mut(|pairs| {
            let mut moved = false;
            for pair in pairs.pairs.iter_mut() {
                if pair_unplaced(pair) {
                    pair.remote_path = placed.clone();
                    moved = true;
                } else {
                    key_still_used |= keeps_the_key(&pair.remote_path, &pair.local_path);
                }
            }
            Ok(moved)
        })?;
    }
    // The saved entries now carry the absolute folder, so the scheduler
    // reads them under its key: what the scan ledger held under the `~` key
    // (a request, a floor) moves with them. Every caller that places, the
    // manual sync, the background cycles and the folder picker, goes
    // through here. When an entry left unplaced still syncs under the `~`
    // key (a homonymous pair that reaches another profile), the state is
    // copied, not moved: both keys keep a request until each one's scan
    // answers it.
    if persisted {
        let placed_config = CloudConfig {
            remote_folder: placed.clone(),
            ..config.clone()
        };
        let from = crate::cloud_scan_schedule::pair_key(config);
        let to = crate::cloud_scan_schedule::pair_key(&placed_config);
        let mut ledger = crate::cloud_scan_schedule::ledger();
        if key_still_used {
            ledger.copied(&from, &to);
        } else {
            ledger.moved(&from, &to);
        }
    }
    Ok(Some(placed))
}

/// Preserve the identity of the exact factory connection through the validated overlay stack.
///
/// `config` is taken mutable because a `~/...` remote folder is placed under
/// the login folder here and the absolute value written back into it: every
/// caller then syncs the folder the session actually opened, never the `~`
/// form.
async fn open_aerocloud_connection(
    config: &mut CloudConfig,
    store: Option<&credential_store::CredentialStore>,
    persist_placement: bool,
) -> Result<ConnectedCloudProvider, String> {
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

    let opened = cloud_provider_factory::create_cloud_connection(config)
        .await
        .map_err(|e| format!("failed to connect provider: {e}"))?;
    compose_aerocloud_connection(opened, config, store, persist_placement).await
}

/// The part of [`open_aerocloud_connection`] after the connect: place the
/// remote folder, then wrap the overlays of the profile the connection used.
/// Split at the connect so a test can change the saved profiles in between,
/// as an edit made while connecting would.
async fn compose_aerocloud_connection(
    opened: cloud_provider_factory::OpenedCloudProvider,
    config: &mut CloudConfig,
    store: &credential_store::CredentialStore,
    persist_placement: bool,
) -> Result<ConnectedCloudProvider, String> {
    let mut provider = opened.provider;
    // A `~/...` remote folder is placed under the login folder here, on the
    // raw provider (through the overlay `~` is a ciphertext name), and the
    // absolute value persisted: the cycle below, its index identity and every
    // later connect read the placed folder, never the `~` form.
    // A preview places the folder for its own run only: the saved target
    // changes when a cycle that executes places it.
    let remote_folder = if persist_placement {
        let resolution =
            ProfileResolution::for_connection(Some(store), config, opened.profile_id.clone())?;
        anchor_login_relative_remote_folder(
            &mut *provider,
            config,
            &resolution,
            Some(store),
            opened.endpoint.as_deref(),
        )
        .await?
    } else {
        place_login_relative_remote_folder(&mut *provider, config).await?
    };
    if let Some(placed) = remote_folder {
        config.remote_folder = placed;
    }
    if cloud_provider_factory::current_overlay_binding(config, store)? != opened.overlay_binding {
        let _ = provider.disconnect().await;
        return Err("Profile encryption binding changed while connecting; retry the cycle".into());
    }

    // The overlay comes from the profile the connection used, never from a
    // second resolution of the name, which could pick a homonym.
    provider = crypt_overlay_provider::build_aerocloud_overlay_stack(
        provider,
        opened
            .profile_id
            .as_deref()
            .or(config.server_profile_id.as_deref()),
        &config.server_profile,
        store,
    )
    .await
    .map_err(|e| format!("overlay stack refused the sync: {e}"))?;
    // Mixed mode passes writes outside the overlay anchor through as
    // plaintext: a sync target that resolved elsewhere (a persisted folder
    // from another login home, a hand-set absolute path) must be refused
    // here, before any transfer, not uploaded cleartext.
    if let Err(e) = ensure_remote_folder_within_overlay(&mut *provider, &config.remote_folder) {
        let _ = provider.disconnect().await;
        return Err(e);
    }
    if config.compress_enabled {
        validate_compress_level(config.compress_level)?;
        provider = Box::new(CompressOverlayProvider::new(
            provider,
            config.compress_level,
        ));
    }
    if cloud_provider_factory::current_overlay_binding(config, store)? != opened.overlay_binding {
        let _ = provider.disconnect().await;
        return Err(
            "Profile encryption binding changed during overlay composition; retry the cycle".into(),
        );
    }
    Ok(ConnectedCloudProvider {
        provider,
        identity: opened.identity,
    })
}

/// Refuse a sync whose remote folder sits outside the resolved crypt overlay
/// anchor. The overlay's mixed mode encrypts writes strictly below the scope
/// and passes everything else through as plaintext, so an out-of-scope target
/// (a folder persisted under a previous login home, or a hand-set absolute
/// path) would upload cleartext next to the vault it was meant to protect.
/// The containment test mirrors [`encode_plain_target`]'s: the anchor itself
/// and descendants under `"{scope}/"` are in scope, anything else is not. A
/// provider without a crypt overlay, a whole-remote scope (`""`) and a
/// relative target (encrypted in full) are all unrestricted.
fn ensure_remote_folder_within_overlay(
    provider: &mut dyn StorageProvider,
    remote_folder: &str,
) -> Result<(), String> {
    let Some(overlay) = provider
        .as_any_mut()
        .downcast_mut::<crypt_overlay_provider::CryptOverlayProvider>()
    else {
        return Ok(());
    };
    let scope = overlay.scope();
    if scope.is_empty() || !remote_folder.starts_with('/') {
        return Ok(());
    }
    let target = crypt_overlay_provider::comparison_path(remote_folder);
    if target == scope || target.strip_prefix(&format!("{scope}/")).is_some() {
        return Ok(());
    }
    Err(format!(
        "AeroCloud remote folder '{remote_folder}' is outside the profile's encrypted \
         overlay scope '{scope}'; refusing to sync plaintext outside the anchor"
    ))
}

/// What a connected provider said about how a pair may be scanned, for the
/// background scheduler to remember between cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderScanTraits {
    /// [`StorageProvider::min_full_scan_interval`] of the composed stack.
    pub min_full_scan_interval: Option<Duration>,
    /// [`StorageProvider::reports_exact_size`] of the composed stack: the
    /// incremental pass reads local sizes against the baseline only when it
    /// is true.
    pub exact_sizes: bool,
}

/// Tell the shared scan ledger what a connected provider said about scanning
/// (its full-scan floor, exact sizes), under the pair's key. The background
/// cycle reports the same traits through its report; a manual "Sync now"
/// connects on its own path and uses this.
pub(crate) fn remember_scan_traits(config: &CloudConfig, provider: &dyn StorageProvider) {
    crate::cloud_scan_schedule::ledger().learn(
        &crate::cloud_scan_schedule::pair_key(config),
        ProviderScanTraits::of(provider),
    );
}

impl ProviderScanTraits {
    pub(crate) fn of(provider: &dyn StorageProvider) -> Self {
        Self {
            min_full_scan_interval: provider.min_full_scan_interval(),
            exact_sizes: provider.reports_exact_size(),
        }
    }
}

/// One pair's background cycle: what it did, whether it scanned the whole
/// remote folder, and what the provider said about scanning.
#[derive(Debug, Clone)]
pub struct PairCycleReport {
    pub result: SyncOperationResult,
    /// Whether the cycle listed the whole remote folder (a full sync).
    pub ran_full_scan: bool,
    /// `None` when the cycle never connected.
    pub traits: Option<ProviderScanTraits>,
    /// The paths an incremental pass could not sync, for the worker to try
    /// again (a full scan reads them all anyway, and reports none).
    pub failed_paths: Vec<FailedPath>,
    /// The scheduler key of the config the connection opened: the remote
    /// folder may have been placed (`~/...` resolved under the login folder)
    /// during connect, so it can differ from the key the caller computed
    /// before the cycle.
    pub placed_key: String,
}

/// A pair's background cycle that failed. A failure after the connection
/// still carries what the provider said about scanning: the floor it
/// declared holds the next scans of the pair back even while the error
/// lasts, and a failure that dropped it would let a fallback scan run every
/// cycle.
#[derive(Debug, Clone)]
pub struct PairCycleError {
    pub message: String,
    /// `None` when the cycle never connected.
    pub traits: Option<ProviderScanTraits>,
    /// The incremental pass refused a plan that no longer matches the pair
    /// (config or local root changed since the watcher prepared it): the
    /// worker answers with a full scan instead of dropping the changes.
    pub needs_full_scan: bool,
    /// The scheduler key of the config the connection ended with: the remote
    /// folder may have been placed (`~/...` resolved under the login folder)
    /// during connect, so it can differ from the key the caller computed
    /// before the cycle. When the connect itself failed it is the key of the
    /// config as far as the attempt mutated it.
    pub placed_key: String,
}

impl PairCycleError {
    pub(crate) fn before_connecting(message: String, placed_key: String) -> Self {
        Self {
            message,
            traits: None,
            needs_full_scan: false,
            placed_key,
        }
    }
}

/// A path an incremental pass could not sync, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedPath {
    /// Relative to the pair's local folder.
    pub key: String,
    pub error: String,
}

/// A full sync of one pair, for the background worker: [`sync_one_config`]
/// that also reports the provider's scan traits.
pub async fn run_pair_full_cycle(
    config: CloudConfig,
    store: Option<&credential_store::CredentialStore>,
    app: Option<AppHandle>,
) -> Result<PairCycleReport, PairCycleError> {
    let mut config = config;
    let opened = match open_aerocloud_connection(&mut config, store, true).await {
        Ok(opened) => opened,
        Err(message) => {
            return Err(PairCycleError::before_connecting(
                message,
                crate::cloud_scan_schedule::pair_key(&config),
            ));
        }
    };
    let placed_key = crate::cloud_scan_schedule::pair_key(&config);
    let mut provider = opened.provider;
    let traits = ProviderScanTraits::of(&*provider);
    let mut svc = CloudService::new();
    svc.connection_identity = Some(opened.identity);
    if let Some(handle) = app {
        svc.set_app_handle(handle);
    }
    svc.init(config).await;
    let result = svc.perform_full_sync_with_provider(&mut *provider).await;
    let _ = provider.disconnect().await;
    match result {
        Ok(result) => Ok(PairCycleReport {
            result,
            ran_full_scan: true,
            traits: Some(traits),
            failed_paths: Vec::new(),
            placed_key,
        }),
        Err(message) => Err(PairCycleError {
            message,
            traits: Some(traits),
            needs_full_scan: false,
            placed_key,
        }),
    }
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
            local_relative_path: None,
            remote_relative_path: None,
            name_match_blocked: false,
            relative_path: "f.txt".to_string(),
            status,
            local_info: local,
            remote_info: remote,
            is_dir,
            sync_reason: String::new(),
            previously_synced,
            baseline_key: None,
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

    #[test]
    fn identical_cloud_config_cannot_reuse_proof_from_a_different_resolved_connection() {
        let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let config = CloudConfig {
            local_folder: root.path().into(),
            update_only: true,
            sync_direction: CompareDirection::LocalToRemote,
            ..Default::default()
        };
        let row = cmp(SyncStatus::RemoteOnly, None, Some(fi(7, 1000)), true, false);
        let mut svc = CloudService::new();
        svc.connection_identity = Some(Some("resolved-host-account-one".into()));
        svc.cloud_receipts.lock().unwrap().insert(
            "f.txt".into(),
            CloudTransferReceipt {
                scope: svc.effective_receipt_scope(&config).unwrap(),
                remote_key: "f.txt".into(),
                destination_side: CompareDirection::LocalToRemote,
                utc_remote_time: true,
                destination: SyncIndexEntry {
                    size: 7,
                    modified: fi(7, 1000).modified,
                    is_dir: false,
                    uploaded_at: None,
                },
            },
        );
        assert!(svc.cloud_receipt_for(&config, &row).is_some());
        svc.connection_identity = Some(Some("resolved-host-account-two".into()));
        assert!(svc.cloud_receipt_for(&config, &row).is_none());
        svc.connection_identity = Some(None);
        assert!(svc.cloud_receipt_for(&config, &row).is_none());
    }

    /// A FileLu upload receipt saved before the fix still proves the unchanged
    /// remote file; a marked receipt, another gap, another provider or a local
    /// destination does not.
    #[test]
    fn a_filelu_upload_receipt_from_before_the_utc_fix_still_proves_the_remote_file() {
        // FileLu lists the file as "2026-04-17 16:29:19" (US Eastern). The old
        // receipt holds that string read as UTC; the listing now reads it as
        // the real instant, 20:29:19Z. Same file, same receipt.
        let old = crate::parse_remote_datetime("2026-04-17 16:29:19").unwrap();
        let now = crate::parse_remote_datetime("2026-04-17T20:29:19Z").unwrap();
        let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let config_for = |protocol: &str, direction: CompareDirection| CloudConfig {
            local_folder: root.path().into(),
            update_only: true,
            preserve_remote_deletes: false,
            sync_direction: direction,
            protocol_type: protocol.to_string(),
            ..Default::default()
        };
        let reason_with = |config: &CloudConfig,
                           utc_remote_time: bool,
                           receipt_time,
                           listed_time: DateTime<Utc>| {
            let svc = CloudService::new();
            let receipt = CloudTransferReceipt {
                scope: svc.effective_receipt_scope(config).unwrap(),
                remote_key: "f.txt".into(),
                destination_side: config.sync_direction,
                utc_remote_time,
                destination: SyncIndexEntry {
                    size: 7,
                    modified: Some(receipt_time),
                    is_dir: false,
                    uploaded_at: None,
                },
            };
            svc.cloud_receipts
                .lock()
                .unwrap()
                .insert("f.txt".into(), receipt);
            let listed = fi(7, listed_time.timestamp());
            let row = if config.sync_direction == CompareDirection::LocalToRemote {
                cmp(SyncStatus::RemoteOnly, None, Some(listed), true, false)
            } else {
                cmp(SyncStatus::LocalOnly, Some(listed), None, true, false)
            };
            svc.changed_destination_reason(config, &row)
        };
        // A receipt saved before the fix.
        let reason = |config: &CloudConfig, receipt_time, listed_time| {
            reason_with(config, false, receipt_time, listed_time)
        };
        let filelu = config_for("filelu", CompareDirection::LocalToRemote);
        assert_eq!(reason(&filelu, old, now), None);
        // Its time is the old reading, never a real instant, so the listed
        // time is never compared to it as one: a same-size file whose real
        // upload time equals the stored wall-clock value (uploaded 4 hours
        // before the original, then moved in or restored) is a change, and so
        // is a listing that happens to equal the stored value.
        assert!(reason(&filelu, old, old).is_some());
        assert!(reason(&filelu, now, now).is_some());
        // A receipt minted or migrated since the fix holds the real time: a
        // same-size file replaced on the server exactly 4 hours later (the
        // old reading of its time is the receipt's) is a change, never the
        // file AeroCloud wrote.
        let later = now + chrono::Duration::hours(4);
        assert!(reason_with(&filelu, true, now, later).is_some());
        assert_eq!(reason_with(&filelu, true, now, now), None);
        assert!(reason_with(&filelu, true, old, now).is_some());
        // Any other gap is still a change on the server.
        let hour = chrono::Duration::hours(1);
        assert!(reason(&filelu, old + hour, now).is_some());
        assert!(reason(&filelu, old, now + hour).is_some());
        // Only FileLu times moved, and only a remote destination holds them.
        let sftp = config_for("sftp", CompareDirection::LocalToRemote);
        assert!(reason(&sftp, old, now).is_some());
        assert_eq!(reason(&sftp, now, now), None);
        let download = config_for("filelu", CompareDirection::RemoteToLocal);
        assert!(reason(&download, old, now).is_some());
    }

    /// The first saved cycle rewrites an unmarked FileLu receipt of its own
    /// scope with the real time and marks it; every other receipt is left as it
    /// was.
    #[test]
    fn the_first_saved_cycle_rewrites_a_pre_utc_fix_filelu_receipt_with_the_real_time() {
        let old = crate::parse_remote_datetime("2026-04-17 16:29:19").unwrap();
        let now = crate::parse_remote_datetime("2026-04-17T20:29:19Z").unwrap();
        let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let config_for = |protocol: &str| CloudConfig {
            local_folder: root.path().into(),
            update_only: true,
            preserve_remote_deletes: false,
            sync_direction: CompareDirection::LocalToRemote,
            protocol_type: protocol.to_string(),
            ..Default::default()
        };
        let entry = |modified| SyncIndexEntry {
            size: 7,
            modified: Some(modified),
            is_dir: false,
            uploaded_at: None,
        };
        let saved_in = |config: &CloudConfig,
                        foreign_scope: bool,
                        utc_remote_time: bool,
                        receipt_time,
                        listed: FileInfo| {
            let svc = CloudService::new();
            let receipt = CloudTransferReceipt {
                scope: if foreign_scope {
                    "another-connection".into()
                } else {
                    svc.effective_receipt_scope(config).unwrap()
                },
                remote_key: "f.txt".into(),
                destination_side: CompareDirection::LocalToRemote,
                utc_remote_time,
                destination: entry(receipt_time),
            };
            let files: HashMap<String, SyncIndexEntry> = [("f.txt".into(), entry(old))].into();
            let prior = SyncIndex {
                profile_id: None,
                version: SYNC_INDEX_VERSION,
                last_sync: Utc::now(),
                local_path: String::new(),
                remote_path: String::new(),
                files: files.clone(),
                remote_spellings: Default::default(),
                cloud_receipts: [("f.txt".into(), receipt)].into(),
                cloud_remote_metadata: Default::default(),
                unverified_keys: Default::default(),
            };
            let row = cmp(
                SyncStatus::Identical,
                Some(fi(7, 1)),
                Some(listed),
                true,
                false,
            );
            let saved = svc.receipts_after(&[row], config, Some(&prior), &files, None);
            let receipt = &saved["f.txt"];
            (receipt.destination.modified, receipt.utc_remote_time)
        };
        let saved_with = |config: &CloudConfig, utc_remote_time: bool, receipt_time, listed| {
            saved_in(config, false, utc_remote_time, receipt_time, listed)
        };
        // A receipt saved before the fix; the time alone.
        let saved_time = |config: &CloudConfig, receipt_time, listed: FileInfo| {
            saved_with(config, false, receipt_time, listed).0
        };
        let filelu = config_for("filelu");
        let listed = fi(7, now.timestamp());
        assert_eq!(
            saved_with(&filelu, false, old, listed.clone()),
            (Some(now), true)
        );
        // Unmigrated stays unmarked, so the next cycle can still migrate it.
        assert_eq!(
            saved_with(&filelu, false, old, fi(8, now.timestamp())),
            (Some(old), false)
        );
        // A marked receipt is never moved through the old reading, even when
        // the listed file is a same-size replacement 4 hours later.
        let later = fi(7, (now + chrono::Duration::hours(4)).timestamp());
        assert_eq!(saved_with(&filelu, true, now, later), (Some(now), true));
        // A receipt of another connection is never touched by this cycle.
        assert_eq!(
            saved_in(&filelu, true, false, old, listed.clone()),
            (Some(old), false)
        );
        // Already right, another gap, another size, another provider: untouched.
        assert_eq!(saved_time(&filelu, now, listed.clone()), Some(now));
        let hour = chrono::Duration::hours(1);
        assert_eq!(
            saved_time(&filelu, old + hour, listed.clone()),
            Some(old + hour)
        );
        assert_eq!(saved_time(&filelu, old, fi(8, now.timestamp())), Some(old));
        assert_eq!(saved_time(&config_for("sftp"), old, listed), Some(old));
    }

    #[test]
    fn receipt_lookup_filters_a_projected_twin_before_using_explicit_baseline_identity() {
        let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let config = CloudConfig {
            local_folder: root.path().into(),
            update_only: true,
            sync_direction: CompareDirection::LocalToRemote,
            ..Default::default()
        };
        let svc = CloudService::new();
        let scope = svc.effective_receipt_scope(&config).unwrap();
        let nfc = "café.txt";
        let nfd = "cafe\u{301}.txt";
        let proof = |remote: &str| CloudTransferReceipt {
            scope: scope.clone(),
            remote_key: remote.into(),
            destination_side: CompareDirection::LocalToRemote,
            utc_remote_time: true,
            destination: SyncIndexEntry {
                size: 7,
                modified: fi(7, 1000).modified,
                is_dir: false,
                uploaded_at: None,
            },
        };
        svc.cloud_receipts.lock().unwrap().extend([
            (nfc.into(), proof("other-object")),
            (nfd.into(), proof(nfc)),
        ]);
        let mut row = cmp(SyncStatus::RemoteOnly, None, Some(fi(7, 1000)), true, false);
        row.relative_path = nfc.into();
        row.local_relative_path = Some(nfc.into());
        row.remote_relative_path = Some(nfc.into());
        row.baseline_key = Some(nfd.into());
        assert!(svc.cloud_receipt_for(&config, &row).is_some());
        row.baseline_key = None;
        assert!(svc.cloud_receipt_for(&config, &row).is_none());
        row.baseline_key = Some(nfd.into());
        row.local_info = Some(fi(7, 1000));
        assert!(svc.cloud_receipt_for(&config, &row).is_none());
    }

    #[test]
    fn unverified_one_sided_baseline_cannot_gain_delete_authority_on_reload() {
        let config = CloudConfig {
            update_only: true,
            ..cfg(
                CompareDirection::LocalToRemote,
                false,
                ConflictStrategy::PreferLocal,
            )
        };
        let svc = CloudService::new();
        let mut index = SyncIndex::new("/local".into(), "/remote".into());
        index.files.insert(
            "f.txt".into(),
            SyncIndexEntry {
                size: 5,
                modified: fi(5, 1000).modified,
                is_dir: false,
                uploaded_at: None,
            },
        );
        index.unverified_keys.insert("f.txt".into());
        for _ in 0..3 {
            let row = cmp(
                SyncStatus::RemoteOnly,
                None,
                Some(fi(5, 1000)),
                index.files.contains_key("f.txt") && !index.unverified_keys.contains("f.txt"),
                false,
            );
            let policy = UpdatePolicy {
                allow_orphan_deletes: true,
                ..Default::default()
            };
            assert_eq!(svc.resolve_action(&config, &row, policy), SyncAction::Skip);
            index.files = svc.post_sync_baseline(&[row], &config, Some(&index), policy);
            assert!(!index.files.contains_key("f.txt"));
            index.unverified_keys.clear();
            index = serde_json::from_str(&serde_json::to_string(&index).unwrap()).unwrap();
        }
    }

    #[test]
    fn migrated_key_verified_on_both_sides_keeps_history_but_not_cloud_authority() {
        let mut index = SyncIndex::new("/local".into(), "/remote".into());
        index.files.insert(
            "f.txt".into(),
            SyncIndexEntry {
                size: 9,
                modified: fi(9, 900).modified,
                is_dir: false,
                uploaded_at: None,
            },
        );
        index.unverified_keys.insert("f.txt".into());
        let files = HashMap::from([("f.txt".into(), fi(5, 1000))]);
        CloudService::verify_migrated_baseline(
            &mut index,
            &files,
            &files,
            &CompareOptions::default(),
            CompareDirection::LocalToRemote,
        );
        assert!(index.unverified_keys.is_empty());
        assert!(index.cloud_receipts.is_empty());
        assert_eq!(index.files["f.txt"].size, 5);
        let row = cmp(SyncStatus::RemoteOnly, None, Some(fi(5, 1000)), true, false);
        let config = CloudConfig {
            update_only: true,
            ..cfg(
                CompareDirection::LocalToRemote,
                false,
                ConflictStrategy::PreferLocal,
            )
        };
        assert_eq!(
            CloudService::new().resolve_action(
                &config,
                &row,
                UpdatePolicy {
                    allow_orphan_deletes: true,
                    ..Default::default()
                }
            ),
            SyncAction::Skip
        );
    }

    #[test]
    fn mapped_remote_survivor_is_not_forgotten_as_gone_on_both() {
        let root = tempfile::tempdir().unwrap();
        let config = CloudConfig {
            local_folder: root.path().into(),
            ..Default::default()
        };
        let mut index = SyncIndex::new(root.path().display().to_string(), "/remote".into());
        index.files.insert(
            "local-name".into(),
            SyncIndexEntry {
                size: 5,
                modified: None,
                is_dir: false,
                uploaded_at: None,
            },
        );
        index
            .remote_spellings
            .insert("local-name".into(), "remote-name".into());
        let remote = HashMap::from([("remote-name".into(), fi(5, 1000))]);
        assert!(CloudService::gone_on_both(
            &config,
            &root.path().display().to_string(),
            &HashMap::new(),
            &remote,
            Some(&index)
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn one_way_update_keeps_newer_destinations_and_deletes_only_baselined_orphans() {
        let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let svc = CloudService::new();
        for direction in [
            CompareDirection::LocalToRemote,
            CompareDirection::RemoteToLocal,
        ] {
            for strategy in [
                ConflictStrategy::PreferLocal,
                ConflictStrategy::PreferRemote,
                ConflictStrategy::KeepBoth,
            ] {
                svc.cloud_receipts.lock().unwrap().clear();
                let config = CloudConfig {
                    local_folder: root.path().into(),
                    update_only: true,
                    ..cfg(direction, false, strategy)
                };
                let upload = direction == CompareDirection::LocalToRemote;
                // Even a conflict from a prior baseline cannot turn a
                // one-way Update into a write back to its source.
                let row = cmp(
                    SyncStatus::Conflict,
                    Some(fi(3, if upload { 1000 } else { 2000 })),
                    Some(fi(9, if upload { 2000 } else { 1000 })),
                    true,
                    false,
                );
                assert_eq!(
                    svc.resolve_action(&config, &row, UpdatePolicy::default()),
                    SyncAction::Skip
                );
                let mut orphan = cmp(
                    if upload {
                        SyncStatus::RemoteOnly
                    } else {
                        SyncStatus::LocalOnly
                    },
                    (!upload).then(|| fi(7, 1000)),
                    upload.then(|| fi(7, 1000)),
                    false,
                    false,
                );
                let delete = if upload {
                    SyncAction::DeleteRemote
                } else {
                    SyncAction::DeleteLocal
                };
                let allowed = UpdatePolicy {
                    allow_orphan_deletes: true,
                    ..Default::default()
                };
                assert_eq!(
                    svc.resolve_action(&config, &orphan, allowed),
                    SyncAction::Skip
                );
                assert!(UpdatePolicy::untracked_destination(&config, &orphan));
                orphan.previously_synced = true;
                let destination = if upload {
                    orphan.remote_info.as_ref()
                } else {
                    orphan.local_info.as_ref()
                }
                .unwrap();
                svc.cloud_receipts.lock().unwrap().insert(
                    orphan.relative_path.clone(),
                    CloudTransferReceipt {
                        scope: CloudService::receipt_scope(&config),
                        remote_key: orphan.relative_path.clone(),
                        destination_side: direction,
                        utc_remote_time: true,
                        destination: SyncIndexEntry {
                            size: destination.size,
                            modified: destination.modified,
                            is_dir: false,
                            uploaded_at: None,
                        },
                    },
                );
                assert_eq!(
                    svc.resolve_action(
                        &config,
                        &orphan,
                        UpdatePolicy {
                            allow_orphan_deletes: true,
                            ..Default::default()
                        }
                    ),
                    delete
                );
                let safe = UpdatePolicy::default();
                assert_eq!(svc.resolve_action(&config, &orphan, safe), SyncAction::Skip);
                let additive = CloudConfig {
                    preserve_remote_deletes: true,
                    ..config
                };
                assert_eq!(
                    svc.resolve_action(&additive, &orphan, UpdatePolicy::default()),
                    SyncAction::Skip
                );
            }
        }
    }

    #[test]
    fn one_way_update_does_not_guess_dates_or_apply_bidirectional_policy() {
        let svc = CloudService::new();
        let config = CloudConfig {
            update_only: true,
            ..cfg(
                CompareDirection::LocalToRemote,
                false,
                ConflictStrategy::PreferRemote,
            )
        };
        let row = cmp(
            SyncStatus::Conflict,
            Some(fi(3, 1000)),
            Some(fi(9, 1001)),
            true,
            false,
        );
        assert_eq!(
            svc.resolve_action(&config, &row, UpdatePolicy::default()),
            SyncAction::Upload
        );
        let no_dates = UpdatePolicy {
            window: crate::sync_core::mtime::ModifyWindow::LEGACY_FTP,
            ..Default::default()
        };
        assert_eq!(
            svc.resolve_action(&config, &row, no_dates),
            SyncAction::AskUser
        );
        let bidirectional = CloudConfig {
            sync_direction: CompareDirection::Bidirectional,
            ..config
        };
        assert_eq!(
            svc.resolve_action(&bidirectional, &row, UpdatePolicy::default()),
            SyncAction::Download
        );
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
    fn askuser_and_deletes_do_not_advance_baseline() {
        let local = fi(1, 1);
        let remote = fi(2, 2);
        for action in [
            SyncAction::AskUser,
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
            uploaded_at: None,
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
        let files = svc.post_sync_baseline(
            &[synced.clone(), never.clone()],
            &preserve,
            Some(&prior),
            UpdatePolicy::default(),
        );
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
        let files = svc.post_sync_baseline(&[gone], &bidi, Some(&prior), UpdatePolicy::default());
        assert!(
            !files.contains_key("gone.txt"),
            "a delete removes the entry"
        );
        assert!(
            files.contains_key("synced.txt"),
            "untouched entries carry forward"
        );
    }

    /// A file paired with a directory is skipped with both sides present.
    /// Recording that skip makes the local file look previously synced once
    /// the remote directory disappears, and the next cycle deletes it.
    #[test]
    fn a_blocked_name_match_does_not_become_a_synced_baseline() {
        fn named(name: &str, size: u64, is_dir: bool) -> FileInfo {
            let mut info = if is_dir {
                dir_info()
            } else {
                fi(size, 1_700_000_000)
            };
            info.name = name.to_string();
            info.path = name.to_string();
            info
        }
        let svc = CloudService::new();
        let config = cfg(
            CompareDirection::Bidirectional,
            false,
            ConflictStrategy::AskUser,
        );
        let mut local = HashMap::new();
        local.insert("notes".to_string(), named("notes", 8, false));
        let mut remote = HashMap::new();
        remote.insert("notes".to_string(), named("notes", 4096, true));
        remote.insert("other.txt".to_string(), named("other.txt", 1, false));
        let report =
            crate::sync::classify_with_summary(local, remote, &CompareOptions::default(), None);
        let collision = report
            .differences
            .iter()
            .find(|c| c.relative_path == "notes")
            .expect("the file/directory pair is reported");
        assert!(collision.name_match_blocked);
        assert!(collision.local_info.is_some() && collision.remote_info.is_some());

        let mut prior = SyncIndex::new("/l".to_string(), "/r".to_string());
        prior.files.insert(
            "notes".to_string(),
            SyncIndexEntry {
                size: 1,
                modified: None,
                is_dir: false,
                uploaded_at: None,
            },
        );
        prior.files.insert(
            "keep.txt".to_string(),
            SyncIndexEntry {
                size: 4,
                modified: None,
                is_dir: false,
                uploaded_at: None,
            },
        );
        let kept = svc.post_sync_baseline(
            &report.differences,
            &config,
            Some(&prior),
            UpdatePolicy::default(),
        );
        assert_eq!(
            kept.get("notes").map(|e| (e.size, e.is_dir)),
            Some((1, false)),
            "a blocked match must not replace the prior entry"
        );
        assert_eq!(kept.get("keep.txt").map(|e| e.size), Some(4));

        let cycle1 =
            svc.post_sync_baseline(&report.differences, &config, None, UpdatePolicy::default());
        assert!(
            !cycle1.contains_key("notes"),
            "a blocked match must not enter the baseline"
        );
        let mut index = SyncIndex::new("/l".to_string(), "/r".to_string());
        index.files = cycle1;

        // The remote directory is gone. Another remote file remains, so the
        // local file is a one-sided survivor, not an empty remote.
        let mut local2 = HashMap::new();
        local2.insert("notes".to_string(), named("notes", 8, false));
        let mut remote2 = HashMap::new();
        remote2.insert("other.txt".to_string(), named("other.txt", 1, false));
        let report2 = crate::sync::classify_with_summary(
            local2,
            remote2,
            &CompareOptions::default(),
            Some(&index),
        );
        let survivor = report2
            .differences
            .iter()
            .find(|c| c.relative_path == "notes")
            .expect("the local file is still compared");
        assert!(!survivor.previously_synced);
        assert_eq!(
            decide_sync_action(
                &survivor.status,
                &config.sync_direction,
                survivor.previously_synced,
                config.preserve_remote_deletes,
            ),
            SyncAction::Upload
        );
    }

    #[test]
    fn a_new_download_reusing_a_deleted_local_name_keeps_its_baseline() {
        let svc = CloudService::new();
        let config = cfg(
            CompareDirection::Bidirectional,
            false,
            ConflictStrategy::AskUser,
        );
        let local = "cafe\u{301}.txt";
        let remote = "café.txt";
        let mut prior = SyncIndex::new("/l".into(), "/r".into());
        prior.files.insert(
            local.into(),
            SyncIndexEntry {
                size: 8,
                modified: None,
                is_dir: false,
                uploaded_at: None,
            },
        );
        prior.remote_spellings.insert(local.into(), remote.into());
        let mut delete = cmp(
            SyncStatus::RemoteOnly,
            None,
            Some(fi(8, 1_700_000_000)),
            true,
            false,
        );
        delete.relative_path = remote.into();
        delete.baseline_key = Some(local.into());
        let mut download = cmp(
            SyncStatus::RemoteOnly,
            None,
            Some(fi(19, 1_700_000_000)),
            false,
            false,
        );
        download.relative_path = local.into();
        for rows in [
            vec![delete.clone(), download.clone()],
            vec![download.clone(), delete.clone()],
        ] {
            let files =
                svc.post_sync_baseline(&rows, &config, Some(&prior), UpdatePolicy::default());
            assert_eq!(files[local].size, 19);
            assert_eq!(files.len(), 1);
        }
    }

    #[test]
    fn shark_review_exact_unicode_twins_keep_both_baselines() {
        let svc = CloudService::new();
        let config = cfg(
            CompareDirection::Bidirectional,
            false,
            ConflictStrategy::AskUser,
        );
        let remote = HashMap::from([
            ("café.txt".to_string(), fi(8, 1_700_000_000)),
            ("cafe\u{301}.txt".to_string(), fi(19, 1_700_000_000)),
        ]);
        let mut prior = SyncIndex::new("/l".into(), "/r".into());
        prior.files = remote
            .iter()
            .map(|(key, info)| {
                (
                    key.clone(),
                    SyncIndexEntry {
                        size: info.size,
                        modified: info.modified,
                        is_dir: false,
                        uploaded_at: None,
                    },
                )
            })
            .collect();
        let mut local = remote.clone();
        local.insert("café.txt".into(), fi(9, 1_700_000_100));
        let report = crate::sync::classify_with_summary(
            local,
            remote,
            &CompareOptions::default(),
            Some(&prior),
        );
        assert_eq!(report.summary.identical_count, 1);
        let mut rows = report.differences;
        rows.extend(report.mirrored);
        let baseline =
            svc.post_sync_baseline(&rows, &config, Some(&prior), UpdatePolicy::default());
        assert_eq!(
            baseline.len(),
            2,
            "independent exact pairs must not collapse: {:?}",
            baseline.keys()
        );
    }

    /// The legacy remote spelling is what proved the file was synced. The
    /// delete row is keyed by the local spelling. Forgetting only that key
    /// leaves the remote one, so a file created again under the same
    /// canonical name is deleted instead of uploaded.
    #[test]
    fn a_delete_forgets_the_legacy_spelling_that_authorized_it() {
        fn named(name: &str, size: u64) -> FileInfo {
            let mut info = fi(size, 1_700_000_000);
            info.name = name.to_string();
            info.path = name.to_string();
            info
        }
        let svc = CloudService::new();
        let config = cfg(
            CompareDirection::Bidirectional,
            false,
            ConflictStrategy::AskUser,
        );
        let local_name = "cafe\u{301}.txt";
        let mut local = HashMap::new();
        local.insert(local_name.to_string(), named(local_name, 8));
        let mut remote = HashMap::new();
        remote.insert("other.txt".to_string(), named("other.txt", 1));
        let mut prior = SyncIndex::new("/l".to_string(), "/r".to_string());
        prior.files.insert(
            "café.txt".to_string(),
            SyncIndexEntry {
                size: 8,
                modified: None,
                is_dir: false,
                uploaded_at: None,
            },
        );
        let report = crate::sync::classify_with_summary(
            local,
            remote,
            &CompareOptions::default(),
            Some(&prior),
        );
        let row = report
            .differences
            .iter()
            .find(|c| c.relative_path == local_name)
            .expect("the local spelling is compared");
        assert!(row.previously_synced);
        assert_eq!(
            decide_sync_action(
                &row.status,
                &config.sync_direction,
                row.previously_synced,
                config.preserve_remote_deletes,
            ),
            SyncAction::DeleteLocal
        );

        let files = svc.post_sync_baseline(
            &report.differences,
            &config,
            Some(&prior),
            UpdatePolicy::default(),
        );
        assert!(
            !files.contains_key("café.txt") && !files.contains_key(local_name),
            "both spellings of the deleted file leave the baseline: {:?}",
            files.keys().collect::<Vec<_>>()
        );
        assert!(files.contains_key("other.txt"));

        let mut index = SyncIndex::new("/l".to_string(), "/r".to_string());
        index.files = files;
        let mut recreated = HashMap::new();
        recreated.insert(local_name.to_string(), named(local_name, 9));
        let mut remote2 = HashMap::new();
        remote2.insert("other.txt".to_string(), named("other.txt", 1));
        let again = crate::sync::classify_with_summary(
            recreated,
            remote2,
            &CompareOptions::default(),
            Some(&index),
        );
        let survivor = again
            .differences
            .iter()
            .find(|c| c.relative_path == local_name)
            .expect("the recreated file is compared");
        assert!(!survivor.previously_synced);
        assert_eq!(
            decide_sync_action(
                &survivor.status,
                &config.sync_direction,
                survivor.previously_synced,
                config.preserve_remote_deletes,
            ),
            SyncAction::Upload
        );
    }

    /// An identical file is not a difference row. Its two spellings still have
    /// to reach the baseline, or the watcher later stats the local name and
    /// uploads a second object.
    #[test]
    fn an_identical_pair_with_two_spellings_is_remembered() {
        fn named(name: &str, size: u64) -> FileInfo {
            let mut info = fi(size, 1_700_000_000);
            info.name = name.to_string();
            info.path = name.to_string();
            info
        }
        let local_name = "cafe\u{301}.txt";
        let remote_name = "café.txt";
        let mut local = HashMap::new();
        local.insert(local_name.to_string(), named(local_name, 8));
        local.insert("plain.txt".to_string(), named("plain.txt", 4));
        let mut remote = HashMap::new();
        remote.insert(remote_name.to_string(), named(remote_name, 8));
        remote.insert("plain.txt".to_string(), named("plain.txt", 4));
        let report =
            crate::sync::classify_with_summary(local, remote, &CompareOptions::default(), None);
        assert!(
            report
                .differences
                .iter()
                .all(|row| row.relative_path != local_name),
            "an identical file is not a difference row"
        );
        assert_eq!(report.mirrored.len(), 1);
        let row = &report.mirrored[0];
        assert_eq!(row.relative_path, local_name);
        assert_eq!(row.remote_relative_path.as_deref(), Some(remote_name));

        let svc = CloudService::new();
        let config = cfg(
            CompareDirection::Bidirectional,
            false,
            ConflictStrategy::AskUser,
        );
        let files =
            svc.post_sync_baseline(&report.mirrored, &config, None, UpdatePolicy::default());
        assert!(files.contains_key(local_name));
        assert!(!files.contains_key(remote_name));
        let spellings = crate::sync::spellings_after_baseline(&report.mirrored, None, &files);
        assert_eq!(
            spellings.get(local_name).map(String::as_str),
            Some(remote_name)
        );
    }

    /// Send-only, preserving remote files: the local copy is gone and the
    /// remote-only row is a skip, keyed by the remote spelling. The local key
    /// stays in the baseline. Its remote spelling has to stay with it, or the
    /// next recreate stats the local name and uploads a second object.
    #[test]
    fn a_skipped_remote_row_keeps_the_local_keys_remote_spelling() {
        let local_name = "cafe\u{301}.txt";
        let remote_name = "café.txt";
        let svc = CloudService::new();
        let config = cfg(
            CompareDirection::LocalToRemote,
            true,
            ConflictStrategy::AskUser,
        );
        let mut prior = SyncIndex::new("/l".to_string(), "/r".to_string());
        prior.files.insert(
            local_name.to_string(),
            SyncIndexEntry {
                size: 8,
                modified: Some(DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap()),
                is_dir: false,
                uploaded_at: None,
            },
        );
        prior
            .remote_spellings
            .insert(local_name.to_string(), remote_name.to_string());

        let mut row = cmp(
            SyncStatus::RemoteOnly,
            None,
            Some(fi(8, 1_700_000_000)),
            true,
            false,
        );
        row.relative_path = remote_name.to_string();
        row.remote_relative_path = Some(remote_name.to_string());
        row.baseline_key = Some(local_name.to_string());
        assert_eq!(
            svc.resolve_action(&config, &row, UpdatePolicy::default()),
            SyncAction::Skip
        );

        let files = svc.post_sync_baseline(
            std::slice::from_ref(&row),
            &config,
            Some(&prior),
            UpdatePolicy::default(),
        );
        assert!(files.contains_key(local_name));
        assert!(!files.contains_key(remote_name));
        let spellings = crate::sync::spellings_after_baseline(
            std::slice::from_ref(&row),
            Some(&prior.remote_spellings),
            &files,
        );
        assert_eq!(
            spellings.get(local_name).map(String::as_str),
            Some(remote_name)
        );

        // A mirror delete of that same row still drops the mapping, because
        // the local key itself leaves the baseline.
        let mirror = cfg(
            CompareDirection::LocalToRemote,
            false,
            ConflictStrategy::AskUser,
        );
        assert_eq!(
            svc.resolve_action(&mirror, &row, UpdatePolicy::default()),
            SyncAction::DeleteRemote
        );
        let gone = svc.post_sync_baseline(
            std::slice::from_ref(&row),
            &mirror,
            Some(&prior),
            UpdatePolicy::default(),
        );
        assert!(!gone.contains_key(local_name));
        let dropped = crate::sync::spellings_after_baseline(
            std::slice::from_ref(&row),
            Some(&prior.remote_spellings),
            &gone,
        );
        assert!(dropped.is_empty(), "{dropped:?}");
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
        let files = svc.post_sync_baseline(&[downloaded], &config, None, UpdatePolicy::default());
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
        let before = Utc::now();
        svc.note_upload_ended("f.txt", Utc::now());
        let after = Utc::now();
        svc.note_landed(&config, "f.txt");
        stamp(&file, 1_850_000_000);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let files = svc.post_sync_baseline(&[uploaded], &config, None, UpdatePolicy::default());
        let entry = files.get("f.txt").expect("the upload is recorded");
        assert_eq!(entry.size, 7);
        let ended = entry.uploaded_at.expect("an upload is dated");
        assert!(
            before <= ended && ended <= after,
            "dated when the upload ended, not when the index is written"
        );
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
        svc.process_comparison_with_provider(provider, &config, &uploaded, UpdatePolicy::default())
            .await
            .expect("the upload succeeds");
        let baseline = svc.post_sync_baseline(&[uploaded], &config, None, UpdatePolicy::default());
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
            profile_id: None,
            version: SYNC_INDEX_VERSION,
            last_sync: Utc::now(),
            local_path: root.to_string_lossy().into_owned(),
            remote_path: "/remote".to_string(),
            files: baseline.clone(),
            remote_spellings: Default::default(),
            cloud_receipts: Default::default(),
            cloud_remote_metadata: Default::default(),
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

    /// Update records uncertainty instead of retrying against its own newer server stamp.
    #[tokio::test]
    async fn update_upload_changed_during_its_own_send_stays_unsettled() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("f.txt");
        std::fs::write(&file, b"payload").unwrap();
        stamp(&file, 1_900_000_000);
        let mut local = fi(7, 1_900_000_000);
        local.path = file.display().to_string();
        let row = cmp(
            SyncStatus::LocalNewer,
            Some(local),
            Some(fi(3, 1)),
            true,
            false,
        );
        let config = CloudConfig {
            update_only: true,
            local_folder: root.path().into(),
            remote_folder: "/remote".into(),
            ..cfg(
                CompareDirection::LocalToRemote,
                false,
                ConflictStrategy::PreferLocal,
            )
        };
        let mut provider = StampingProvider {
            edits_during_upload: 1,
            ..Default::default()
        };
        let svc = CloudService::new();
        let policy = UpdatePolicy {
            root_identity: LocalRootIdentity::read(root.path()),
            ..Default::default()
        };
        assert_eq!(
            svc.process_comparison_with_provider(&mut provider, &config, &row, policy)
                .await
                .unwrap(),
            SyncAction::Upload
        );
        assert_eq!(provider.sent.len(), 1);
        let files = svc.post_sync_baseline(&[row], &config, None, policy);
        assert_eq!(files["f.txt"].modified, Some(UNSETTLED_BASELINE));
        assert_eq!(std::fs::read(&file).unwrap(), b"EDIT01!");
        assert_eq!(svc.take_unsettled(), vec!["f.txt"]);
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
            assert_ne!(
                provider.sent.last().unwrap(),
                &std::fs::read(&file).unwrap(),
                "the remote still holds an older send ({stat_time:?})"
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
        svc.process_comparison_with_provider(
            &mut StampingProvider::default(),
            &config,
            &uploaded,
            UpdatePolicy::default(),
        )
        .await
        .expect("the upload succeeds");
        assert!(
            started.elapsed() >= at_least,
            "a file written just now was sent after {:?}",
            started.elapsed()
        );

        // Left alone past the window: no wait. Tokio's clock is paused around
        // this one call, so it moves only if the call sleeps, and a loaded
        // runner cannot move it; the file's age is still read on the real
        // clock.
        std::fs::write(&file, b"payload").unwrap();
        tokio::time::sleep(window + std::time::Duration::from_millis(50)).await;
        tokio::time::pause();
        let started = tokio::time::Instant::now();
        CloudService::wait_until_quiet(&file, window, started + SEND_QUIET_BOUND).await;
        assert_eq!(
            tokio::time::Instant::now(),
            started,
            "a file left alone past the window is read without a wait"
        );
        tokio::time::resume();

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
                uploaded_at: None,
            };
            let prior = SyncIndex {
                profile_id: None,
                version: SYNC_INDEX_VERSION,
                last_sync: Utc::now(),
                local_path: "/l/unverified".to_string(),
                remote_path: "/unverified".to_string(),
                files: HashMap::from([
                    ("sub/b.txt".to_string(), entry(3)),
                    ("kept.txt".to_string(), entry(4)),
                ]),
                remote_spellings: Default::default(),
                cloud_receipts: Default::default(),
                cloud_remote_metadata: Default::default(),
                unverified_keys: std::collections::HashSet::from(["sub/b.txt".to_string()]),
            };
            let svc = CloudService::new();
            svc.unsettled.lock().unwrap().push("f.txt".to_string());
            svc.save_unsettled_into(
                "/l/unverified",
                "/unverified",
                &[],
                Some(&prior),
                None,
                None,
            );
            let saved = crate::sync::load_sync_index("/l/unverified", "/unverified")
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
    /// bound is the file's, once per cycle. The clock is paused so the
    /// waits are measured, not the runner's load.
    #[tokio::test(start_paused = true)]
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
        let started = tokio::time::Instant::now();
        svc.process_comparison_with_provider(
            &mut provider,
            &config,
            &uploaded,
            UpdatePolicy::default(),
        )
        .await
        .expect("the upload succeeds");
        let took = started.elapsed();
        assert_eq!(provider.sent.len(), 3);
        assert!(
            took >= std::time::Duration::from_millis(300)
                && took < std::time::Duration::from_millis(600),
            "three sends of a file written without pause waited {took:?}"
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
            .process_comparison_with_provider(
                &mut StampingProvider::default(),
                &config,
                &uploaded,
                UpdatePolicy::default(),
            )
            .await
            .expect("the upload succeeds");
        assert!(matches!(action, SyncAction::Upload));
        stamp(&file, 1_900_000_000);
        let files = svc.post_sync_baseline(&[uploaded], &config, None, UpdatePolicy::default());
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
        assert_eq!(
            svc.resolve_action(&config, &c, UpdatePolicy::default()),
            SyncAction::AskUser
        );
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
        assert_eq!(
            svc.resolve_action(&config, &c, UpdatePolicy::default()),
            SyncAction::Skip
        );
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
        assert_eq!(
            svc.resolve_action(&config, &c, UpdatePolicy::default()),
            SyncAction::DeleteRemote
        );
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
                    svc.resolve_action(&config, &c, UpdatePolicy::default()),
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

    #[cfg(unix)]
    #[tokio::test]
    async fn literal_unix_paths_scan_reaches_an_escaped_reincluded_child() {
        let root = tempfile::tempdir().unwrap();
        for folder in [r"dir\name", "dir/name"] {
            std::fs::create_dir_all(root.path().join(folder)).unwrap();
            for file in ["keep.txt", "drop.txt"] {
                std::fs::write(root.path().join(folder).join(file), file).unwrap();
            }
        }
        std::fs::write(
            root.path().join(".aeroignore"),
            "/dir\\\\name/\n!/dir\\\\name/keep.txt\n",
        )
        .unwrap();
        let config = CloudConfig {
            local_folder: root.path().to_path_buf(),
            exclude_patterns: vec!["**".into()],
            ..Default::default()
        };
        let (files, complete) = CloudService::new()
            .scan_local_folder(&config)
            .await
            .unwrap();
        assert!(complete);
        assert_eq!(files.len(), 1, "{:?}", files.keys());
        assert!(files.contains_key(r"dir\name/keep.txt"));
    }

    #[cfg(unix)]
    struct ModeReset<'a>(&'a std::path::Path);

    #[cfg(unix)]
    impl Drop for ModeReset<'_> {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    /// An escaped anchored negation used to make `may_reinclude_below` true
    /// for every excluded directory, so a scan entered trees that cannot
    /// match. An unreadable unrelated directory is the proof: entering it
    /// fails the scan, skipping it leaves the scan successful.
    #[cfg(unix)]
    #[tokio::test]
    async fn literal_unix_paths_escaped_negation_does_not_enter_unrelated_trees() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let literal = root.path().join(r"dir\name");
        std::fs::create_dir_all(&literal).unwrap();
        std::fs::write(literal.join("keep.txt"), "keep").unwrap();
        let other = root.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("secret.txt"), "secret").unwrap();
        std::fs::write(
            root.path().join(".aeroignore"),
            "/dir\\\\name/\n!/dir\\\\name/keep.txt\n",
        )
        .unwrap();
        let _restore = ModeReset(&other);
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            std::fs::read_dir(&other).is_err(),
            "mode 000 must be unreadable or this test cannot see an unwanted walk"
        );
        let config = CloudConfig {
            local_folder: root.path().to_path_buf(),
            exclude_patterns: vec!["**".into()],
            ..Default::default()
        };
        let (files, complete) = CloudService::new()
            .scan_local_folder(&config)
            .await
            .expect("an unrelated unreadable tree must not be entered");
        assert!(complete);
        assert_eq!(files.len(), 1, "{:?}", files.keys());
        assert!(files.contains_key(r"dir\name/keep.txt"));
    }

    /// A wildcard negation is still allowed to match below any excluded
    /// directory. The same unreadable tree must therefore be entered.
    #[cfg(unix)]
    #[tokio::test]
    async fn literal_unix_paths_wildcard_negation_still_enters_excluded_trees() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let other = root.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("secret.txt"), "secret").unwrap();
        std::fs::write(root.path().join(".aeroignore"), "!/dir\\\\name/*.txt\n").unwrap();
        let _restore = ModeReset(&other);
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o000)).unwrap();
        let config = CloudConfig {
            local_folder: root.path().to_path_buf(),
            exclude_patterns: vec!["**".into()],
            ..Default::default()
        };
        let scanned = CloudService::new().scan_local_folder(&config).await;
        assert!(
            scanned.is_err(),
            "a wildcard negation must still enter the excluded tree, got {scanned:?}"
        );
    }

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

    // ---- a path gone from both sides leaves the baseline ----

    /// A remote folder held in memory, listed in full (an authoritative
    /// listing). Listing `broken` fails, which leaves the scan incomplete.
    #[derive(Default)]
    struct MemoryProvider {
        cwd: String,
        /// Absolute path -> (content, time).
        stored: HashMap<String, (Vec<u8>, String)>,
        broken: Option<String>,
        list_time: Option<String>,
        list_time_overrides: HashMap<String, String>,
        fail_download_path: Option<String>,
        fail_upload: bool,
        fail_upload_path: Option<String>,
        fail_stat_path: Option<String>,
        stat_calls: Vec<String>,
        scan_floor: Option<Duration>,
        /// The login folder the provider recorded at connect, as FTP keeps
        /// the post-login `PWD`.
        login_folder: Option<String>,
        /// `None` answers SFTP; `Some(Ftp)` exercises the FTP rule that a
        /// missing login folder fails closed instead of a `cd("~")`.
        kind: Option<crate::providers::ProviderType>,
    }

    impl MemoryProvider {
        /// A file another device put on the remote.
        fn put(&mut self, path: &str, content: &[u8]) {
            self.stored.insert(
                path.to_string(),
                (content.to_vec(), Utc::now().to_rfc3339()),
            );
        }

        fn entry(&self, path: &str) -> Option<ProviderRemoteEntry> {
            let (content, time) = self.stored.get(path)?;
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            let mut entry = ProviderRemoteEntry::file(name, path.to_string(), content.len() as u64);
            entry.modified = Some(time.clone());
            Some(entry)
        }
    }

    #[async_trait::async_trait]
    impl StorageProvider for MemoryProvider {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> crate::providers::ProviderType {
            self.kind.unwrap_or(crate::providers::ProviderType::Sftp)
        }
        fn login_folder_hint(&self) -> Option<String> {
            self.login_folder.clone()
        }
        fn display_name(&self) -> String {
            "memory".to_string()
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
            if self.broken.as_deref() == Some(self.cwd.as_str()) {
                return Err(ProviderError::Other("listing failed".to_string()));
            }
            let prefix = format!("{}/", self.cwd);
            let mut folders = std::collections::BTreeSet::new();
            let mut out = Vec::new();
            for path in self.stored.keys() {
                let Some(rest) = path.strip_prefix(&prefix) else {
                    continue;
                };
                match rest.split_once('/') {
                    Some((folder, _)) => {
                        folders.insert(folder.to_string());
                    }
                    None => {
                        if let Some(mut entry) = self.entry(path) {
                            if let Some(time) = self
                                .list_time_overrides
                                .get(path)
                                .or(self.list_time.as_ref())
                            {
                                entry.modified = Some(time.clone());
                            }
                            out.push(entry);
                        }
                    }
                }
            }
            out.extend(folders.into_iter().map(|folder| {
                let path = format!("{prefix}{folder}");
                ProviderRemoteEntry::directory(folder, path)
            }));
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
            remote_path: &str,
            local_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            if self.fail_download_path.as_deref() == Some(remote_path) {
                return Err(ProviderError::Other("fixture download refused".into()));
            }
            let (content, _) = self
                .stored
                .get(remote_path)
                .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;
            std::fs::write(local_path, content).map_err(|e| ProviderError::Other(e.to_string()))
        }
        async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
            self.stored
                .get(remote_path)
                .map(|(content, _)| content.clone())
                .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))
        }
        async fn upload(
            &mut self,
            local_path: &str,
            remote_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            if self.fail_upload || self.fail_upload_path.as_deref() == Some(remote_path) {
                return Err(ProviderError::Other("fixture upload refused".into()));
            }
            let content =
                std::fs::read(local_path).map_err(|e| ProviderError::Other(e.to_string()))?;
            let time: DateTime<Utc> = std::fs::metadata(local_path)
                .and_then(|meta| meta.modified())
                .map_err(|e| ProviderError::Other(e.to_string()))?
                .into();
            self.stored
                .insert(remote_path.to_string(), (content, time.to_rfc3339()));
            Ok(())
        }
        async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
            self.stored
                .remove(path)
                .map(|_| ())
                .ok_or_else(|| ProviderError::NotFound(path.to_string()))
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
            self.stat_calls.push(path.to_string());
            if self.fail_stat_path.as_deref() == Some(path) {
                return Err(ProviderError::Other("fixture stat unavailable".into()));
            }
            self.entry(path)
                .ok_or_else(|| ProviderError::NotFound(path.to_string()))
        }
        async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
            self.stored
                .get(path)
                .map(|(content, _)| content.len() as u64)
                .ok_or_else(|| ProviderError::NotFound(path.to_string()))
        }
        async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
            Ok(self.stored.contains_key(path))
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("memory".to_string())
        }
        fn min_full_scan_interval(&self) -> Option<Duration> {
            self.scan_floor
        }
    }

    /// Run `body` with the sync index under a scratch `XDG_CONFIG_HOME`, which
    /// moves the AeroFTP data root on Linux only. Synchronous, so the shared
    /// environment lock (a std mutex) is never held across an `.await`.
    #[cfg(target_os = "linux")]
    fn with_scratch_index(body: impl std::future::Future<Output = ()>) {
        let _env = crate::test_env::lock();
        let data = tempfile::tempdir().unwrap();
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", data.path());
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(body)
        }));
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn manual_sync_records_the_connected_providers_scan_floor() {
        with_scratch_index(async {
            let local = tempfile::tempdir().unwrap();
            let floor = Duration::from_secs(30 * 60);
            let config = CloudConfig {
                enabled: true,
                local_folder: local.path().into(),
                remote_folder: "/manual-scan-traits".into(),
                sync_direction: CompareDirection::Bidirectional,
                ..Default::default()
            };
            let key = crate::cloud_scan_schedule::pair_key(&config);
            let start = std::time::Instant::now();
            crate::cloud_scan_schedule::ledger().record_full_scan(&key, start);
            let opened = ConnectedCloudProvider {
                provider: Box::new(MemoryProvider {
                    scan_floor: Some(floor),
                    ..Default::default()
                }),
                identity: None,
            };
            sync_opened_config(config, opened, None, true)
                .await
                .unwrap();
            let five_minutes = Duration::from_secs(5 * 60);
            assert!(
                !crate::cloud_scan_schedule::ledger().scheduled_full_scan_due(
                    &key,
                    start + five_minutes,
                    five_minutes,
                )
            );
            assert!(
                crate::cloud_scan_schedule::ledger().scheduled_full_scan_due(
                    &key,
                    start + floor,
                    five_minutes,
                )
            );
        });
    }

    /// A `~/...` remote folder is placed under the login folder on the first
    /// connect and the absolute value persisted: the pair whose local folder
    /// matches moves, an homonymous pair on another machine keeps its `~`
    /// form until its own connect places it.
    #[test]
    #[cfg(target_os = "linux")]
    fn anchor_places_a_login_relative_remote_folder_and_persists_the_pair() {
        with_scratch_index(async {
            let local_a = std::path::PathBuf::from("/sync/a");
            let local_b = std::path::PathBuf::from("/sync/b");
            let pair_for = |local: &std::path::Path| {
                crate::cloud_pairs::CloudPathPair::from_legacy(&CloudConfig {
                    enabled: true,
                    local_folder: local.to_path_buf(),
                    remote_folder: "~/docs".to_string(),
                    server_profile: "nas".to_string(),
                    protocol_type: "sftp".to_string(),
                    ..Default::default()
                })
            };
            cloud_pairs::with_cloud_pairs_mut(|pairs| {
                pairs.pairs = vec![pair_for(&local_a), pair_for(&local_b)];
                Ok(())
            })
            .unwrap();
            // The legacy single config resolves the same way.
            cloud_config::with_cloud_config_mut(|stored| {
                stored.remote_folder = "~/docs".to_string();
                stored.server_profile = "nas".to_string();
                stored.local_folder = local_a.clone();
                Ok(())
            })
            .unwrap();

            let config = CloudConfig {
                enabled: true,
                local_folder: local_a.clone(),
                remote_folder: "~/docs".to_string(),
                server_profile: "nas".to_string(),
                protocol_type: "sftp".to_string(),
                ..Default::default()
            };
            let mut provider = MemoryProvider {
                login_folder: Some("/home/u".to_string()),
                ..Default::default()
            };
            let placed = anchor_login_relative_remote_folder(
                &mut provider,
                &config,
                &resolved_as("p"),
                None,
                None,
            )
            .await
            .unwrap();
            assert_eq!(placed.as_deref(), Some("/home/u/docs"));

            let pairs = cloud_pairs::load_cloud_pairs_config();
            assert_eq!(pairs.pairs[0].remote_path, "/home/u/docs");
            assert_eq!(pairs.pairs[1].remote_path, "~/docs");
            let stored = cloud_config::load_cloud_config();
            assert_eq!(stored.remote_folder, "/home/u/docs");

            // A second connect finds the absolute value and places nothing.
            let config = CloudConfig {
                remote_folder: "/home/u/docs".to_string(),
                ..config
            };
            let placed = anchor_login_relative_remote_folder(
                &mut provider,
                &config,
                &resolved_as("p"),
                None,
                None,
            )
            .await
            .unwrap();
            assert_eq!(placed, None);
        });
    }

    /// A connection whose profile name selects the profile it connected to.
    fn resolved_as(id: &str) -> ProfileResolution {
        ProfileResolution {
            connected: Some(id.to_string()),
            by_name: Some(id.to_string()),
            by_reference: Some(id.to_string()),
            names_read: true,
        }
    }

    /// Profile names are not unique: an entry is placed only when its own
    /// reference reaches the profile the connection used. A reference pinned
    /// by id must be that id; a reference by name must select it. A
    /// homonymous profile's login folder never lands in another profile's
    /// configuration.
    #[test]
    #[cfg(target_os = "linux")]
    fn anchor_places_only_entries_that_reach_the_connected_profile() {
        with_scratch_index(async {
            let local = std::path::PathBuf::from("/sync/a");
            let by_name = CloudConfig {
                enabled: true,
                local_folder: local.clone(),
                remote_folder: "~/docs".to_string(),
                server_profile: "nas".to_string(),
                protocol_type: "sftp".to_string(),
                ..Default::default()
            };
            let reset = || {
                cloud_pairs::with_cloud_pairs_mut(|pairs| {
                    pairs.pairs = vec![crate::cloud_pairs::CloudPathPair::from_legacy(&by_name)];
                    Ok(())
                })
                .unwrap();
                cloud_config::with_cloud_config_mut(|stored| {
                    stored.remote_folder = "~/docs".to_string();
                    stored.server_profile = "nas".to_string();
                    stored.server_profile_id = Some("id-a".to_string());
                    stored.local_folder = local.clone();
                    Ok(())
                })
                .unwrap();
            };
            let mut provider = MemoryProvider {
                login_folder: Some("/home/u".to_string()),
                ..Default::default()
            };

            // Connected by name to id-b, the first profile named "nas": the
            // pair (by name) is placed, the configuration pinned to id-a is not.
            reset();
            anchor_login_relative_remote_folder(
                &mut provider,
                &by_name,
                &resolved_as("id-b"),
                None,
                None,
            )
            .await
            .unwrap();
            assert_eq!(
                cloud_pairs::load_cloud_pairs_config().pairs[0].remote_path,
                "/home/u/docs"
            );
            assert_eq!(cloud_config::load_cloud_config().remote_folder, "~/docs");

            // Pinned to id-a while "nas" selects id-b: the configuration is
            // placed, the pair (by name, so id-b) keeps its `~` form.
            reset();
            let by_id = CloudConfig {
                server_profile_id: Some("id-a".to_string()),
                ..by_name.clone()
            };
            let homonym = ProfileResolution {
                connected: Some("id-a".to_string()),
                by_name: Some("id-b".to_string()),
                by_reference: Some("id-a".to_string()),
                names_read: true,
            };
            anchor_login_relative_remote_folder(&mut provider, &by_id, &homonym, None, None)
                .await
                .unwrap();
            assert_eq!(
                cloud_pairs::load_cloud_pairs_config().pairs[0].remote_path,
                "~/docs"
            );
            assert_eq!(
                cloud_config::load_cloud_config().remote_folder,
                "/home/u/docs"
            );

            // Pinned to id-a and "nas" selects id-a: both reach it, both placed.
            reset();
            anchor_login_relative_remote_folder(
                &mut provider,
                &by_id,
                &resolved_as("id-a"),
                None,
                None,
            )
            .await
            .unwrap();
            assert_eq!(
                cloud_pairs::load_cloud_pairs_config().pairs[0].remote_path,
                "/home/u/docs"
            );
            assert_eq!(
                cloud_config::load_cloud_config().remote_folder,
                "/home/u/docs"
            );
        });
    }

    /// Placing a `~/...` folder against a `cloud_pairs.json` that does not
    /// parse is an error, and neither file is written: the pairs keep their
    /// bytes and the legacy config keeps its `~` form.
    #[test]
    #[cfg(target_os = "linux")]
    fn anchor_with_an_unparseable_pairs_file_writes_nothing() {
        with_scratch_index(async {
            cloud_config::with_cloud_config_mut(|stored| {
                stored.remote_folder = "~/docs".to_string();
                stored.server_profile = "nas".to_string();
                stored.local_folder = std::path::PathBuf::from("/sync/a");
                Ok(())
            })
            .unwrap();
            let pairs_path = cloud_pairs::get_pairs_path();
            let bytes = b"{ not json".as_slice();
            std::fs::write(&pairs_path, bytes).unwrap();
            let config = CloudConfig {
                enabled: true,
                local_folder: std::path::PathBuf::from("/sync/a"),
                remote_folder: "~/docs".to_string(),
                server_profile: "nas".to_string(),
                protocol_type: "sftp".to_string(),
                ..Default::default()
            };
            let mut provider = MemoryProvider {
                login_folder: Some("/home/u".to_string()),
                ..Default::default()
            };
            let err = anchor_login_relative_remote_folder(
                &mut provider,
                &config,
                &resolved_as("p"),
                None,
                None,
            )
            .await
            .unwrap_err();
            assert!(err.contains("parse"), "{err}");
            assert_eq!(std::fs::read(&pairs_path).unwrap(), bytes);
            assert_eq!(cloud_config::load_cloud_config().remote_folder, "~/docs");
        });
    }

    /// A placement with no entry to place rewrites no file: a pairs file
    /// holding only other folders keeps its exact bytes, and an absent
    /// legacy config is not created.
    /// A pair edited between the placement's first read and its locked
    /// write (another window saved it meanwhile) no longer names the `~`
    /// form: the write re-checks under the lock, places nothing over the
    /// edit, and saves nothing, so the file keeps the bytes the edit wrote.
    #[cfg(target_os = "linux")]
    #[test]
    fn anchor_leaves_a_pair_edited_after_its_first_read() {
        with_scratch_index(async {
            let pairs_path = cloud_pairs::get_pairs_path();
            std::fs::create_dir_all(pairs_path.parent().unwrap()).unwrap();
            let pair = |remote: &str| -> cloud_pairs::CloudPathPair {
                serde_json::from_value(serde_json::json!({
                    "id": "p1",
                    "name": "docs",
                    "local_path": "/sync/a",
                    "remote_path": remote,
                    "enabled": true,
                    "server_profile": "nas",
                }))
                .unwrap()
            };
            let compact = |remote: &str| {
                serde_json::to_vec(&cloud_pairs::CloudPairsConfig {
                    pairs: vec![pair(remote)],
                    ..Default::default()
                })
                .unwrap()
            };
            std::fs::write(&pairs_path, compact("~/docs")).unwrap();
            let edited = compact("/srv/elsewhere");
            {
                let pairs_path = pairs_path.clone();
                let edited = edited.clone();
                *BETWEEN_PLACEMENT_READ_AND_WRITE.lock().unwrap() = Some(Box::new(move || {
                    std::fs::write(&pairs_path, &edited).unwrap()
                }));
            }
            let config_path = cloud_config::get_config_path();
            let _ = std::fs::remove_file(&config_path);
            let config = CloudConfig {
                enabled: true,
                local_folder: std::path::PathBuf::from("/sync/a"),
                remote_folder: "~/docs".to_string(),
                server_profile: "nas".to_string(),
                protocol_type: "sftp".to_string(),
                ..Default::default()
            };
            let mut provider = MemoryProvider {
                login_folder: Some("/home/u".to_string()),
                ..Default::default()
            };
            let placed = anchor_login_relative_remote_folder(
                &mut provider,
                &config,
                &resolved_as("p"),
                None,
                None,
            )
            .await
            .unwrap();
            assert_eq!(placed.as_deref(), Some("/home/u/docs"));
            assert!(BETWEEN_PLACEMENT_READ_AND_WRITE.lock().unwrap().is_none());
            assert_eq!(std::fs::read(&pairs_path).unwrap(), edited);
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn anchor_rewrites_no_file_without_an_entry_to_place() {
        with_scratch_index(async {
            let pairs_path = cloud_pairs::get_pairs_path();
            std::fs::create_dir_all(pairs_path.parent().unwrap()).unwrap();
            let other: cloud_pairs::CloudPathPair = serde_json::from_value(serde_json::json!({
                "id": "p1",
                "name": "other",
                "local_path": "/sync/b",
                "remote_path": "/srv/other",
                "enabled": true,
                "server_profile": "nas",
            }))
            .unwrap();
            let bytes = serde_json::to_vec(&cloud_pairs::CloudPairsConfig {
                pairs: vec![other],
                ..Default::default()
            })
            .unwrap();
            std::fs::write(&pairs_path, &bytes).unwrap();
            let config_path = cloud_config::get_config_path();
            let _ = std::fs::remove_file(&config_path);
            let config = CloudConfig {
                enabled: true,
                local_folder: std::path::PathBuf::from("/sync/a"),
                remote_folder: "~/docs".to_string(),
                server_profile: "nas".to_string(),
                protocol_type: "sftp".to_string(),
                ..Default::default()
            };
            let mut provider = MemoryProvider {
                login_folder: Some("/home/u".to_string()),
                ..Default::default()
            };
            let placed = anchor_login_relative_remote_folder(
                &mut provider,
                &config,
                &resolved_as("p"),
                None,
                None,
            )
            .await
            .unwrap();
            assert_eq!(placed.as_deref(), Some("/home/u/docs"));
            assert_eq!(std::fs::read(&pairs_path).unwrap(), bytes);
            assert!(!config_path.exists());
        });
    }

    /// A `/~/...` remote folder already on disk names a literal `~`
    /// directory: refused with the correction, never synced.
    #[test]
    #[cfg(target_os = "linux")]
    fn anchor_refuses_a_literal_tilde_remote_folder() {
        with_scratch_index(async {
            let config = CloudConfig {
                remote_folder: "/~/docs".to_string(),
                ..Default::default()
            };
            let mut provider = MemoryProvider::default();
            let err = anchor_login_relative_remote_folder(
                &mut provider,
                &config,
                &resolved_as("p"),
                None,
                None,
            )
            .await
            .unwrap_err();
            assert!(err.contains("literal '~' directory"), "{err}");
            assert!(err.contains("~/docs"), "{err}");
        });
    }

    /// Without a login folder (WebDAV here) `~` is an ordinary name: a
    /// `/~/docs` or `~/docs` target is used as written, neither refused nor
    /// placed.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_tilde_is_an_ordinary_name_without_a_login_folder() {
        with_scratch_index(async {
            let mut provider = MemoryProvider {
                kind: Some(crate::providers::ProviderType::WebDav),
                ..Default::default()
            };
            for remote in ["/~/docs", "~/docs"] {
                let config = CloudConfig {
                    remote_folder: remote.to_string(),
                    ..Default::default()
                };
                let placed = place_login_relative_remote_folder(&mut provider, &config)
                    .await
                    .unwrap();
                assert_eq!(placed, None, "{remote}");
            }
        });
    }

    /// An FTP session whose post-login `PWD` did not answer cannot place a
    /// `~/...` remote folder: `cd("~")` is not portable there, so the anchor
    /// fails closed and nothing is persisted.
    #[test]
    #[cfg(target_os = "linux")]
    fn anchor_on_ftp_without_a_login_pwd_fails_and_persists_nothing() {
        with_scratch_index(async {
            cloud_config::with_cloud_config_mut(|stored| {
                stored.remote_folder = "~/docs".to_string();
                stored.server_profile = "nas".to_string();
                stored.local_folder = std::path::PathBuf::from("/sync/a");
                Ok(())
            })
            .unwrap();
            let config = CloudConfig {
                enabled: true,
                local_folder: std::path::PathBuf::from("/sync/a"),
                remote_folder: "~/docs".to_string(),
                server_profile: "nas".to_string(),
                protocol_type: "ftp".to_string(),
                ..Default::default()
            };
            let mut provider = MemoryProvider {
                kind: Some(crate::providers::ProviderType::Ftp),
                login_folder: None,
                ..Default::default()
            };
            let err = anchor_login_relative_remote_folder(
                &mut provider,
                &config,
                &resolved_as("p"),
                None,
                None,
            )
            .await
            .unwrap_err();
            assert!(err.contains("did not report its login folder"), "{err}");
            assert_eq!(
                cloud_config::load_cloud_config().remote_folder,
                "~/docs",
                "a refused placement must not persist"
            );
        });
    }

    /// A minimal FTP server for the AeroCloud cycle tests: answers the control
    /// conversation, logs every command line, and serves an empty listing over
    /// a PASV data connection. `login_dir` is what `PWD` reports after login,
    /// which is where the FTP provider anchors its login-relative paths.
    struct ScriptedFtp {
        port: u16,
        log: Arc<std::sync::Mutex<Vec<String>>>,
    }

    async fn scripted_ftp_server(login_dir: &'static str) -> ScriptedFtp {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let task_log = log.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let task_log = task_log.clone();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let _ = write.write_all(b"220 AeroFTP test server\r\n").await;
                    let mut reader = BufReader::new(read);
                    let mut data: Option<tokio::net::TcpListener> = None;
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                            break;
                        }
                        let cmd = line.trim_end_matches(['\r', '\n']).to_string();
                        let verb = cmd.split_whitespace().next().unwrap_or("").to_uppercase();
                        task_log.lock().unwrap().push(cmd);
                        if matches!(
                            verb.as_str(),
                            "MLSD" | "MLST" | "LIST" | "NLST" | "RETR" | "STOR" | "APPE"
                        ) {
                            let _ = write.write_all(b"150 Opening data connection\r\n").await;
                            if let Some(listener) = data.take() {
                                if let Ok((mut conn, _)) = listener.accept().await {
                                    match verb.as_str() {
                                        "STOR" | "APPE" => {
                                            let mut sink = Vec::new();
                                            let _ = tokio::io::AsyncReadExt::read_to_end(
                                                &mut conn, &mut sink,
                                            )
                                            .await;
                                        }
                                        "MLSD" | "MLST" => {}
                                        _ => {
                                            let _ = conn.write_all(b"total 0\r\n").await;
                                        }
                                    }
                                    let _ = tokio::io::AsyncWriteExt::shutdown(&mut conn).await;
                                }
                            }
                            let _ = write.write_all(b"226 Transfer complete\r\n").await;
                            continue;
                        }
                        let reply = match verb.as_str() {
                            "USER" => "331 Password required".to_string(),
                            "PASS" => "230 Logged in".to_string(),
                            "PWD" | "XPWD" => format!("257 \"{login_dir}\""),
                            "CWD" | "XCWD" | "CDUP" | "XCUP" => "250 OK".to_string(),
                            "TYPE" | "OPTS" | "PBSZ" | "PROT" | "NOOP" => "200 OK".to_string(),
                            "SYST" => "215 UNIX Type: L8".to_string(),
                            "FEAT" => "211-Features:\r\n UTF8\r\n211 End".to_string(),
                            "PASV" => {
                                let listener =
                                    tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                                let port = listener.local_addr().unwrap().port();
                                data = Some(listener);
                                format!(
                                    "227 Entering Passive Mode (127,0,0,1,{},{})",
                                    port / 256,
                                    port % 256
                                )
                            }
                            "EPSV" => {
                                let listener =
                                    tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                                let port = listener.local_addr().unwrap().port();
                                data = Some(listener);
                                format!("229 Entering Extended Passive Mode (|||{port}|)")
                            }
                            "MKD" | "XMKD" => "257 \"created\"".to_string(),
                            "RMD" | "XRMD" | "DELE" => "250 OK".to_string(),
                            "RNFR" => "350 Ready for RNTO".to_string(),
                            "RNTO" => "250 OK".to_string(),
                            "QUIT" => {
                                let _ = write.write_all(b"221 Bye\r\n").await;
                                break;
                            }
                            "MDTM" | "SIZE" | "MLST" | "STAT" => "550 Not available".to_string(),
                            _ => "502 Not implemented".to_string(),
                        };
                        if write
                            .write_all(format!("{reply}\r\n").as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
        });
        ScriptedFtp { port, log }
    }

    /// Clears the process-wide cached vault on drop so a test vault never
    /// leaks into the next test.
    struct VaultGuard;
    impl Drop for VaultGuard {
        fn drop(&mut self) {
            credential_store::CredentialStore::clear_cache();
        }
    }

    /// An unlocked test vault cached for `create_cloud_connection`, plus the
    /// guard that clears the cache when the test ends.
    fn open_test_vault() -> (
        tempfile::TempDir,
        credential_store::CredentialStore,
        VaultGuard,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.db");
        let key = [0x24; 32];
        credential_store::CredentialStore::write_test_vault(&path, &key);
        credential_store::CredentialStore::cache_vault(path, key);
        let store = credential_store::CredentialStore::from_cache().expect("vault cached");
        (dir, store, VaultGuard)
    }

    /// Seed the legacy profiles blob and the `server_<id>` credential the
    /// factory reads, the way a saved FTP profile is stored.
    fn seed_ftp_profile(
        store: &credential_store::CredentialStore,
        id: &str,
        port: u16,
        overlay: Option<serde_json::Value>,
    ) {
        let mut profile = serde_json::json!({
            "id": id,
            "name": "nas",
            "protocol": "ftp",
            "host": "127.0.0.1",
            "port": port,
            "username": "u",
            "options": {"tlsMode": "none"},
        });
        if let Some(overlay) = overlay {
            profile["aeroCryptOverlay"] = overlay;
        }
        let mut profiles: Vec<serde_json::Value> = store
            .get("config_server_profiles")
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        profiles.push(profile);
        store
            .store(
                "config_server_profiles",
                &serde_json::json!(profiles).to_string(),
            )
            .unwrap();
        store.store(&format!("server_{id}"), "pw").unwrap();
    }

    /// Seed the keystore secrets an AeroCrypt binding unlocks from, so the
    /// overlay stack resolves without a remote marker (headerless).
    fn seed_aerocrypt_overlay(store: &credential_store::CredentialStore, id: &str) {
        use base64::Engine as _;
        let salt = crate::aerocrypt::overlay::random_salt_v3();
        let tmp = crate::aerocrypt::overlay::OverlayConfig::v3_bootstrap(salt);
        let master_key =
            crate::aerocrypt::overlay::derive_master_key(&tmp, "overlay-pass").unwrap();
        let config_json = crate::aerocrypt::overlay::init_config_v3(&salt, &master_key).unwrap();
        let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
        store
            .store(&format!("aerocrypt_overlay_pw_{id}"), "overlay-pass")
            .unwrap();
        store
            .store(&format!("aerocrypt_overlay_salt_{id}"), &salt_b64)
            .unwrap();
        store
            .store(&format!("aerocrypt_overlay_config_{id}"), &config_json)
            .unwrap();
    }

    fn aerocloud_config(local: &std::path::Path, remote: &str, id: &str) -> CloudConfig {
        CloudConfig {
            enabled: true,
            server_profile: "nas".to_string(),
            server_profile_id: Some(id.to_string()),
            protocol_type: "ftp".to_string(),
            connection_params: serde_json::json!({}),
            local_folder: local.to_path_buf(),
            remote_folder: remote.to_string(),
            exclude_patterns: Vec::new(),
            sync_direction: CompareDirection::Bidirectional,
            ..CloudConfig::default()
        }
    }

    /// A `~/docs` pair on a full cycle writes under the placed login folder
    /// (`/home/u/docs`), never under a literal `~`: `run_pair_full_cycle`
    /// applies the folder `open_aerocloud_connection` placed, like the other
    /// two cycle paths already did.
    #[test]
    #[cfg(target_os = "linux")]
    fn full_cycle_places_a_login_relative_folder_under_the_login_dir() {
        with_scratch_index(async {
            let ftp = scripted_ftp_server("/home/u").await;
            let (_vault_dir, store, _guard) = open_test_vault();
            seed_ftp_profile(&store, "srv_full", ftp.port, None);
            let local = tempfile::tempdir().unwrap();
            std::fs::write(local.path().join("note.txt"), b"hello").unwrap();
            let config = aerocloud_config(local.path(), "~/docs", "srv_full");
            cloud_pairs::with_cloud_pairs_mut(|pairs| {
                pairs
                    .pairs
                    .push(crate::cloud_pairs::CloudPathPair::from_legacy(&config));
                Ok(())
            })
            .unwrap();
            cloud_config::with_cloud_config_mut(|stored| {
                stored.remote_folder = "~/docs".to_string();
                stored.server_profile = "nas".to_string();
                stored.local_folder = local.path().to_path_buf();
                Ok(())
            })
            .unwrap();

            let report = run_pair_full_cycle(config, Some(&store), None)
                .await
                .unwrap();
            assert!(
                report.result.errors.is_empty(),
                "full cycle errors: {:?}",
                report.result.errors
            );
            assert!(report.result.uploaded >= 1, "the local file must upload");
            let log = ftp.log.lock().unwrap().join("\n");
            assert!(log.contains("/home/u/docs"), "commands:\n{log}");
            assert!(!log.contains('~'), "commands:\n{log}");
            assert_eq!(
                cloud_config::load_cloud_config().remote_folder,
                "/home/u/docs"
            );
        });
    }

    /// A preview places a `~/...` folder for its own run only: both saved
    /// files keep their exact bytes, and the preview still reads the placed
    /// folder, never a literal `~`.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_dry_run_places_the_folder_without_saving_it() {
        with_scratch_index(async {
            let ftp = scripted_ftp_server("/home/u").await;
            let (_vault_dir, store, _guard) = open_test_vault();
            seed_ftp_profile(&store, "srv_dry", ftp.port, None);
            let local = tempfile::tempdir().unwrap();
            std::fs::write(local.path().join("note.txt"), b"hello").unwrap();
            let config = aerocloud_config(local.path(), "~/docs", "srv_dry");
            cloud_pairs::with_cloud_pairs_mut(|pairs| {
                pairs
                    .pairs
                    .push(crate::cloud_pairs::CloudPathPair::from_legacy(&config));
                Ok(())
            })
            .unwrap();
            cloud_config::with_cloud_config_mut(|stored| {
                *stored = config.clone();
                Ok(())
            })
            .unwrap();
            let config_bytes = std::fs::read(cloud_config::get_config_path()).unwrap();
            let pairs_bytes = std::fs::read(cloud_pairs::get_pairs_path()).unwrap();

            sync_one_config(config, Some(&store), None, true)
                .await
                .unwrap();

            assert_eq!(
                std::fs::read(cloud_config::get_config_path()).unwrap(),
                config_bytes
            );
            assert_eq!(
                std::fs::read(cloud_pairs::get_pairs_path()).unwrap(),
                pairs_bytes
            );
            let log = ftp.log.lock().unwrap().join("\n");
            assert!(!log.contains('~'), "commands:\n{log}");
        });
    }

    /// The connected profile is the one the connection carries, never a
    /// re-read: if the list changed while connecting, the re-read would name
    /// another profile. Only the name lookup reads the list as it is now.
    /// Replace the saved profiles with `(id, name)` FTP profiles, written
    /// where `mcp_list_active_server_profiles` reads them (the active
    /// account's list once the first read migrated the vault), by the same
    /// writer the GUI uses, which takes the profile list lock.
    fn set_saved_profiles(store: &credential_store::CredentialStore, profiles: &[(&str, &str)]) {
        let list: Vec<serde_json::Value> = profiles
            .iter()
            .map(|(id, name)| {
                serde_json::json!({
                    "id": id, "name": name, "protocol": "ftp", "host": "127.0.0.1",
                    "port": 21, "username": "u", "options": {"tlsMode": "none"},
                })
            })
            .collect();
        crate::user_partitions::init_or_migrate_cli(store).unwrap();
        let mut conn = crate::user_partitions::open_or_init_cli().unwrap();
        let root_key = store.derive_user_partition_wrapping_key();
        crate::user_partitions::replace_active_server_profiles(&mut conn, &root_key, &list)
            .unwrap();
        let read = crate::user_partitions::mcp_list_active_server_profiles(store).unwrap();
        assert_eq!(read.len(), profiles.len());
    }

    /// The endpoint a connection made now for `config` would carry, as the
    /// factory fingerprints it.
    fn connected_endpoint(
        store: &credential_store::CredentialStore,
        config: &CloudConfig,
    ) -> Option<String> {
        cloud_provider_factory::current_endpoint_fingerprint(store, config).unwrap()
    }

    /// A config and a pair, both by the name "nas", on `~/docs` from `/sync/a`.
    fn seed_unplaced_entries() -> CloudConfig {
        let config = CloudConfig {
            enabled: true,
            local_folder: "/sync/a".into(),
            remote_folder: "~/docs".to_string(),
            server_profile: "nas".to_string(),
            protocol_type: "sftp".to_string(),
            ..Default::default()
        };
        let pair = crate::cloud_pairs::CloudPathPair::from_legacy(&config);
        cloud_pairs::with_cloud_pairs_mut(|pairs| {
            pairs.pairs = vec![pair];
            Ok(())
        })
        .unwrap();
        let stored = config.clone();
        cloud_config::with_cloud_config_mut(move |disk| {
            *disk = stored;
            Ok(())
        })
        .unwrap();
        config
    }

    fn saved_forms() -> (String, String) {
        (
            cloud_config::load_cloud_config().remote_folder,
            cloud_pairs::load_cloud_pairs_config().pairs[0]
                .remote_path
                .clone(),
        )
    }

    /// The name check and the placement write happen while the saved profile
    /// list is locked: a rename that moved the name to another profile after
    /// the caller resolved it is read under the lock, and the entries by that
    /// name keep their `~` form; while the placement writes, no profile
    /// writer can take the list.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_profile_name_moved_before_the_placement_keeps_the_tilde_form() {
        with_scratch_index(async {
            let (_vault_dir, store, _guard) = open_test_vault();
            let login = || MemoryProvider {
                login_folder: Some("/home/u".to_string()),
                ..Default::default()
            };
            for moved in [false, true] {
                set_saved_profiles(&store, &[("srv_a", "nas"), ("srv_b", "other")]);
                let config = seed_unplaced_entries();
                let resolution =
                    ProfileResolution::for_connection(Some(&store), &config, Some("srv_a".into()))
                        .unwrap();
                assert_eq!(resolution.by_name.as_deref(), Some("srv_a"));
                if moved {
                    // Another window renames srv_a and gives its name to srv_b.
                    *BEFORE_PLACEMENT_LOCK.lock().unwrap() = Some(Box::new(|| {
                        let store = credential_store::CredentialStore::from_cache().unwrap();
                        set_saved_profiles(&store, &[("srv_a", "old"), ("srv_b", "nas")]);
                    }));
                }
                *BETWEEN_PLACEMENT_READ_AND_WRITE.lock().unwrap() = Some(Box::new(|| {
                    let busy = crate::user_partitions::ProfileListLock::acquire_within(
                        &crate::user_partitions::cli_db_path().unwrap(),
                        Duration::ZERO,
                    );
                    assert_eq!(
                        busy.err().as_deref(),
                        Some(crate::user_partitions::PROFILE_LIST_BUSY)
                    );
                }));
                let placed = anchor_login_relative_remote_folder(
                    &mut login(),
                    &config,
                    &resolution,
                    Some(&store),
                    connected_endpoint(&store, &config).as_deref(),
                )
                .await
                .unwrap();
                assert_eq!(placed.as_deref(), Some("/home/u/docs"), "moved {moved}");
                assert!(BEFORE_PLACEMENT_LOCK.lock().unwrap().is_none());
                assert!(BETWEEN_PLACEMENT_READ_AND_WRITE.lock().unwrap().is_none());
                let expected = if moved { "~/docs" } else { "/home/u/docs" };
                assert_eq!(
                    saved_forms(),
                    (expected.to_string(), expected.to_string()),
                    "moved {moved}"
                );
            }
            // The lock is released with the placement.
            crate::user_partitions::ProfileListLock::acquire_within(
                &crate::user_partitions::cli_db_path().unwrap(),
                Duration::ZERO,
            )
            .unwrap();
        });
    }

    /// A connection with no saved profile (the config's own credentials)
    /// places the folder in the entries that carry its reference, so the
    /// saved config and the next cycle read the folder this one synced. An
    /// entry whose name now selects a saved profile connects there and keeps
    /// its `~` form, and without the names read nothing is placed.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_connection_without_a_saved_profile_places_its_own_entries() {
        with_scratch_index(async {
            let (_vault_dir, store, _guard) = open_test_vault();
            let login = || MemoryProvider {
                login_folder: Some("/home/u".to_string()),
                ..Default::default()
            };
            set_saved_profiles(&store, &[("srv_x", "elsewhere")]);
            // The credentials the config's own connection logs in with.
            store
                .store(
                    "server_nas",
                    r#"{"server":"127.0.0.1:22","username":"u","password":"pw"}"#,
                )
                .unwrap();
            let config = seed_unplaced_entries();
            let resolution =
                ProfileResolution::for_connection(Some(&store), &config, None).unwrap();
            let placed = anchor_login_relative_remote_folder(
                &mut login(),
                &config,
                &resolution,
                Some(&store),
                connected_endpoint(&store, &config).as_deref(),
            )
            .await
            .unwrap();
            assert_eq!(placed.as_deref(), Some("/home/u/docs"));
            assert_eq!(
                saved_forms(),
                ("/home/u/docs".to_string(), "/home/u/docs".to_string())
            );
            // The reloaded config is the folder the cycle synced: a second
            // connect places nothing and keeps it.
            let reloaded = cloud_config::load_cloud_config();
            assert_eq!(reloaded.remote_folder, "/home/u/docs");
            let again = ProfileResolution::for_connection(Some(&store), &reloaded, None).unwrap();
            assert_eq!(
                anchor_login_relative_remote_folder(
                    &mut login(),
                    &reloaded,
                    &again,
                    Some(&store),
                    connected_endpoint(&store, &reloaded).as_deref()
                )
                .await
                .unwrap(),
                None
            );

            // A saved profile took the name meanwhile: those entries connect
            // through it now.
            let config = seed_unplaced_entries();
            let resolution =
                ProfileResolution::for_connection(Some(&store), &config, None).unwrap();
            *BEFORE_PLACEMENT_LOCK.lock().unwrap() = Some(Box::new(|| {
                let store = credential_store::CredentialStore::from_cache().unwrap();
                set_saved_profiles(&store, &[("srv_x", "nas")]);
            }));
            anchor_login_relative_remote_folder(
                &mut login(),
                &config,
                &resolution,
                Some(&store),
                connected_endpoint(&store, &config).as_deref(),
            )
            .await
            .unwrap();
            assert_eq!(saved_forms(), ("~/docs".to_string(), "~/docs".to_string()));

            // Locked vault: no name was read.
            set_saved_profiles(&store, &[("srv_x", "elsewhere")]);
            let config = seed_unplaced_entries();
            let unread = ProfileResolution::for_connection(None, &config, None).unwrap();
            anchor_login_relative_remote_folder(&mut login(), &config, &unread, None, None)
                .await
                .unwrap();
            assert_eq!(saved_forms(), ("~/docs".to_string(), "~/docs".to_string()));
        });
    }

    /// The placement moves the pair's scan ledger state with the entries it
    /// saves: a full scan asked for under the `~` key is due under the
    /// absolute key the scheduler reads next, whoever placed it (the manual
    /// sync, a background cycle, the folder picker). A placement that saves
    /// nothing leaves the ledger where it was.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_placement_moves_the_scan_ledger_with_the_saved_entries() {
        with_scratch_index(async {
            let login = || MemoryProvider {
                login_folder: Some("/home/u".to_string()),
                ..Default::default()
            };
            let seed = |local: &str| {
                let config = CloudConfig {
                    local_folder: local.into(),
                    ..seed_unplaced_entries()
                };
                let pair = crate::cloud_pairs::CloudPathPair::from_legacy(&config);
                cloud_pairs::with_cloud_pairs_mut(|pairs| {
                    pairs.pairs = vec![pair];
                    Ok(())
                })
                .unwrap();
                let stored = config.clone();
                cloud_config::with_cloud_config_mut(move |disk| {
                    *disk = stored;
                    Ok(())
                })
                .unwrap();
                config
            };
            let key = crate::cloud_scan_schedule::pair_key;
            let now = std::time::Instant::now();

            // Placed and saved: the request follows the entries.
            let config = seed("/sync/ledger-moves");
            let tilde_key = key(&config);
            crate::cloud_scan_schedule::ledger()
                .request_full_scan(&tilde_key, "the watcher lost events");
            let resolution = ProfileResolution::for_connection(None, &config, None).unwrap();
            let resolution = ProfileResolution {
                names_read: true,
                ..resolution
            };
            let placed =
                anchor_login_relative_remote_folder(&mut login(), &config, &resolution, None, None)
                    .await
                    .unwrap();
            assert_eq!(placed.as_deref(), Some("/home/u/docs"));
            let placed_key = key(&cloud_config::load_cloud_config());
            assert_ne!(placed_key, tilde_key);
            {
                let ledger = crate::cloud_scan_schedule::ledger();
                assert!(ledger.pending_full_scan_due(&placed_key, now));
                assert!(!ledger.pending_full_scan_due(&tilde_key, now));
            }

            // Nothing saved (the names were not read): the request stays
            // under the key the entries still carry.
            let config = seed("/sync/ledger-stays");
            let tilde_key = key(&config);
            crate::cloud_scan_schedule::ledger()
                .request_full_scan(&tilde_key, "the watcher lost events");
            let unread = ProfileResolution::for_connection(None, &config, None).unwrap();
            anchor_login_relative_remote_folder(&mut login(), &config, &unread, None, None)
                .await
                .unwrap();
            assert_eq!(cloud_config::load_cloud_config().remote_folder, "~/docs");
            let ledger = crate::cloud_scan_schedule::ledger();
            assert!(ledger.pending_full_scan_due(&tilde_key, now));
            assert!(!ledger.pending_full_scan_due(
                &key(&CloudConfig {
                    remote_folder: "/home/u/docs".to_string(),
                    ..config
                }),
                now
            ));
        });
    }

    /// The configuration pinned to id-a is placed while the pair on the same
    /// local folder resolves by name to id-b and keeps its `~` form: both
    /// shared the `~` key, and the pair still syncs under it, so a full scan
    /// asked for there stays due there and is due under the placed key too.
    /// When nothing is left on the `~` key, the state moves.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_pair_left_on_the_tilde_key_keeps_its_scan_request() {
        with_scratch_index(async {
            let key = crate::cloud_scan_schedule::pair_key;
            let local = std::path::PathBuf::from("/sync/homonym-ledger");
            let by_id = CloudConfig {
                enabled: true,
                local_folder: local.clone(),
                remote_folder: "~/docs".to_string(),
                server_profile: "nas".to_string(),
                server_profile_id: Some("id-a".to_string()),
                protocol_type: "sftp".to_string(),
                ..Default::default()
            };
            let reset = || {
                let pair = crate::cloud_pairs::CloudPathPair::from_legacy(&by_id);
                cloud_pairs::with_cloud_pairs_mut(|pairs| {
                    pairs.pairs = vec![pair];
                    Ok(())
                })
                .unwrap();
                let stored = by_id.clone();
                cloud_config::with_cloud_config_mut(move |disk| {
                    *disk = stored;
                    Ok(())
                })
                .unwrap();
            };
            let tilde_key = key(&by_id);
            let placed_key = key(&CloudConfig {
                remote_folder: "/home/u/docs".to_string(),
                ..by_id.clone()
            });
            let now = std::time::Instant::now();
            let mut provider = MemoryProvider {
                login_folder: Some("/home/u".to_string()),
                ..Default::default()
            };

            reset();
            crate::cloud_scan_schedule::ledger()
                .request_full_scan(&tilde_key, "the watcher lost events");
            let homonym = ProfileResolution {
                connected: Some("id-a".to_string()),
                by_name: Some("id-b".to_string()),
                by_reference: Some("id-a".to_string()),
                names_read: true,
            };
            anchor_login_relative_remote_folder(&mut provider, &by_id, &homonym, None, None)
                .await
                .unwrap();
            assert_eq!(
                cloud_config::load_cloud_config().remote_folder,
                "/home/u/docs"
            );
            assert_eq!(
                cloud_pairs::load_cloud_pairs_config().pairs[0].remote_path,
                "~/docs"
            );
            {
                let ledger = crate::cloud_scan_schedule::ledger();
                assert!(
                    ledger.pending_full_scan_due(&tilde_key, now),
                    "the pair's request stays"
                );
                assert!(ledger.pending_full_scan_due(&placed_key, now));
            }

            // Both entries reach the connected profile: nothing is left on
            // the `~` key, so the state moves.
            reset();
            crate::cloud_scan_schedule::ledger().record_full_scan(&placed_key, now);
            let both = ProfileResolution {
                connected: Some("id-a".to_string()),
                by_name: Some("id-a".to_string()),
                by_reference: Some("id-a".to_string()),
                names_read: true,
            };
            anchor_login_relative_remote_folder(&mut provider, &by_id, &both, None, None)
                .await
                .unwrap();
            assert_eq!(
                cloud_pairs::load_cloud_pairs_config().pairs[0].remote_path,
                "/home/u/docs"
            );
            let ledger = crate::cloud_scan_schedule::ledger();
            assert!(
                !ledger.pending_full_scan_due(&tilde_key, now),
                "nothing is left on the `~` key"
            );
            assert!(
                ledger.pending_full_scan_due(&placed_key, now),
                "the request moved"
            );
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn the_resolution_keeps_the_profile_the_connection_used() {
        with_scratch_index(async {
            let (_vault_dir, store, _guard) = open_test_vault();
            // "nas" now selects srv_b; the connection was made with srv_a.
            seed_ftp_profile(&store, "srv_b", 21, None);
            let config = CloudConfig {
                server_profile: "nas".to_string(),
                server_profile_id: Some("srv_a".to_string()),
                ..Default::default()
            };
            let resolution =
                ProfileResolution::for_connection(Some(&store), &config, Some("srv_a".into()))
                    .unwrap();
            assert_eq!(resolution.connected.as_deref(), Some("srv_a"));
            assert_eq!(resolution.by_name.as_deref(), Some("srv_b"));
        });
    }

    /// A remote folder persisted under a previous login home must not sync
    /// once the profile's login-relative overlay resolves beneath the new
    /// one: mixed mode would upload it plaintext, so the cycle refuses
    /// before any transfer.
    #[test]
    #[cfg(target_os = "linux")]
    fn full_cycle_refuses_a_folder_persisted_under_another_login_home() {
        with_scratch_index(async {
            let ftp = scripted_ftp_server("/srv/u").await;
            let (_vault_dir, store, _guard) = open_test_vault();
            let overlay = serde_json::json!({
                "enabled": true,
                "kind": "aerocrypt",
                "remoteScope": "~/docs",
                "withHeader": false,
            });
            seed_ftp_profile(&store, "srv_move", ftp.port, Some(overlay));
            seed_aerocrypt_overlay(&store, "srv_move");
            let local = tempfile::tempdir().unwrap();
            std::fs::write(local.path().join("note.txt"), b"hello").unwrap();
            // The absolute value the first connect persisted under /home/u.
            let config = aerocloud_config(local.path(), "/home/u/docs", "srv_move");

            let err = run_pair_full_cycle(config, Some(&store), None)
                .await
                .unwrap_err();
            assert!(
                err.message.contains("/home/u/docs") && err.message.contains("/srv/u/docs"),
                "the refusal must name target and anchor: {}",
                err.message
            );
            let log = ftp.log.lock().unwrap().join("\n");
            assert!(!log.contains("STOR"), "no upload may happen:\n{log}");
        });
    }

    /// The overlay is wrapped for the profile the connection used. A profile
    /// list edited while connecting (the connected profile renamed, a
    /// homonym with the same binding but none of its keys saved under the
    /// name) passes both binding checks, so only the connected id keeps the
    /// overlay on the keys of the profile the session belongs to: resolved
    /// again by name, the overlay finds no keys and the cycle fails.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_overlay_follows_the_connected_profile_when_the_name_moves() {
        with_scratch_index(async {
            let ftp = scripted_ftp_server("/home/u").await;
            let (_vault_dir, store, _guard) = open_test_vault();
            let binding = serde_json::json!({
                "enabled": true,
                "kind": "aerocrypt",
                "remoteScope": "/docs",
                "withHeader": false,
            });
            seed_ftp_profile(&store, "srv_a", ftp.port, Some(binding));
            seed_aerocrypt_overlay(&store, "srv_a");
            let local = tempfile::tempdir().unwrap();
            let mut config = aerocloud_config(local.path(), "/docs/sync", "srv_a");
            config.server_profile_id = None;

            let opened = cloud_provider_factory::create_cloud_connection(&config)
                .await
                .unwrap();
            assert_eq!(opened.profile_id.as_deref(), Some("srv_a"));
            // Meanwhile srv_a is renamed and srv_b takes its name, written
            // where the connect read the list (the active account's).
            let mut profiles =
                crate::user_partitions::mcp_list_active_server_profiles(&store).unwrap();
            assert_eq!(profiles.len(), 1);
            let mut homonym = profiles[0].clone();
            homonym["id"] = serde_json::json!("srv_b");
            profiles[0]["name"] = serde_json::json!("old");
            profiles.push(homonym);
            let mut conn = crate::user_partitions::open_or_init_cli().unwrap();
            let root_key = store.derive_user_partition_wrapping_key();
            crate::user_partitions::replace_active_server_profiles(&mut conn, &root_key, &profiles)
                .unwrap();
            let names = crate::user_partitions::mcp_list_active_server_profiles(&store).unwrap();
            assert_eq!(
                crate::crypt_overlay_provider::select_aerocloud_profile(&names, None, "nas")
                    .and_then(|p| p["id"].as_str()),
                Some("srv_b")
            );

            let mut composed = compose_aerocloud_connection(opened, &mut config, &store, false)
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            // The overlay is on: a folder outside its anchor is refused.
            assert!(
                ensure_remote_folder_within_overlay(&mut *composed.provider, "/elsewhere").is_err()
            );
        });
    }

    /// The profile a connection used is edited to another endpoint (host,
    /// port, user, starting folder) after the connect, under the same id and
    /// name: the login folder this session found belongs to the old
    /// endpoint, so the placement keeps the saved `~` form for the next
    /// connect to place, while this run still syncs the folder it opened. An
    /// unedited profile is placed and saved.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_profile_edited_after_the_connect_keeps_the_tilde_form() {
        with_scratch_index(async {
            let ftp = scripted_ftp_server("/home/u").await;
            let (_vault_dir, store, _guard) = open_test_vault();
            seed_ftp_profile(&store, "srv_e", ftp.port, None);
            let original = crate::user_partitions::mcp_list_active_server_profiles(&store).unwrap();
            let write_profiles = |profiles: &[serde_json::Value]| {
                let mut conn = crate::user_partitions::open_or_init_cli().unwrap();
                let root_key = store.derive_user_partition_wrapping_key();
                crate::user_partitions::replace_active_server_profiles(
                    &mut conn, &root_key, profiles,
                )
                .unwrap();
            };
            let local = tempfile::tempdir().unwrap();
            let edits: [(&str, Option<(&str, serde_json::Value)>); 5] = [
                ("unedited", None),
                ("host", Some(("host", serde_json::json!("localhost")))),
                ("port", Some(("port", serde_json::json!(ftp.port + 1)))),
                ("user", Some(("username", serde_json::json!("v")))),
                (
                    "starting folder",
                    Some(("initialPath", serde_json::json!("/srv"))),
                ),
            ];
            for (what, edit) in edits {
                write_profiles(&original);
                let mut config = aerocloud_config(local.path(), "~/docs", "srv_e");
                let stored = config.clone();
                cloud_config::with_cloud_config_mut(move |disk| {
                    *disk = stored;
                    Ok(())
                })
                .unwrap();
                let pair = crate::cloud_pairs::CloudPathPair::from_legacy(&config);
                cloud_pairs::with_cloud_pairs_mut(|pairs| {
                    pairs.pairs = vec![pair];
                    Ok(())
                })
                .unwrap();

                let opened = cloud_provider_factory::create_cloud_connection(&config)
                    .await
                    .unwrap_or_else(|e| panic!("{what}: {e}"));
                assert!(opened.endpoint.is_some(), "{what}");
                if let Some((field, value)) = &edit {
                    let mut edited = original.clone();
                    edited[0][*field] = value.clone();
                    write_profiles(&edited);
                }
                let composed = compose_aerocloud_connection(opened, &mut config, &store, true)
                    .await
                    .unwrap_or_else(|e| panic!("{what}: {e}"));
                drop(composed);

                assert_eq!(config.remote_folder, "/home/u/docs", "{what}: this run");
                let expected = if edit.is_some() {
                    "~/docs"
                } else {
                    "/home/u/docs"
                };
                assert_eq!(
                    cloud_config::load_cloud_config().remote_folder,
                    expected,
                    "{what}: saved config"
                );
                assert_eq!(
                    cloud_pairs::load_cloud_pairs_config().pairs[0].remote_path,
                    expected,
                    "{what}: saved pair"
                );
            }
        });
    }

    /// A hand-set absolute remote folder outside the overlay anchor is
    /// refused the same way: nothing uploads cleartext beside the vault.
    #[test]
    #[cfg(target_os = "linux")]
    fn full_cycle_refuses_an_absolute_folder_outside_the_overlay_scope() {
        with_scratch_index(async {
            let ftp = scripted_ftp_server("/home/u").await;
            let (_vault_dir, store, _guard) = open_test_vault();
            let overlay = serde_json::json!({
                "enabled": true,
                "kind": "aerocrypt",
                "remoteScope": "/docs",
                "withHeader": false,
            });
            seed_ftp_profile(&store, "srv_elsewhere", ftp.port, Some(overlay));
            seed_aerocrypt_overlay(&store, "srv_elsewhere");
            let local = tempfile::tempdir().unwrap();
            std::fs::write(local.path().join("note.txt"), b"hello").unwrap();
            let config = aerocloud_config(local.path(), "/altrove", "srv_elsewhere");

            let err = run_pair_full_cycle(config, Some(&store), None)
                .await
                .unwrap_err();
            assert!(
                err.message.contains("/altrove") && err.message.contains("/docs"),
                "the refusal must name target and anchor: {}",
                err.message
            );
            let log = ftp.log.lock().unwrap().join("\n");
            assert!(!log.contains("STOR"), "no upload may happen:\n{log}");
        });
    }

    /// A manual first "Sync now" on a `~/...` folder records its full scan
    /// under the key of the folder the connect placed and saved, the key the
    /// scheduler reads on its next tick: the interval and the provider's
    /// floor hold the next scheduled scan back, and a full scan asked for
    /// under the `~` key before the placement is answered, not left pending.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_manual_first_sync_records_its_scan_under_the_placed_key() {
        with_scratch_index(async {
            let ftp = scripted_ftp_server("/home/u").await;
            let (_vault_dir, store, _guard) = open_test_vault();
            seed_ftp_profile(&store, "srv_manual", ftp.port, None);
            let local = tempfile::tempdir().unwrap();
            let config = aerocloud_config(local.path(), "~/docs", "srv_manual");
            cloud_config::with_cloud_config_mut(|stored| {
                *stored = config.clone();
                Ok(())
            })
            .unwrap();
            let tilde_key = crate::cloud_scan_schedule::pair_key(&config);
            crate::cloud_scan_schedule::ledger()
                .request_full_scan(&tilde_key, "the watcher lost events");
            let start = std::time::Instant::now();

            sync_one_config(config.clone(), Some(&store), None, false)
                .await
                .unwrap_or_else(|e| panic!("{e}"));

            let reloaded = cloud_config::load_cloud_config();
            assert_eq!(reloaded.remote_folder, "/home/u/docs");
            let placed_key = crate::cloud_scan_schedule::pair_key(&reloaded);
            assert_ne!(placed_key, tilde_key);
            let five_minutes = Duration::from_secs(5 * 60);
            let ledger = crate::cloud_scan_schedule::ledger();
            assert!(!ledger.scheduled_full_scan_due(&placed_key, start, five_minutes));
            assert!(ledger.scheduled_full_scan_due(
                &placed_key,
                start + five_minutes + Duration::from_secs(60),
                five_minutes
            ));
            assert!(!ledger.pending_full_scan_due(&placed_key, start + five_minutes));
            // The ledger is shared by the process: only this pair's keys are read.
            assert!(
                !ledger.pending_full_scan_due(&tilde_key, start + five_minutes),
                "nothing pends under the `~` key"
            );
        });
    }

    /// A watcher plan prepared while the remote folder still read `~/docs`
    /// is refused once the connect places it: the pass fails closed before
    /// any write, asks for a full scan, and the next full cycle syncs the
    /// placed absolute folder. The history kept under `~/docs` belongs to the
    /// folder an older build opened by sending `~/docs` as written, which many
    /// servers open as a folder literally named `~`: a file it lists that is
    /// missing from `/home/u/docs` is uploaded, never deleted locally. The
    /// placement is saved, so the passes after the scan stay incremental.
    #[test]
    #[cfg(target_os = "linux")]
    fn incremental_plan_prepared_on_the_tilde_form_is_refused_then_full_scanned() {
        with_scratch_index(async {
            let ftp = scripted_ftp_server("/home/u").await;
            let (_vault_dir, store, _guard) = open_test_vault();
            seed_ftp_profile(&store, "srv_plan", ftp.port, None);
            let local = tempfile::tempdir().unwrap();
            // A baseline the pair would have from before placement existed,
            // keyed on the `~` form, plus a local file the watcher reports.
            // `old.txt` is unchanged since that baseline, so read against it
            // the file would only be missing remotely.
            std::fs::write(local.path().join("old.txt"), b"old").unwrap();
            std::fs::File::options()
                .write(true)
                .open(local.path().join("old.txt"))
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000))
                .unwrap();
            let mut index = crate::sync::SyncIndex::new(
                local.path().to_string_lossy().into_owned(),
                "~/docs".to_string(),
            );
            index.files.insert(
                "old.txt".to_string(),
                crate::sync::SyncIndexEntry {
                    size: 3,
                    modified: chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0),
                    is_dir: false,
                    uploaded_at: None,
                },
            );
            crate::sync::save_sync_index(&index).unwrap();
            std::fs::write(local.path().join("new.txt"), b"new").unwrap();

            let config = aerocloud_config(local.path(), "~/docs", "srv_plan");
            cloud_config::with_cloud_config_mut(|stored| {
                *stored = config.clone();
                Ok(())
            })
            .unwrap();
            let changes = LocalChanges {
                paths: vec![local.path().join("new.txt")],
                renames: Vec::new(),
            };
            let prep =
                prepare_incremental(&config, &changes, IncrementalHints { compare_size: true })
                    .unwrap();
            let plan = match prep {
                IncrementalPrep::Work { plan, .. } => plan,
                IncrementalPrep::Settled { .. } => {
                    panic!("expected a work plan, every path settled")
                }
                IncrementalPrep::FullScanOnly(reason) => {
                    panic!("expected a work plan, full scan only: {reason}")
                }
            };

            let err = run_pair_incremental_cycle(config.clone(), Some(&store), None, plan, false)
                .await
                .unwrap_err();
            assert!(
                err.message.contains("configuration changed"),
                "the stale plan must be refused: {}",
                err.message
            );
            assert!(
                err.needs_full_scan,
                "the worker must answer the refusal with a full scan"
            );
            // The worker asks the full scan under the key of the config the
            // connect opened, not the `~` key it computed before the cycle:
            // the pending request must land where the next turn's reloaded
            // config will look for it.
            let tilde_key = crate::cloud_scan_schedule::pair_key(&config);
            let mut placed = config.clone();
            placed.remote_folder = "/home/u/docs".to_string();
            let placed_key = crate::cloud_scan_schedule::pair_key(&placed);
            assert_eq!(
                err.placed_key, placed_key,
                "the error must carry the key of the config the connect opened"
            );
            let mut ledger = crate::cloud_scan_schedule::ScanLedger::default();
            ledger.request_full_scan(&err.placed_key, &err.message);
            let now = std::time::Instant::now();
            assert!(
                ledger.pending_full_scan_due(&placed_key, now),
                "the placed pair waits for its full scan"
            );
            assert!(
                !ledger.pending_full_scan_due(&tilde_key, now),
                "nothing may pend under the `~` key the config no longer has"
            );
            let log = ftp.log.lock().unwrap().join("\n");
            assert!(
                !log.contains("STOR"),
                "the refused pass wrote nothing:\n{log}"
            );

            // The next cycle is the full scan the worker asked for, on the
            // placed absolute folder.
            let report = run_pair_full_cycle(config, Some(&store), None)
                .await
                .unwrap();
            assert!(
                report.result.errors.is_empty(),
                "{:?}",
                report.result.errors
            );
            // Every ledger write of the cycle lands under the placed key:
            // the scan just run answers the placed pair's schedule, and the
            // `~` key the config no longer has stays untouched.
            assert_eq!(report.placed_key, placed_key);
            ledger.record_full_scan(&report.placed_key, now);
            if let Some(traits) = report.traits {
                ledger.learn(&report.placed_key, traits);
            }
            let user_interval = std::time::Duration::from_secs(5 * 60);
            assert!(
                !ledger.scheduled_full_scan_due(&placed_key, now, user_interval),
                "the placed pair just scanned"
            );
            assert!(
                ledger.scheduled_full_scan_due(&tilde_key, now, user_interval),
                "nothing may be recorded under the `~` key"
            );
            let log = ftp.log.lock().unwrap().join("\n");
            assert!(log.contains("/home/u/docs"), "commands:\n{log}");
            assert!(!log.contains('~'), "commands:\n{log}");
            // The file the `~` history lists, unchanged locally and absent
            // from the placed folder, is uploaded, never deleted locally.
            assert!(local.path().join("old.txt").exists());
            assert!(
                log.lines()
                    .any(|command| command.starts_with("STOR") && command.contains("old.txt")),
                "commands:\n{log}"
            );

            // The connect saved the placed folder, so the watcher's next
            // plans carry its identity and run as incremental passes.
            let stored = cloud_config::load_cloud_config_strict().unwrap();
            assert_eq!(stored.remote_folder, "/home/u/docs");
            for name in ["second.txt", "third.txt"] {
                std::fs::write(local.path().join(name), name.as_bytes()).unwrap();
                let changes = LocalChanges {
                    paths: vec![local.path().join(name)],
                    renames: Vec::new(),
                };
                let plan = match prepare_incremental(
                    &stored,
                    &changes,
                    IncrementalHints { compare_size: true },
                )
                .unwrap()
                {
                    IncrementalPrep::Work { plan, .. } => plan,
                    _ => panic!("{name}: expected a work plan"),
                };
                let report =
                    run_pair_incremental_cycle(stored.clone(), Some(&store), None, plan, false)
                        .await
                        .unwrap_or_else(|e| panic!("{name}: {}", e.message));
                assert!(!report.ran_full_scan, "{name}");
                assert_eq!(report.placed_key, placed_key, "{name}");
            }
            let log = ftp.log.lock().unwrap().join("\n");
            assert!(
                log.lines()
                    .any(|command| command.starts_with("STOR") && command.contains("third.txt")),
                "commands:\n{log}"
            );
        });
    }

    const GONE_REMOTE: &str = "/gone-on-both";

    /// A Skip/Identical/mirrored bookkeeping entry is not a transfer receipt.
    #[test]
    #[cfg(target_os = "linux")]
    fn update_never_transferred_pairs_remain_protected_after_source_deletion() {
        with_scratch_index(async {
            for upload in [true, false] {
                for kind in ["newer", "identical", "mirrored"] {
                    let root = tempfile::tempdir().unwrap();
                    let config = CloudConfig {
                        enabled: true,
                        update_only: true,
                        preserve_remote_deletes: false,
                        sync_direction: if upload {
                            CompareDirection::LocalToRemote
                        } else {
                            CompareDirection::RemoteToLocal
                        },
                        local_folder: root.path().into(),
                        remote_folder: "/proof".into(),
                        ..Default::default()
                    };
                    let local_key = if kind == "mirrored" {
                        "café.txt"
                    } else {
                        "photo"
                    };
                    let remote_key = if kind == "mirrored" {
                        "cafe\u{301}.txt"
                    } else {
                        "photo"
                    };
                    let mut provider = MemoryProvider::default();
                    let local_path = root.path().join(local_key);
                    let remote_path = format!("/proof/{remote_key}");
                    let source = b"old bytes";
                    let destination = if kind == "newer" {
                        &b"sole newer copy"[..]
                    } else {
                        &source[..]
                    };
                    std::fs::write(&local_path, if upload { &source[..] } else { destination })
                        .unwrap();
                    let local_time = if !upload && kind == "newer" {
                        1_800_000_000
                    } else {
                        1_700_000_000
                    };
                    filetime::set_file_mtime(
                        &local_path,
                        filetime::FileTime::from_unix_time(local_time, 0),
                    )
                    .unwrap();
                    let remote_time = if upload && kind == "newer" {
                        1_800_000_000
                    } else {
                        1_700_000_000
                    };
                    provider.stored.insert(
                        remote_path.clone(),
                        (
                            (if upload { destination } else { &source[..] }).to_vec(),
                            DateTime::<Utc>::from_timestamp(remote_time, 0)
                                .unwrap()
                                .to_rfc3339(),
                        ),
                    );
                    // A syncable companion keeps empty-source protection from masking missing provenance.
                    std::fs::write(root.path().join("keep"), b"keep").unwrap();
                    filetime::set_file_mtime(
                        root.path().join("keep"),
                        filetime::FileTime::from_unix_time(1_700_000_000, 0),
                    )
                    .unwrap();
                    provider.stored.insert(
                        "/proof/keep".into(),
                        (
                            b"keep".to_vec(),
                            DateTime::<Utc>::from_timestamp(1_700_000_000, 0)
                                .unwrap()
                                .to_rfc3339(),
                        ),
                    );
                    let svc = CloudService::new();
                    svc.init(config.clone()).await;
                    let first = svc
                        .perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert_eq!(first.uploaded + first.downloaded, 0, "{upload}/{kind}");
                    assert!(svc.load_index(&config).unwrap().cloud_receipts.is_empty());
                    if upload {
                        std::fs::remove_file(&local_path).unwrap();
                    } else {
                        provider.stored.remove(&remote_path);
                    }
                    for _ in 0..3 {
                        let preview = svc
                            .preview_full_sync_with_provider(&mut provider)
                            .await
                            .unwrap();
                        let result = svc
                            .perform_full_sync_with_provider(&mut provider)
                            .await
                            .unwrap();
                        assert_eq!((preview.deleted, result.deleted), (0, 0), "{upload}/{kind}");
                        assert!(
                            result.skipped_details.iter().any(|row| row.needs_review),
                            "{upload}/{kind}"
                        );
                        let kept = if upload {
                            provider.stored[&remote_path].0.clone()
                        } else {
                            std::fs::read(&local_path).unwrap()
                        };
                        assert_eq!(kept, destination);
                        assert!(svc.load_index(&config).unwrap().cloud_receipts.is_empty());
                    }
                }
            }
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn update_legacy_v2_slash_bookkeeping_never_authorizes_a_delete() {
        with_scratch_index(async {
            for upload in [true, false] {
                let root = tempfile::tempdir().unwrap();
                let config = CloudConfig {
                    enabled: true,
                    update_only: true,
                    preserve_remote_deletes: false,
                    sync_direction: if upload {
                        CompareDirection::LocalToRemote
                    } else {
                        CompareDirection::RemoteToLocal
                    },
                    local_folder: root.path().into(),
                    remote_folder: "/legacy".into(),
                    ..Default::default()
                };
                let mut provider = MemoryProvider::default();
                let mut legacy =
                    SyncIndex::new(root.path().to_string_lossy().into_owned(), "/legacy".into());
                legacy.version = 2;
                legacy.files.insert(
                    "photo".into(),
                    SyncIndexEntry {
                        size: 9,
                        modified: DateTime::<Utc>::from_timestamp(1_700_000_000, 0),
                        is_dir: false,
                        uploaded_at: None,
                    },
                );
                let raw = serde_json::to_string(&legacy).unwrap();
                assert!(!raw.contains("cloud_receipts"));
                save_sync_index(&serde_json::from_str(&raw).unwrap()).unwrap();
                if upload {
                    provider.put("/legacy/photo", b"sole copy");
                } else {
                    std::fs::write(root.path().join("photo"), b"sole copy").unwrap();
                }
                std::fs::write(root.path().join("keep"), b"keep").unwrap();
                provider.put("/legacy/keep", b"keep");
                let svc = CloudService::new();
                svc.init(config.clone()).await;
                for _ in 0..3 {
                    let result = svc
                        .perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert_eq!(result.deleted, 0);
                    assert!(result
                        .skipped_details
                        .iter()
                        .any(|row| row.path == "photo" && row.needs_review));
                    assert!(!svc
                        .load_index(&config)
                        .unwrap()
                        .cloud_receipts
                        .contains_key("photo"));
                }
            }
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn update_changed_transferred_destination_is_kept_with_its_original_receipt() {
        with_scratch_index(async {
            for upload in [true, false] {
                let root = tempfile::tempdir().unwrap();
                let config = CloudConfig {
                    enabled: true,
                    update_only: true,
                    preserve_remote_deletes: false,
                    sync_direction: if upload {
                        CompareDirection::LocalToRemote
                    } else {
                        CompareDirection::RemoteToLocal
                    },
                    local_folder: root.path().into(),
                    remote_folder: "/changed".into(),
                    ..Default::default()
                };
                let mut provider = MemoryProvider::default();
                for key in ["photo", "keep"] {
                    if upload {
                        std::fs::write(root.path().join(key), b"sent bytes").unwrap();
                    } else {
                        provider.put(&format!("/changed/{key}"), b"sent bytes");
                    }
                }
                let svc = CloudService::new();
                svc.init(config.clone()).await;
                let first = svc
                    .perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap();
                assert!(first.errors.is_empty(), "{:?}", first.errors);
                let prior = svc.load_index(&config).unwrap();
                assert!(prior.cloud_receipts.contains_key("photo"), "{upload}");
                let receipt = serde_json::to_value(&prior.cloud_receipts["photo"]).unwrap();
                if upload {
                    provider.put("/changed/photo", b"new phone modification");
                    std::fs::remove_file(root.path().join("photo")).unwrap();
                } else {
                    std::fs::write(root.path().join("photo"), b"new local modification").unwrap();
                    provider.stored.remove("/changed/photo");
                }
                for _ in 0..3 {
                    let result = svc
                        .perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert_eq!(result.deleted, 0);
                    assert!(
                        result.skipped_details.iter().any(|row| row.path == "photo"
                            && row.needs_review
                            && row.reason.contains("Destination changed")),
                        "{:?}",
                        result.skipped_details
                    );
                    assert_eq!(
                        serde_json::to_value(
                            &svc.load_index(&config).unwrap().cloud_receipts["photo"]
                        )
                        .unwrap(),
                        receipt
                    );
                }
            }
        });
    }

    /// A replacement root cannot inherit proof captured after a transfer from the original tree.
    #[test]
    #[cfg(target_os = "linux")]
    fn update_receipt_writeback_cannot_rebind_a_completed_transfer_to_a_replaced_root() {
        with_scratch_index(async {
            for upload in [true, false] {
                let base = tempfile::tempdir().unwrap();
                let root = base.path().join("active");
                std::fs::create_dir(&root).unwrap();
                std::fs::write(root.join("keep"), b"keep").unwrap();
                let config = CloudConfig {
                    enabled: true,
                    update_only: true,
                    preserve_remote_deletes: false,
                    local_folder: root.clone(),
                    remote_folder: "/root-swap".into(),
                    sync_direction: if upload {
                        CompareDirection::LocalToRemote
                    } else {
                        CompareDirection::RemoteToLocal
                    },
                    ..Default::default()
                };
                let mut provider = MemoryProvider::default();
                provider.put("/root-swap/keep", b"keep");
                if upload {
                    std::fs::write(root.join("photo"), b"actual transfer").unwrap();
                } else {
                    provider.put("/root-swap/photo", b"actual transfer");
                }
                let svc = CloudService::new();
                svc.init(config.clone()).await;
                let plan = svc
                    .build_sync_plan_with_provider(&mut provider, &config, false)
                    .await
                    .unwrap();
                for row in &plan.comparisons {
                    svc.process_comparison_with_provider(
                        &mut provider,
                        &config,
                        row,
                        plan.update_policy,
                    )
                    .await
                    .unwrap();
                }
                let files =
                    svc.post_sync_baseline(&plan.comparisons, &config, None, plan.update_policy);
                let original = svc.receipts_after(
                    &plan.comparisons,
                    &config,
                    None,
                    &files,
                    plan.update_policy.root_identity,
                );
                assert!(original.contains_key("photo"));
                std::fs::rename(&root, base.path().join("original")).unwrap();
                std::fs::create_dir(&root).unwrap();
                let replacement = svc.receipts_after(
                    &plan.comparisons,
                    &config,
                    None,
                    &files,
                    plan.update_policy.root_identity,
                );
                assert!(
                    replacement.is_empty(),
                    "a completed transfer must not mint proof for another physical root"
                );
                let mut prior = SyncIndex::new(
                    root.to_string_lossy().into_owned(),
                    config.remote_folder.clone(),
                );
                prior.files = files.clone();
                prior.cloud_receipts = original.clone();
                let prior_svc = CloudService::new();
                let kept = prior_svc.receipts_after(
                    &plan.comparisons,
                    &config,
                    Some(&prior),
                    &files,
                    plan.update_policy.root_identity,
                );
                assert_eq!(
                    serde_json::to_value(&kept).unwrap(),
                    serde_json::to_value(&original).unwrap(),
                    "old proof keeps its original scope"
                );
                prior.cloud_receipts = kept;
                prior_svc.remember_cloud_receipts(Some(&prior));
                let row = plan
                    .comparisons
                    .iter()
                    .find(|row| row.relative_path == "photo")
                    .unwrap();
                assert!(prior_svc.cloud_receipt_for(&config, row).is_none());
                prior_svc.revoke_cloud_receipt(row);
                let revoked = prior_svc.receipts_after(
                    &plan.comparisons,
                    &config,
                    Some(&prior),
                    &files,
                    plan.update_policy.root_identity,
                );
                assert!(
                    !revoked.contains_key("photo"),
                    "root change must not bypass persisted revocation"
                );
            }
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn update_transient_account_identity_preserves_proof_without_minting() {
        with_scratch_index(async {
            for upload in [true, false] {
                let root = tempfile::tempdir().unwrap();
                let config = CloudConfig {
                    enabled: true,
                    update_only: true,
                    preserve_remote_deletes: false,
                    local_folder: root.path().into(),
                    remote_folder: "/identity".into(),
                    sync_direction: if upload {
                        CompareDirection::LocalToRemote
                    } else {
                        CompareDirection::RemoteToLocal
                    },
                    ..Default::default()
                };
                let mut provider = MemoryProvider::default();
                std::fs::write(root.path().join("keep"), b"keep").unwrap();
                provider.put("/identity/keep", b"keep");
                if upload {
                    std::fs::write(root.path().join("photo"), b"photo").unwrap();
                } else {
                    provider.put("/identity/photo", b"photo");
                }
                let mut svc = CloudService::new();
                svc.connection_identity = Some(Some("account-one".into()));
                svc.init(config.clone()).await;
                assert!(svc
                    .perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap()
                    .errors
                    .is_empty());
                let before =
                    serde_json::to_value(svc.load_index(&config).unwrap().cloud_receipts).unwrap();
                svc.connection_identity = Some(None);
                assert!(svc
                    .perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap()
                    .errors
                    .is_empty());
                assert_eq!(
                    serde_json::to_value(svc.load_index(&config).unwrap().cloud_receipts).unwrap(),
                    before
                );
                // A new transfer while identity is unavailable cannot acquire proof.
                if upload {
                    std::fs::write(root.path().join("unknown"), b"unknown").unwrap();
                } else {
                    provider.put("/identity/unknown", b"unknown");
                }
                svc.perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap();
                assert!(!svc
                    .load_index(&config)
                    .unwrap()
                    .cloud_receipts
                    .contains_key("unknown"));
                svc.connection_identity = Some(Some("account-one".into()));
                if upload {
                    std::fs::remove_file(root.path().join("photo")).unwrap();
                } else {
                    provider.stored.remove("/identity/photo");
                }
                assert_eq!(
                    svc.perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap()
                        .deleted,
                    1
                );
            }
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn update_inspects_changed_rows_not_the_unchanged_tree() {
        with_scratch_index(async {
            let root = tempfile::tempdir().unwrap();
            let config = CloudConfig {
                enabled: true,
                update_only: true,
                preserve_remote_deletes: true,
                local_folder: root.path().into(),
                remote_folder: "/scale".into(),
                sync_direction: CompareDirection::LocalToRemote,
                ..Default::default()
            };
            let mut provider = MemoryProvider::default();
            for i in 0..1000 {
                let name = format!("file{i}");
                let path = root.path().join(&name);
                std::fs::write(&path, b"same").unwrap();
                filetime::set_file_mtime(
                    &path,
                    filetime::FileTime::from_unix_time(1_700_000_000, 0),
                )
                .unwrap();
                provider.stored.insert(
                    format!("/scale/{name}"),
                    (b"same".to_vec(), "2023-11-14T22:13:20Z".into()),
                );
            }
            provider.put("/scale/untracked", b"remote only");
            let svc = CloudService::new();
            svc.init(config.clone()).await;
            let preview = svc
                .preview_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert!(preview.errors.is_empty());
            assert!(provider.stat_calls.is_empty());
            std::fs::write(root.path().join("file1"), b"changed").unwrap();
            let preview = svc
                .preview_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert_eq!(preview.uploaded, 1);
            assert_eq!(provider.stat_calls.len(), 1);
            provider.stat_calls.clear();
            let run = svc
                .perform_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert!(run.errors.is_empty(), "{:?}", run.errors);
            assert_eq!(run.uploaded, 1);
            assert!(
                provider.stat_calls.len() <= 4,
                "{}",
                provider.stat_calls.len()
            );
        });
    }

    /// Server LIST clocks (S3) must warm once, not require a HEAD per unchanged file forever.
    #[test]
    #[cfg(target_os = "linux")]
    fn update_reuses_persisted_stat_clocks_only_for_unchanged_scoped_snapshots() {
        with_scratch_index(async {
            let root = tempfile::tempdir().unwrap();
            let config = CloudConfig {
                enabled: true,
                update_only: true,
                preserve_remote_deletes: true,
                local_folder: root.path().into(),
                remote_folder: "/s3-clock".into(),
                sync_direction: CompareDirection::LocalToRemote,
                ..Default::default()
            };
            let mut provider = MemoryProvider {
                list_time: Some("2030-01-01T00:00:00Z".into()),
                ..Default::default()
            };
            for i in 0..1000 {
                let name = format!("file{i}");
                let path = root.path().join(&name);
                std::fs::write(&path, b"same").unwrap();
                filetime::set_file_mtime(
                    &path,
                    filetime::FileTime::from_unix_time(1_700_000_000, 0),
                )
                .unwrap();
                provider.stored.insert(
                    format!("/s3-clock/{name}"),
                    (b"same".to_vec(), "2023-11-14T22:13:20Z".into()),
                );
            }
            let mut first = CloudService::new();
            first.connection_identity = Some(Some("account-a".into()));
            first.init(config.clone()).await;
            assert!(first
                .perform_full_sync_with_provider(&mut provider)
                .await
                .unwrap()
                .errors
                .is_empty());
            assert_eq!(
                provider.stat_calls.len(),
                1000,
                "first scan must verify unknown logical dates"
            );
            let index = first.load_index(&config).unwrap();
            assert_eq!(index.cloud_remote_metadata.len(), 1000);
            assert!(
                index.cloud_receipts.is_empty(),
                "normalization never creates transfer authority"
            );
            let mut next = CloudService::new();
            next.connection_identity = Some(Some("account-a".into()));
            next.init(config.clone()).await;
            provider.stat_calls.clear();
            next.preview_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert!(
                provider.stat_calls.is_empty(),
                "preview reuses persisted, unchanged LIST snapshots"
            );
            assert!(next
                .perform_full_sync_with_provider(&mut provider)
                .await
                .unwrap()
                .errors
                .is_empty());
            assert!(
                provider.stat_calls.is_empty(),
                "clean cycles reuse the cache across service instances"
            );
            provider
                .list_time_overrides
                .insert("/s3-clock/file2".into(), "2031-01-01T00:00:00Z".into());
            next.perform_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert_eq!(
                provider.stat_calls,
                vec!["/s3-clock/file2"],
                "changed raw LIST invalidates only that entry"
            );
            provider.stat_calls.clear();
            std::fs::write(root.path().join("file1"), b"changed local").unwrap();
            let preview = next
                .preview_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert_eq!(preview.uploaded, 1);
            assert_eq!(provider.stat_calls, vec!["/s3-clock/file1"]);
            provider.stat_calls.clear();
            assert!(next
                .perform_full_sync_with_provider(&mut provider)
                .await
                .unwrap()
                .errors
                .is_empty());
            assert!(provider.stat_calls.len() <= 4);
            assert!(
                !next
                    .load_index(&config)
                    .unwrap()
                    .cloud_remote_metadata
                    .contains_key("file1"),
                "attempted writes invalidate old observations"
            );
            provider.stat_calls.clear();
            next.connection_identity = Some(Some("account-b".into()));
            next.preview_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert_eq!(
                provider.stat_calls.len(),
                1000,
                "another target cannot borrow cached logical clocks"
            );
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn update_unknown_identity_and_partial_failure_retain_only_untouched_prior_proof() {
        with_scratch_index(async {
            for upload in [true, false] {
                let root = tempfile::tempdir().unwrap();
                let config = CloudConfig {
                    enabled: true,
                    update_only: true,
                    preserve_remote_deletes: true,
                    local_folder: root.path().into(),
                    remote_folder: "/unknown-partial".into(),
                    sync_direction: if upload {
                        CompareDirection::LocalToRemote
                    } else {
                        CompareDirection::RemoteToLocal
                    },
                    ..Default::default()
                };
                let mut provider = MemoryProvider::default();
                for key in ["keep", "touched"] {
                    if upload {
                        std::fs::write(root.path().join(key), b"original").unwrap();
                    } else {
                        provider.put(&format!("/unknown-partial/{key}"), b"original");
                    }
                }
                let mut svc = CloudService::new();
                svc.connection_identity = Some(Some("known".into()));
                svc.init(config.clone()).await;
                assert!(svc
                    .perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap()
                    .errors
                    .is_empty());
                let prior = svc.load_index(&config).unwrap();
                assert_eq!(prior.cloud_receipts.len(), 2);
                svc.connection_identity = Some(None);
                if upload {
                    std::fs::write(root.path().join("unknown"), b"new source").unwrap();
                    std::fs::write(root.path().join("touched"), b"changed source").unwrap();
                    filetime::set_file_mtime(
                        root.path().join("touched"),
                        filetime::FileTime::from_unix_time(2_000_000_000, 0),
                    )
                    .unwrap();
                    provider.fail_upload_path = Some("/unknown-partial/touched".into());
                } else {
                    provider.put("/unknown-partial/unknown", b"new source");
                    provider.stored.insert(
                        "/unknown-partial/touched".into(),
                        (b"changed source".to_vec(), "2033-05-18T03:33:20Z".into()),
                    );
                    provider.fail_download_path = Some("/unknown-partial/touched".into());
                }
                let run = svc
                    .perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap();
                assert!(!run.errors.is_empty());
                assert_eq!(if upload { run.uploaded } else { run.downloaded }, 1);
                let after = svc.load_index(&config).unwrap();
                assert_eq!(after.cloud_receipts.len(), 1);
                assert_eq!(
                    serde_json::to_value(after.cloud_receipts.get("keep")).unwrap(),
                    serde_json::to_value(prior.cloud_receipts.get("keep")).unwrap()
                );
                assert!(!after.cloud_receipts.contains_key("touched"));
                assert!(!after.cloud_receipts.contains_key("unknown"));
                svc.connection_identity = Some(Some("known".into()));
                provider.fail_upload_path = None;
                provider.fail_download_path = None;
                assert!(svc
                    .perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap()
                    .errors
                    .is_empty());
                assert!(
                    !svc.load_index(&config)
                        .unwrap()
                        .cloud_receipts
                        .contains_key("unknown"),
                    "identity recovery cannot mint from Identical"
                );
            }
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn partial_errors_preserve_ordinary_and_bidirectional_comparison_baselines() {
        with_scratch_index(async {
            for (direction, update_only) in [
                (CompareDirection::Bidirectional, true),
                (CompareDirection::Bidirectional, false),
                (CompareDirection::LocalToRemote, false),
            ] {
                let root = tempfile::tempdir().unwrap();
                let config = CloudConfig {
                    enabled: true,
                    update_only,
                    preserve_remote_deletes: true,
                    local_folder: root.path().into(),
                    remote_folder: "/ordinary-partial".into(),
                    sync_direction: direction,
                    ..Default::default()
                };
                std::fs::write(root.path().join("good"), b"actual success").unwrap();
                std::fs::write(root.path().join("bad"), b"failure").unwrap();
                let mut prior = SyncIndex::new(
                    root.path().to_string_lossy().into_owned(),
                    config.remote_folder.clone(),
                );
                prior.files.insert(
                    "previous".into(),
                    SyncIndexEntry {
                        size: 7,
                        modified: Some(DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap()),
                        is_dir: false,
                        uploaded_at: None,
                    },
                );
                prior
                    .remote_spellings
                    .insert("previous".into(), "previous-alias".into());
                save_sync_index(&prior).unwrap();
                let mut provider = MemoryProvider {
                    fail_upload_path: Some("/ordinary-partial/bad".into()),
                    ..Default::default()
                };
                let svc = CloudService::new();
                svc.init(config.clone()).await;
                let run = svc
                    .perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap();
                assert!(!run.errors.is_empty());
                assert_eq!(run.uploaded, 1);
                let after = svc.load_index(&config).unwrap();
                assert_eq!(
                    serde_json::to_value(after.files).unwrap(),
                    serde_json::to_value(prior.files).unwrap()
                );
                assert_eq!(after.remote_spellings, prior.remote_spellings);
            }
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_recycled_device_inode_cannot_hit_another_mount_generation_cache() {
        let root = tempfile::tempdir().unwrap();
        let config = CloudConfig {
            local_folder: root.path().into(),
            ..Default::default()
        };
        let current = LocalRootIdentity::read(root.path()).unwrap();
        let mut old = current;
        old.generation = Some([0x7f; 32]);
        assert_ne!(old, current);
        let svc = CloudService::new();
        let stale = serde_json::json!({"kind":"obsolete mount"});
        *svc.receipt_root_cache.lock().unwrap() =
            Some((config.local_folder.clone(), old, Some(stale.clone())));
        assert_ne!(
            svc.cached_receipt_root_key(&config, Some(current)),
            Some(stale)
        );
        *svc.receipt_root_cache.lock().unwrap() =
            Some((config.local_folder.clone(), current, None));
        assert!(
            svc.effective_receipt_scope(&config).is_none(),
            "missing native/mount identity cannot hash into usable authority"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn update_partial_failure_keeps_successful_transfer_receipts_and_aliases() {
        with_scratch_index(async {
            for upload in [true, false] {
                let root = tempfile::tempdir().unwrap();
                let local = root.path().join("café.txt");
                let remote = "/partial/cafe\u{301}.txt";
                let config = CloudConfig {
                    enabled: true,
                    update_only: true,
                    preserve_remote_deletes: false,
                    local_folder: root.path().into(),
                    remote_folder: "/partial".into(),
                    sync_direction: if upload {
                        CompareDirection::LocalToRemote
                    } else {
                        CompareDirection::RemoteToLocal
                    },
                    ..Default::default()
                };
                let mut provider = MemoryProvider::default();
                std::fs::write(
                    &local,
                    if upload {
                        &b"actual source"[..]
                    } else {
                        &b"old"[..]
                    },
                )
                .unwrap();
                filetime::set_file_mtime(
                    &local,
                    filetime::FileTime::from_unix_time(
                        if upload { 1_800_000_000 } else { 1_600_000_000 },
                        0,
                    ),
                )
                .unwrap();
                provider.stored.insert(
                    remote.into(),
                    (
                        (if upload {
                            &b"old"[..]
                        } else {
                            &b"actual source"[..]
                        })
                        .to_vec(),
                        "2023-11-14T22:13:20Z".into(),
                    ),
                );
                if upload {
                    std::fs::write(root.path().join("bad"), b"bad source").unwrap();
                    provider.fail_upload_path = Some("/partial/bad".into());
                } else {
                    provider.put("/partial/bad", b"bad source");
                    provider.fail_stat_path = Some("/partial/bad".into());
                }
                let svc = CloudService::new();
                svc.init(config.clone()).await;
                let result = svc
                    .perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap();
                assert!(!result.errors.is_empty());
                assert_eq!(result.uploaded + result.downloaded, 1, "{result:?}");
                let saved = svc.load_index(&config).unwrap();
                assert!(saved.cloud_receipts.contains_key("café.txt"));
                assert!(!saved.cloud_receipts.contains_key("bad"));
                assert!(!saved.files.contains_key("bad"));
                provider.fail_stat_path = None;
                provider.fail_upload_path = None;
                if upload {
                    std::fs::remove_file(&local).unwrap();
                } else {
                    provider.stored.remove(remote);
                }
                let run = svc
                    .perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap();
                assert!(run.errors.is_empty(), "{:?}", run.errors);
                assert_eq!(run.deleted, 1, "{run:?}");
            }
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn native_root_identity_is_resolved_once_for_a_large_orphan_plan() {
        with_scratch_index(async {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("keep"), b"keep").unwrap();
            let config = CloudConfig {
                enabled: true,
                update_only: true,
                preserve_remote_deletes: false,
                local_folder: root.path().into(),
                remote_folder: "/many".into(),
                sync_direction: CompareDirection::LocalToRemote,
                ..Default::default()
            };
            let mut provider = MemoryProvider::default();
            provider.put("/many/keep", b"keep");
            let mut index = SyncIndex::new(
                root.path().to_string_lossy().into_owned(),
                config.remote_folder.clone(),
            );
            let scope = CloudService::receipt_scope(&config);
            for i in 0..1000 {
                let key = format!("file{i}");
                provider.stored.insert(
                    format!("/many/{key}"),
                    (b"data".to_vec(), "2023-11-14T22:13:20Z".into()),
                );
                let state = SyncIndexEntry {
                    size: 4,
                    modified: Some(DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap()),
                    is_dir: false,
                    uploaded_at: None,
                };
                index.files.insert(key.clone(), state.clone());
                index.cloud_receipts.insert(
                    key.clone(),
                    CloudTransferReceipt {
                        scope: scope.clone(),
                        remote_key: key,
                        destination_side: CompareDirection::LocalToRemote,
                        utc_remote_time: true,
                        destination: state,
                    },
                );
            }
            save_sync_index(&index).unwrap();
            let svc = CloudService::new();
            svc.init(config).await;
            let preview = svc
                .preview_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert!(!preview.errors.is_empty(), "mass gate must remain active");
            assert_eq!(
                svc.native_root_reads
                    .load(std::sync::atomic::Ordering::Relaxed),
                1
            );
        });
    }

    #[test]
    fn stable_profile_and_root_identity_ignore_display_and_compression_level() {
        let mut config = CloudConfig {
            server_profile_id: Some("stable-id".into()),
            server_profile: "old label".into(),
            compress_enabled: true,
            ..Default::default()
        };
        let root =
            Some(serde_json::json!({"kind":"native-test","volume":"uuid","file":"native-file-id"}));
        let before = CloudService::receipt_scope_with_key(&config, root.clone());
        config.server_profile = "renamed".into();
        config.compress_level = 9;
        assert_eq!(
            CloudService::receipt_scope_with_key(&config, root.clone()),
            before
        );
        let other = Some(
            serde_json::json!({"kind":"native-test","volume":"other-uuid","file":"native-file-id"}),
        );
        assert_ne!(CloudService::receipt_scope_with_key(&config, other), before);
        let other =
            Some(serde_json::json!({"kind":"native-test","volume":"uuid","file":"other-file-id"}));
        assert_ne!(CloudService::receipt_scope_with_key(&config, other), before);
    }

    /// Exact recorded alias identity survives source disappearance without borrowing other names.
    #[test]
    #[cfg(target_os = "linux")]
    fn update_transferred_alias_receipt_survives_source_removal_and_checks_recreation() {
        with_scratch_index(async {
            for upload in [true, false] {
                for recreate in [true, false] {
                    let root = tempfile::tempdir().unwrap();
                    let config = CloudConfig {
                        enabled: true,
                        update_only: true,
                        preserve_remote_deletes: false,
                        sync_direction: if upload {
                            CompareDirection::LocalToRemote
                        } else {
                            CompareDirection::RemoteToLocal
                        },
                        local_folder: root.path().into(),
                        remote_folder: "/alias".into(),
                        ..Default::default()
                    };
                    let local = root.path().join("café.txt");
                    let remote = "/alias/cafe\u{301}.txt";
                    std::fs::write(
                        &local,
                        if upload {
                            &b"source contents"[..]
                        } else {
                            &b"old"[..]
                        },
                    )
                    .unwrap();
                    filetime::set_file_mtime(
                        &local,
                        filetime::FileTime::from_unix_time(
                            if upload { 1_700_000_000 } else { 1_600_000_000 },
                            0,
                        ),
                    )
                    .unwrap();
                    let mut provider = MemoryProvider::default();
                    provider.stored.insert(
                        remote.into(),
                        (
                            (if upload {
                                &b"old"[..]
                            } else {
                                &b"source contents"[..]
                            })
                            .to_vec(),
                            DateTime::<Utc>::from_timestamp(
                                if upload { 1_600_000_000 } else { 1_700_000_000 },
                                0,
                            )
                            .unwrap()
                            .to_rfc3339(),
                        ),
                    );
                    std::fs::write(root.path().join("keep"), b"keep").unwrap();
                    provider.put("/alias/keep", b"keep");
                    let svc = CloudService::new();
                    svc.init(config.clone()).await;
                    let first = svc
                        .perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert!(first.errors.is_empty(), "{:?}", first.errors);
                    assert!(svc
                        .load_index(&config)
                        .unwrap()
                        .cloud_receipts
                        .contains_key("café.txt"));
                    if upload {
                        std::fs::remove_file(&local).unwrap();
                    } else {
                        provider.stored.remove(remote);
                    }
                    let plan = svc
                        .build_sync_plan_with_provider(&mut provider, &config, false)
                        .await
                        .unwrap();
                    let row = plan
                        .comparisons
                        .iter()
                        .find(|row| row.relative_path.contains("caf"))
                        .unwrap();
                    assert_eq!(
                        svc.resolve_action(&config, row, plan.update_policy),
                        if upload {
                            SyncAction::DeleteRemote
                        } else {
                            SyncAction::DeleteLocal
                        },
                        "{row:?}"
                    );
                    if recreate {
                        if upload {
                            std::fs::write(&local, b"recreated local source").unwrap();
                        } else {
                            provider.put(remote, b"recreated remote source");
                        }
                        assert_eq!(
                            svc.process_comparison_with_provider(
                                &mut provider,
                                &config,
                                row,
                                plan.update_policy
                            )
                            .await
                            .unwrap(),
                            SyncAction::Skip
                        );
                        assert!(local.exists() && provider.stored.contains_key(remote));
                    } else {
                        let result = svc
                            .perform_full_sync_with_provider(&mut provider)
                            .await
                            .unwrap();
                        assert_eq!(result.deleted, 1, "{:?}", result);
                        assert!(!svc
                            .load_index(&config)
                            .unwrap()
                            .files
                            .contains_key("café.txt"));
                        assert!(!svc
                            .load_index(&config)
                            .unwrap()
                            .cloud_receipts
                            .contains_key("café.txt"));
                    }
                }
            }
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn update_failed_overwrite_revokes_old_receipt_without_advancing_comparison_history() {
        with_scratch_index(async {
            let root = tempfile::tempdir().unwrap();
            let config = CloudConfig {
                enabled: true,
                update_only: true,
                preserve_remote_deletes: false,
                sync_direction: CompareDirection::LocalToRemote,
                local_folder: root.path().into(),
                remote_folder: "/failed".into(),
                ..Default::default()
            };
            let mut provider = MemoryProvider::default();
            for key in ["photo", "keep"] {
                std::fs::write(root.path().join(key), b"original").unwrap();
            }
            let svc = CloudService::new();
            svc.init(config.clone()).await;
            assert!(svc
                .perform_full_sync_with_provider(&mut provider)
                .await
                .unwrap()
                .errors
                .is_empty());
            let before = svc.load_index(&config).unwrap();
            assert!(before.cloud_receipts.contains_key("photo"));
            std::fs::write(root.path().join("photo"), b"edited source").unwrap();
            filetime::set_file_mtime(
                root.path().join("photo"),
                filetime::FileTime::from_unix_time(2_000_000_000, 0),
            )
            .unwrap();
            provider.fail_upload = true;
            let failed = svc
                .perform_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert!(!failed.errors.is_empty());
            let saved = svc.load_index(&config).unwrap();
            assert!(!saved.cloud_receipts.contains_key("photo"));
            assert_eq!(
                serde_json::to_value(&saved.files).unwrap(),
                serde_json::to_value(&before.files).unwrap()
            );
            provider.fail_upload = false;
            std::fs::remove_file(root.path().join("photo")).unwrap();
            let next = svc
                .perform_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert_eq!(next.deleted, 0);
            assert_eq!(provider.stored["/failed/photo"].0, b"original");
            assert!(next
                .skipped_details
                .iter()
                .any(|entry| entry.path == "photo" && entry.needs_review));
        });
    }

    /// S3's LIST upload clock and Swift fractional list dates cannot invalidate a stable STAT snapshot.
    #[test]
    #[cfg(target_os = "linux")]
    fn update_plan_and_executor_use_the_same_provider_stat_clock() {
        with_scratch_index(async {
            for listed in ["2030-01-01T00:00:00Z", "2023-11-14T22:13:20.123456Z"] {
                for newer_destination in [true, false] {
                    let root = tempfile::tempdir().unwrap();
                    let config = CloudConfig {
                        enabled: true,
                        update_only: true,
                        sync_direction: CompareDirection::RemoteToLocal,
                        local_folder: root.path().into(),
                        remote_folder: "/clock".into(),
                        ..Default::default()
                    };
                    let mut provider = MemoryProvider {
                        list_time: Some(listed.into()),
                        ..Default::default()
                    };
                    provider.stored.insert(
                        "/clock/photo".into(),
                        (
                            b"remote source".to_vec(),
                            DateTime::<Utc>::from_timestamp(1_700_000_000, 0)
                                .unwrap()
                                .to_rfc3339(),
                        ),
                    );
                    std::fs::write(root.path().join("photo"), b"local destination").unwrap();
                    filetime::set_file_mtime(
                        root.path().join("photo"),
                        filetime::FileTime::from_unix_time(
                            if newer_destination {
                                1_800_000_000
                            } else {
                                1_600_000_000
                            },
                            0,
                        ),
                    )
                    .unwrap();
                    let svc = CloudService::new();
                    svc.init(config.clone()).await;
                    let preview = svc
                        .preview_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    let result = svc
                        .perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert!(result.errors.is_empty(), "{:?}", result.errors);
                    assert_eq!(preview.downloaded, u32::from(!newer_destination));
                    assert_eq!(result.downloaded, preview.downloaded);
                    assert_eq!(
                        std::fs::read(root.path().join("photo")).unwrap(),
                        if newer_destination {
                            &b"local destination"[..]
                        } else {
                            &b"remote source"[..]
                        }
                    );
                }
            }
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn one_way_update_preview_and_real_run_agree_without_a_baseline() {
        with_scratch_index(async {
            for direction in [
                CompareDirection::LocalToRemote,
                CompareDirection::RemoteToLocal,
            ] {
                let local = tempfile::tempdir().unwrap();
                let upload = direction == CompareDirection::LocalToRemote;
                let mut provider = MemoryProvider::default();
                let remote = "/update-only";
                let write_local = |name: &str, bytes: &[u8], secs: u64| {
                    let path = local.path().join(name);
                    std::fs::write(&path, bytes).unwrap();
                    std::fs::File::open(&path)
                        .unwrap()
                        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(secs))
                        .unwrap();
                };
                // Local and remote deliberately contain different bytes and
                // lengths; both copies of the newer destination must survive.
                write_local(
                    "keep",
                    b"local version",
                    if upload { 1_700_000_000 } else { 1_700_001_000 },
                );
                provider.stored.insert(
                    format!("{remote}/keep"),
                    (
                        b"remote newer or older version".to_vec(),
                        DateTime::<Utc>::from_timestamp(
                            if upload { 1_700_001_000 } else { 1_700_000_000 },
                            0,
                        )
                        .unwrap()
                        .to_rfc3339(),
                    ),
                );
                if upload {
                    write_local("new", b"source only", 1_700_000_000);
                    provider.put(&format!("{remote}/orphan"), b"destination only");
                } else {
                    provider.put(&format!("{remote}/new"), b"source only");
                    write_local("orphan", b"destination only", 1_700_000_000);
                }
                let before_local = std::fs::read(local.path().join("keep")).unwrap();
                let before_remote = provider.stored[&format!("{remote}/keep")].0.clone();
                let svc = CloudService::new();
                svc.init(CloudConfig {
                    enabled: true,
                    local_folder: local.path().into(),
                    remote_folder: remote.into(),
                    sync_direction: direction,
                    update_only: true,
                    preserve_remote_deletes: false,
                    // The preference must not reverse the one-way direction.
                    conflict_strategy: if upload {
                        ConflictStrategy::PreferRemote
                    } else {
                        ConflictStrategy::PreferLocal
                    },
                    ..Default::default()
                })
                .await;
                for round in 0..3 {
                    let preview = svc
                        .preview_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert_eq!(preview.deleted, 0);
                    assert_eq!(
                        (preview.uploaded, preview.downloaded),
                        if round > 0 {
                            (0, 0)
                        } else if upload {
                            (1, 0)
                        } else {
                            (0, 1)
                        }
                    );
                    assert_eq!(
                        provider.stored.contains_key(&format!("{remote}/orphan")),
                        upload
                    );
                    assert_eq!(local.path().join("orphan").exists(), !upload);
                    let result = svc
                        .perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert!(result.errors.is_empty(), "{:?}", result.errors);
                    assert_eq!(
                        (result.uploaded, result.downloaded, result.deleted),
                        (preview.uploaded, preview.downloaded, preview.deleted)
                    );
                    assert_eq!(
                        std::fs::read(local.path().join("keep")).unwrap(),
                        before_local
                    );
                    assert_eq!(provider.stored[&format!("{remote}/keep")].0, before_remote);
                    assert_eq!(local.path().join("orphan").exists(), !upload);
                    assert_eq!(
                        provider.stored.contains_key(&format!("{remote}/orphan")),
                        upload
                    );
                    let warnings: Vec<_> = result
                        .skipped_details
                        .iter()
                        .filter(|entry| entry.needs_review)
                        .collect();
                    assert_eq!(warnings.len(), 1);
                    assert_eq!(warnings[0].path, "orphan");
                    assert!(warnings[0].reason.contains("AeroSync"));
                    assert!(preview
                        .skipped_details
                        .iter()
                        .any(|entry| entry.path == "orphan" && entry.needs_review));
                    assert!(!svc
                        .load_index(&*svc.config.read().await)
                        .unwrap()
                        .files
                        .contains_key("orphan"));
                    if upload {
                        assert_eq!(provider.stored[&format!("{remote}/new")].0, b"source only");
                    } else {
                        assert_eq!(
                            std::fs::read(local.path().join("new")).unwrap(),
                            b"source only"
                        );
                    }
                }
            }
        });
    }

    #[test]
    fn update_delete_limit_counts_files_and_keeps_the_small_folder_allowance() {
        assert!(!CloudService::update_delete_safety_trips(9, 9));
        assert!(!CloudService::update_delete_safety_trips(10, 20));
        assert!(CloudService::update_delete_safety_trips(10, 19));
        assert!(CloudService::update_delete_safety_trips(20, 20));
    }

    // The scratch-index fixture isolates the application data root through XDG only on Linux.
    #[test]
    #[cfg(target_os = "linux")]
    fn one_way_update_first_cycle_untracked_files_stay_out_of_the_baseline() {
        with_scratch_index(async {
            for direction in [
                CompareDirection::LocalToRemote,
                CompareDirection::RemoteToLocal,
            ] {
                let local = tempfile::tempdir().unwrap();
                let upload = direction == CompareDirection::LocalToRemote;
                let mut provider = MemoryProvider::default();
                let remote = "/mass-update-only";
                for i in 0..3 {
                    let name = format!("source-{i}");
                    if upload {
                        std::fs::write(local.path().join(name), b"source").unwrap();
                    } else {
                        provider.put(&format!("{remote}/{name}"), b"source");
                    }
                }
                for i in 0..20 {
                    let name = format!("orphan-{i}");
                    if upload {
                        provider.put(&format!("{remote}/{name}"), b"destination");
                    } else {
                        std::fs::write(local.path().join(name), b"destination").unwrap();
                    }
                }
                let svc = CloudService::new();
                svc.init(CloudConfig {
                    enabled: true,
                    local_folder: local.path().into(),
                    remote_folder: remote.into(),
                    sync_direction: direction,
                    update_only: true,
                    preserve_remote_deletes: false,
                    ..Default::default()
                })
                .await;
                // Repeating the cycle cannot convert refused orphans into
                // deletion authority via a newly saved baseline.
                for _ in 0..2 {
                    let preview = svc
                        .preview_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert_eq!(preview.deleted, 0);
                    assert!(preview.errors.is_empty(), "{:?}", preview.errors);
                    assert_eq!(
                        preview
                            .skipped_details
                            .iter()
                            .filter(|entry| entry.needs_review)
                            .count(),
                        20
                    );
                    let result = svc
                        .perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert_eq!(result.deleted, 0);
                    assert_eq!(result.errors, preview.errors);
                    assert_eq!(
                        result
                            .skipped_details
                            .iter()
                            .filter(|entry| entry.needs_review)
                            .count(),
                        20
                    );
                    assert!(!svc
                        .load_index(&*svc.config.read().await)
                        .unwrap()
                        .files
                        .keys()
                        .any(|name| name.starts_with("orphan-")));
                    for i in 0..20 {
                        let name = format!("orphan-{i}");
                        let bytes = if upload {
                            provider.stored[&format!("{remote}/{name}")].0.clone()
                        } else {
                            std::fs::read(local.path().join(name)).unwrap()
                        };
                        assert_eq!(bytes, b"destination");
                    }
                }
            }
        });
    }

    // The scratch-index fixture relocates the application data root only on Linux.
    #[test]
    #[cfg(target_os = "linux")]
    fn one_way_update_baselined_deletes_respect_the_mass_limit() {
        with_scratch_index(async {
            for direction in [
                CompareDirection::LocalToRemote,
                CompareDirection::RemoteToLocal,
            ] {
                for removed in [1, 20] {
                    let local = tempfile::tempdir().unwrap();
                    let upload = direction == CompareDirection::LocalToRemote;
                    let remote = "/baselined-update";
                    let mut provider = MemoryProvider::default();
                    for i in 0..removed + 3 {
                        let name = format!("file-{i}");
                        if upload {
                            std::fs::write(local.path().join(name), b"synced bytes").unwrap();
                        } else {
                            provider.put(&format!("{remote}/{name}"), b"synced bytes");
                        }
                    }
                    let svc = CloudService::new();
                    let config = CloudConfig {
                        enabled: true,
                        local_folder: local.path().into(),
                        remote_folder: remote.into(),
                        sync_direction: direction,
                        update_only: true,
                        preserve_remote_deletes: false,
                        ..Default::default()
                    };
                    svc.init(config.clone()).await;
                    let first = svc
                        .perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert!(first.errors.is_empty(), "{:?}", first.errors);
                    let original_receipts =
                        serde_json::to_value(svc.load_index(&config).unwrap().cloud_receipts)
                            .unwrap();
                    let original_baseline =
                        serde_json::to_value(svc.load_index(&config).unwrap().files).unwrap();
                    assert_eq!(original_baseline.as_object().unwrap().len(), removed + 3);
                    for i in 0..removed {
                        let name = format!("file-{i}");
                        if upload {
                            std::fs::remove_file(local.path().join(name)).unwrap();
                        } else {
                            provider.stored.remove(&format!("{remote}/{name}"));
                        }
                    }
                    let preview = svc
                        .preview_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    let result = svc
                        .perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    let expected = if removed == 1 { 1 } else { 0 };
                    assert_eq!(preview.deleted, expected);
                    assert_eq!(result.deleted, expected);
                    assert_eq!(preview.errors, result.errors);
                    assert_eq!(result.errors.is_empty(), removed == 1);
                    assert!(result
                        .skipped_details
                        .iter()
                        .all(|entry| !entry.needs_review));
                    for i in 0..removed {
                        let name = format!("file-{i}");
                        let present = if upload {
                            provider.stored.contains_key(&format!("{remote}/{name}"))
                        } else {
                            local.path().join(name).exists()
                        };
                        assert_eq!(present, removed == 20);
                    }
                    if removed == 20 {
                        assert_eq!(
                            serde_json::to_value(svc.load_index(&config).unwrap().files).unwrap(),
                            original_baseline
                        );
                        assert_eq!(
                            serde_json::to_value(svc.load_index(&config).unwrap().cloud_receipts)
                                .unwrap(),
                            original_receipts
                        );
                        assert!(result.errors[0].contains("blocked 20 deletes"));
                        let again = svc
                            .perform_full_sync_with_provider(&mut provider)
                            .await
                            .unwrap();
                        assert_eq!(again.deleted, 0);
                        assert!(again
                            .skipped_details
                            .iter()
                            .all(|entry| !entry.needs_review));
                        assert_eq!(
                            serde_json::to_value(svc.load_index(&config).unwrap().files).unwrap(),
                            original_baseline
                        );
                        assert_eq!(
                            serde_json::to_value(svc.load_index(&config).unwrap().cloud_receipts)
                                .unwrap(),
                            original_receipts
                        );
                    }
                }
            }
        });
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn one_way_update_incomplete_first_scan_does_not_delete_visible_orphans() {
        with_scratch_index(async {
            let local = tempfile::tempdir().unwrap();
            std::fs::write(local.path().join("new"), b"source only").unwrap();
            let mut provider = MemoryProvider {
                broken: Some("/update-only/broken".into()),
                ..Default::default()
            };
            provider.put("/update-only/broken/invisible", b"cannot list this folder");
            provider.put("/update-only/orphan", b"visible destination only");
            let svc = CloudService::new();
            svc.init(CloudConfig {
                enabled: true,
                local_folder: local.path().into(),
                remote_folder: "/update-only".into(),
                sync_direction: CompareDirection::LocalToRemote,
                update_only: true,
                preserve_remote_deletes: false,
                ..Default::default()
            })
            .await;
            let preview = svc
                .preview_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert_eq!(preview.deleted, 0);
            let result = svc
                .perform_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert_eq!(result.deleted, 0);
            assert!(provider.stored.contains_key("/update-only/orphan"));
            assert!(provider
                .stored
                .contains_key("/update-only/broken/invisible"));
        });
    }

    fn gone_config(local: &std::path::Path) -> CloudConfig {
        CloudConfig {
            enabled: true,
            local_folder: local.to_path_buf(),
            remote_folder: GONE_REMOTE.into(),
            sync_direction: CompareDirection::Bidirectional,
            ..Default::default()
        }
    }

    /// Sync `names` up in a first clean cycle, then delete `gone.txt` here and,
    /// as another device would, on the remote.
    async fn synced_then_gone_on_both(
        svc: &CloudService,
        provider: &mut MemoryProvider,
        local: &std::path::Path,
        names: &[&str],
    ) {
        for name in names {
            let path = local.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, name).unwrap();
        }
        svc.init(gone_config(local)).await;
        let first = svc.perform_full_sync_with_provider(provider).await.unwrap();
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(first.file_details.len(), names.len());
        std::fs::remove_file(local.join("gone.txt")).unwrap();
        provider.stored.remove(&format!("{GONE_REMOTE}/gone.txt"));
    }

    /// A file deleted here and on another device between two cycles is in
    /// neither scan, so no comparison named it and its baseline entry stayed
    /// for good. A file created later at that path then read as synced before
    /// and was deleted on the other side: a new local file deleted locally, a
    /// new remote one on the remote. The clean cycle that sees the path gone
    /// from both complete scans forgets it, and each new file is copied.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_path_gone_from_both_sides_is_forgotten_so_a_new_file_there_is_copied() {
        with_scratch_index(async {
            for new_on_remote in [false, true] {
                let local = tempfile::tempdir().unwrap();
                let mut provider = MemoryProvider::default();
                let svc = CloudService::new();
                synced_then_gone_on_both(
                    &svc,
                    &mut provider,
                    local.path(),
                    &["keep.txt", "gone.txt"],
                )
                .await;

                let second = svc
                    .perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap();
                assert!(second.errors.is_empty(), "{:?}", second.errors);
                let baseline = svc.load_index(&gone_config(local.path())).unwrap();
                assert!(baseline.files.contains_key("keep.txt"));
                assert!(
                    !baseline.files.contains_key("gone.txt"),
                    "a path gone from both sides stays in the baseline"
                );

                let remote_path = format!("{GONE_REMOTE}/gone.txt");
                if new_on_remote {
                    provider.put(&remote_path, b"a new remote file");
                } else {
                    std::fs::write(local.path().join("gone.txt"), b"a new local file").unwrap();
                }
                let third = svc
                    .perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap();
                assert!(third.errors.is_empty(), "{:?}", third.errors);
                assert_eq!(third.deleted, 0, "new on the remote: {new_on_remote}");
                let expected: &[u8] = if new_on_remote {
                    b"a new remote file"
                } else {
                    b"a new local file"
                };
                assert_eq!(
                    std::fs::read(local.path().join("gone.txt")).unwrap(),
                    expected
                );
                assert_eq!(provider.stored[&remote_path].0, expected);
            }
        });
    }

    /// Audit 4.2.5 A03: a remote folder whose listing fails while writes
    /// still work (a read-denied folder, a transient error) hides what it
    /// holds. Left in the plan, each local file under it read as LocalOnly
    /// and went up over the remote copy the scan never saw, with or without a
    /// baseline. Both sides of that folder stay out of the cycle: zero writes
    /// into it, its baseline entries kept, the rest of the tree synced. A scan
    /// that cannot list the sync folder itself refuses the cycle.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_remote_folder_that_did_not_list_gets_no_upload() {
        with_scratch_index(async {
            let hidden = format!("{GONE_REMOTE}/blocked/report.txt");
            let added = format!("{GONE_REMOTE}/blocked/new.txt");
            for with_baseline in [false, true] {
                let local = tempfile::tempdir().unwrap();
                std::fs::create_dir_all(local.path().join("blocked")).unwrap();
                std::fs::write(local.path().join("blocked/report.txt"), b"local copy").unwrap();
                std::fs::write(local.path().join("keep.txt"), b"keep").unwrap();
                let mut provider = MemoryProvider::default();
                let svc = CloudService::new();
                svc.init(gone_config(local.path())).await;
                if with_baseline {
                    let first = svc
                        .perform_full_sync_with_provider(&mut provider)
                        .await
                        .unwrap();
                    assert!(first.errors.is_empty(), "{:?}", first.errors);
                }
                // Another device writes a newer remote copy; then the folder
                // stops listing, and a new local file appears under it.
                provider.put(&hidden, b"newer remote copy");
                provider.broken = Some(format!("{GONE_REMOTE}/blocked"));
                std::fs::write(local.path().join("blocked/new.txt"), b"new").unwrap();
                std::fs::write(local.path().join("later.txt"), b"later").unwrap();

                let run = svc
                    .perform_full_sync_with_provider(&mut provider)
                    .await
                    .unwrap();
                assert_eq!(
                    provider.stored[&hidden].0, b"newer remote copy",
                    "baseline {with_baseline}: the unseen remote copy was overwritten"
                );
                assert!(
                    !provider.stored.contains_key(&added),
                    "baseline {with_baseline}: an upload reached the folder that did not list"
                );
                assert_eq!(
                    std::fs::read(local.path().join("blocked/report.txt")).unwrap(),
                    b"local copy"
                );
                assert!(
                    provider
                        .stored
                        .contains_key(&format!("{GONE_REMOTE}/later.txt")),
                    "baseline {with_baseline}: the rest of the tree still syncs ({:?})",
                    run.errors
                );
                if with_baseline {
                    let baseline = svc.load_index(&gone_config(local.path())).unwrap();
                    assert!(baseline.files.contains_key("blocked/report.txt"));
                }
            }

            // The sync folder itself does not list: nothing to bound around,
            // so the cycle is refused and writes nothing.
            let local = tempfile::tempdir().unwrap();
            std::fs::write(local.path().join("a.txt"), b"a").unwrap();
            let mut provider = MemoryProvider {
                broken: Some(GONE_REMOTE.to_string()),
                ..MemoryProvider::default()
            };
            provider.put(&format!("{GONE_REMOTE}/a.txt"), b"remote a");
            let svc = CloudService::new();
            svc.init(gone_config(local.path())).await;
            let refused = svc.perform_full_sync_with_provider(&mut provider).await;
            assert!(refused.is_err(), "{refused:?}");
            assert_eq!(
                provider.stored[&format!("{GONE_REMOTE}/a.txt")].0,
                b"remote a"
            );
        });
    }

    /// Both sides emptied (the folder cleared here and on the remote): the
    /// mass-delete gate trips on the empty sides, and the cycle still forgets
    /// what is gone from both. Kept, the old names would read as synced once
    /// the folder fills again, and a new file reusing one would be deleted.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_folder_emptied_on_both_sides_leaves_an_empty_baseline() {
        with_scratch_index(async {
            let local = tempfile::tempdir().unwrap();
            let mut provider = MemoryProvider::default();
            let svc = CloudService::new();
            synced_then_gone_on_both(
                &svc,
                &mut provider,
                local.path(),
                &["gone.txt", "sub/a.txt"],
            )
            .await;
            std::fs::remove_dir_all(local.path().join("sub")).unwrap();
            provider.stored.clear();
            let second = svc
                .perform_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert!(second.errors.is_empty(), "{:?}", second.errors);
            let baseline = svc.load_index(&gone_config(local.path())).unwrap();
            assert!(baseline.files.is_empty(), "{:?}", baseline.files.keys());
        });
    }

    /// Requested 79: AeroCloud records in the baseline the profile it syncs
    /// with, so that a Proton Drive switch of that profile can move it, and
    /// does not take as its baseline an index another profile wrote for the
    /// same pair of folders. `aeroftp-cli aerocloud sync` runs this engine too
    /// (`sync_one_config`).
    #[cfg(target_os = "linux")]
    #[test]
    fn the_baseline_records_the_profile_and_ignores_another_profiles() {
        with_scratch_index(async {
            let local = tempfile::tempdir().unwrap();
            std::fs::write(local.path().join("a.txt"), b"a").unwrap();
            let mut provider = MemoryProvider::default();
            let svc = CloudService::new();
            let config = CloudConfig {
                server_profile_id: Some("srv_p1".into()),
                ..gone_config(local.path())
            };
            svc.init(config.clone()).await;
            let first = svc
                .perform_full_sync_with_provider(&mut provider)
                .await
                .unwrap();
            assert!(first.errors.is_empty(), "{:?}", first.errors);
            let baseline = svc.load_index(&config).unwrap();
            assert_eq!(baseline.profile_id.as_deref(), Some("srv_p1"));
            let other = CloudConfig {
                server_profile_id: Some("srv_p2".into()),
                ..config.clone()
            };
            assert!(svc.load_index(&other).is_none());
            let unbound = CloudConfig {
                server_profile_id: None,
                ..config
            };
            assert!(svc.load_index(&unbound).is_some());
        });
    }

    /// A path the scans leave out by design is absent from both of them and is
    /// not gone: `.aeroignore`, the exclude list, an excluded folder (and what
    /// is under it), a local link or what sits behind one. Only the plain
    /// path the scans would list is forgotten.
    #[test]
    fn a_path_the_scans_leave_out_keeps_its_entry() {
        let local = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(local.path().join(".aeroignore"), "ignored/\n").unwrap();
        let keys = [
            ("gone.txt", false),
            ("debug.log", false),
            ("cache", true),
            ("cache/blob.bin", false),
            ("private", true),
            ("private/notes.txt", false),
            ("ignored/a.txt", false),
        ];
        #[cfg(unix)]
        {
            let target = outside.path();
            std::fs::write(target.join("f"), b"f").unwrap();
            std::os::unix::fs::symlink(target, local.path().join("linked")).unwrap();
            std::os::unix::fs::symlink(target.join("f"), local.path().join("link.txt")).unwrap();
        }
        let links: &[(&str, bool)] = if cfg!(unix) {
            &[("linked/a.txt", false), ("link.txt", false)]
        } else {
            &[]
        };
        let config = CloudConfig {
            local_folder: local.path().to_path_buf(),
            exclude_patterns: vec!["*.log".to_string(), "cache".to_string()],
            excluded_folders: vec!["/private/".to_string()],
            ..Default::default()
        };
        let index = SyncIndex {
            profile_id: None,
            version: SYNC_INDEX_VERSION,
            last_sync: Utc::now(),
            local_path: local.path().to_string_lossy().into_owned(),
            remote_path: GONE_REMOTE.to_string(),
            files: keys
                .iter()
                .chain(links)
                .map(|(key, is_dir)| {
                    let entry = SyncIndexEntry {
                        size: 1,
                        modified: None,
                        is_dir: *is_dir,
                        uploaded_at: None,
                    };
                    (key.to_string(), entry)
                })
                .collect(),
            remote_spellings: Default::default(),
            cloud_receipts: Default::default(),
            cloud_remote_metadata: Default::default(),
            unverified_keys: Default::default(),
        };
        let gone = CloudService::gone_on_both(
            &config,
            &local.path().to_string_lossy(),
            &HashMap::new(),
            &HashMap::new(),
            Some(&index),
        )
        .unwrap();
        assert_eq!(gone, vec!["gone.txt".to_string()]);
    }

    /// A scan that did not see the whole tree cannot say a path is gone: the
    /// cycle forgets nothing, whichever side was incomplete.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_incomplete_scan_forgets_nothing() {
        use std::os::unix::fs::PermissionsExt;
        with_scratch_index(async {
            for remote_incomplete in [true, false] {
                let local = tempfile::tempdir().unwrap();
                let mut provider = MemoryProvider::default();
                let svc = CloudService::new();
                synced_then_gone_on_both(
                    &svc,
                    &mut provider,
                    local.path(),
                    &["keep.txt", "gone.txt", "sub/a.txt"],
                )
                .await;
                let drafts = local.path().join("drafts");
                if remote_incomplete {
                    provider.broken = Some(format!("{GONE_REMOTE}/sub"));
                } else {
                    // Listed but not searchable: its entries cannot be stated.
                    std::fs::create_dir(&drafts).unwrap();
                    std::fs::write(drafts.join("note.txt"), b"x").unwrap();
                    std::fs::set_permissions(&drafts, std::fs::Permissions::from_mode(0o400))
                        .unwrap();
                }
                let second = svc.perform_full_sync_with_provider(&mut provider).await;
                if !remote_incomplete {
                    std::fs::set_permissions(&drafts, std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                }
                let second = second.unwrap();
                assert!(second.errors.is_empty(), "{:?}", second.errors);
                let baseline = svc.load_index(&gone_config(local.path())).unwrap();
                assert!(
                    baseline.files.contains_key("gone.txt"),
                    "forgotten on an incomplete scan (remote incomplete: {remote_incomplete})"
                );
            }
        });
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
                            profile_id: None,
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
                                    uploaded_at: None,
                                },
                            )]),
                            remote_spellings: Default::default(),
                            cloud_receipts: Default::default(),
                            cloud_remote_metadata: Default::default(),
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

#[cfg(test)]
mod walk_scope_tests {
    use super::*;

    /// The AeroCloud remote scan builds each folder's path from its parent
    /// and the name the listing gives, and its visited set compared those
    /// strings: a name that leads back to the folder (`../d1`) or above it
    /// (`d1/../..`) or out of it (`up/../../elsewhere`) is a new string each
    /// time, so the scan went down to its depth cap or indexed `/top.txt`
    /// and `/elsewhere/big.bin` as files of the sync folder. It indexes each
    /// path under the folder once, and a folder it does not follow makes
    /// the scan incomplete, which keeps deletes from propagating.
    #[tokio::test]
    async fn the_cloud_scan_indexes_each_path_under_the_folder_once() {
        let local = tempfile::tempdir().expect("temp dir");
        for shape in crate::sync_core::scan::tests::stray_listings() {
            let case = shape.case;
            let mut provider = shape.provider();
            let config = CloudConfig {
                local_folder: local.path().to_path_buf(),
                remote_folder: "/root".into(),
                ..Default::default()
            };
            let svc = CloudService::new();
            let RemoteScan {
                files,
                complete,
                boundaries,
            } = svc
                .scan_remote_folder_with_provider(&mut provider, &config)
                .await
                .unwrap_or_else(|e| panic!("{case}: {e}"));
            let mut indexed: Vec<String> = files.keys().cloned().collect();
            indexed.sort();
            let mut expected: Vec<String> = shape
                .files
                .iter()
                .chain(shape.folders)
                .map(|p| p.to_string())
                .collect();
            expected.sort();
            assert_eq!(indexed, expected, "{case}");
            assert_eq!(complete, shape.refused == 0, "{case}");
            // A folder the walk refused is named, so a cycle stays out of it
            // instead of reading what it holds as absent (audit 4.2.5 A03).
            assert_eq!(boundaries.unseen.is_empty(), shape.refused == 0, "{case}");
            assert!(boundaries.unbounded.is_none(), "{case}");
        }
    }
}
