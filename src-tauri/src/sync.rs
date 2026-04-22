// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

// AeroFTP Sync Module
// File comparison and synchronization logic

use crate::providers::{ProviderError, StorageProvider};
use crate::sync_core::scan::{scan_local_tree, scan_remote_tree, ScanOptions};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Mutex to prevent concurrent journal writes from corrupting the file (M38)
static JOURNAL_WRITE_LOCK: std::sync::LazyLock<Mutex<()>> =
    std::sync::LazyLock::new(|| Mutex::new(()));

/// Tolerance for timestamp comparison (seconds)
/// Accounts for filesystem and timezone differences
const TIMESTAMP_TOLERANCE_SECS: i64 = 30;

/// Status of a file comparison
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    /// Files are identical (same size and timestamp within tolerance)
    Identical,
    /// Local file is newer -> should upload
    LocalNewer,
    /// Remote file is newer -> should download
    RemoteNewer,
    /// File exists only locally -> upload or ignore
    LocalOnly,
    /// File exists only remotely -> download or ignore
    RemoteOnly,
    /// Both files modified since last sync -> user decision needed
    Conflict,
    /// Same timestamp but different size -> likely checksum needed
    SizeMismatch,
}

/// Information about a file (local or remote)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
    pub is_dir: bool,
    pub checksum: Option<String>,
}

/// Result of comparing a single file/directory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileComparison {
    pub relative_path: String,
    pub status: SyncStatus,
    pub local_info: Option<FileInfo>,
    pub remote_info: Option<FileInfo>,
    pub is_dir: bool,
    /// Human-readable explanation of why this file needs syncing
    pub sync_reason: String,
    /// True if this file existed in the sync index from a previous sync.
    /// For bidirectional sync: a local_only/remote_only file that was previously synced
    /// means the OTHER side deleted it (vs being a genuinely new file).
    #[serde(default)]
    pub previously_synced: bool,
}

/// Options for comparison
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareOptions {
    /// Compare by timestamp
    pub compare_timestamp: bool,
    /// Compare by size
    pub compare_size: bool,
    /// Compare by checksum (slower but accurate)
    pub compare_checksum: bool,
    /// Patterns to exclude (e.g., "node_modules", ".git")
    pub exclude_patterns: Vec<String>,
    /// Direction of comparison
    pub direction: CompareDirection,
    /// Minimum file size in bytes (skip smaller files)
    #[serde(default)]
    pub min_size: Option<u64>,
    /// Maximum file size in bytes (skip larger files)
    #[serde(default)]
    pub max_size: Option<u64>,
    /// Minimum file age in seconds (skip newer files)
    #[serde(default)]
    pub min_age_secs: Option<u64>,
    /// Maximum file age in seconds (skip older files)
    #[serde(default)]
    pub max_age_secs: Option<u64>,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            compare_timestamp: true,
            compare_size: true,
            compare_checksum: false,
            exclude_patterns: vec![
                "node_modules".to_string(),
                ".git".to_string(),
                ".DS_Store".to_string(),
                "Thumbs.db".to_string(),
                "__pycache__".to_string(),
                "*.pyc".to_string(),
                ".env".to_string(),
                "target".to_string(),
            ],
            direction: CompareDirection::Bidirectional,
            min_size: None,
            max_size: None,
            min_age_secs: None,
            max_age_secs: None,
        }
    }
}

/// Direction of synchronization
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompareDirection {
    /// Local -> Remote (upload changes)
    LocalToRemote,
    /// Remote -> Local (download changes)
    RemoteToLocal,
    /// Both directions (full sync)
    Bidirectional,
}

/// Canonical direction of a sync-tree execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDirection {
    Upload,
    Download,
    Both,
}

impl SyncDirection {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "upload" | "up" | "push" => Some(Self::Upload),
            "download" | "down" | "pull" => Some(Self::Download),
            "both" | "bidirectional" => Some(Self::Both),
            _ => None,
        }
    }
}

/// Conflict resolution when both sides disagree on the same relative path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictMode {
    /// Keep the larger file (default, matches the CLI).
    Larger,
    /// Keep the newer file.
    Newer,
    /// Skip the file when sizes differ.
    Skip,
}

impl ConflictMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "larger" => Some(Self::Larger),
            "newer" => Some(Self::Newer),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

/// Explicit policy used to decide whether a file needs syncing and, later,
/// which transfer strategy should be preferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeltaPolicy {
    Disabled,
    SizeOnly,
    #[default]
    Mtime,
    Hash,
    Delta,
}

impl DeltaPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "disabled" => Some(Self::Disabled),
            "size_only" | "size-only" | "size" => Some(Self::SizeOnly),
            "mtime" => Some(Self::Mtime),
            "hash" => Some(Self::Hash),
            "delta" => Some(Self::Delta),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::SizeOnly => "size_only",
            Self::Mtime => "mtime",
            Self::Hash => "hash",
            Self::Delta => "delta",
        }
    }

    pub const fn wants_checksums(self) -> bool {
        matches!(self, Self::Hash | Self::Delta)
    }
}

/// Tunable options for the sync tree core.
#[derive(Debug, Clone)]
pub struct SyncOptions {
    pub direction: SyncDirection,
    pub delta_policy: DeltaPolicy,
    pub dry_run: bool,
    pub delete_orphans: bool,
    pub conflict_mode: ConflictMode,
    pub scan: ScanOptions,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            direction: SyncDirection::Upload,
            delta_policy: DeltaPolicy::default(),
            dry_run: false,
            delete_orphans: false,
            conflict_mode: ConflictMode::Larger,
            scan: ScanOptions::default(),
        }
    }
}

/// High-level phase, emitted via [`SyncProgressSink::on_phase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPhase {
    Scanning,
    Planning,
    Executing,
    Done,
}

/// Per-file outcome delivered to the progress sink after each operation.
#[derive(Debug, Clone)]
pub enum FileOutcome {
    Uploaded { bytes: u64 },
    Downloaded { bytes: u64 },
    Deleted,
    Skipped { reason: String },
    Failed { error: String },
}

/// Aggregated counters returned by [`sync_tree_core`].
#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    pub uploaded: u32,
    pub downloaded: u32,
    pub deleted: u32,
    pub skipped: u32,
    pub errors: Vec<SyncError>,
    pub elapsed_secs: f64,
    pub dry_run: bool,
    pub direction: Option<SyncDirection>,
    pub delta_policy: Option<DeltaPolicy>,
}

impl SyncReport {
    pub fn error_count(&self) -> usize {
        self.errors.len()
    }
}

/// Single sync error with enough context for an agent to retry intelligently.
#[derive(Debug, Clone)]
pub struct SyncError {
    pub rel_path: String,
    pub operation: &'static str,
    pub message: String,
    pub decision_policy: DeltaPolicy,
}

/// Callback interface used to report sync progress.
pub trait SyncProgressSink: Send {
    fn on_phase(&mut self, phase: SyncPhase);
    fn on_file_start(
        &mut self,
        rel: &str,
        total: u64,
        op: &'static str,
        decision_policy: DeltaPolicy,
    );
    fn on_file_progress(&mut self, rel: &str, sent: u64, total: u64);
    fn on_file_done(&mut self, rel: &str, outcome: &FileOutcome);
}

/// Drop-in no-op sink for callers that do not care about progress.
pub struct NoopProgressSink;

impl SyncProgressSink for NoopProgressSink {
    fn on_phase(&mut self, _phase: SyncPhase) {}
    fn on_file_start(
        &mut self,
        _rel: &str,
        _total: u64,
        _op: &'static str,
        _decision_policy: DeltaPolicy,
    ) {
    }
    fn on_file_progress(&mut self, _rel: &str, _sent: u64, _total: u64) {}
    fn on_file_done(&mut self, _rel: &str, _outcome: &FileOutcome) {}
}

enum SyncTreeAction {
    Copy,
    Skip(String),
}

struct SyncTreeDecision {
    action: SyncTreeAction,
    decision_policy: DeltaPolicy,
}

#[derive(Clone, Copy)]
struct SyncFileMeta<'a> {
    size: u64,
    mtime: Option<&'a str>,
    hash: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct SyncTransferSpec<'a> {
    rel: &'a str,
    total: u64,
    decision_policy: DeltaPolicy,
    /// Policy originally requested by the caller. Kept distinct from
    /// `decision_policy` because the decide layer may downgrade the policy
    /// when required data is missing (e.g. `Hash` falls back to `Mtime` if
    /// the remote provider has no checksum). The native delta wrapper
    /// (P1-T01) is consulted on this field, not on `decision_policy`, so
    /// the native attempt fires if and only if the user explicitly asked
    /// for `Delta`.
    requested_policy: DeltaPolicy,
}

/// Action to perform during sync
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncAction {
    Upload,
    Download,
    DeleteLocal,
    DeleteRemote,
    Skip,
    AskUser,
    KeepBoth,
}

/// A sync operation to execute
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncOperation {
    pub comparison: FileComparison,
    pub action: SyncAction,
}

/// Result of sync operations
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub uploaded: u32,
    pub downloaded: u32,
    pub deleted: u32,
    pub skipped: u32,
    pub errors: Vec<String>,
}

impl Default for SyncResult {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl SyncResult {
    pub fn new() -> Self {
        Self {
            uploaded: 0,
            downloaded: 0,
            deleted: 0,
            skipped: 0,
            errors: Vec::new(),
        }
    }
}

/// Validate a relative path against traversal attacks.
/// Rejects null bytes, absolute paths, drive letters, and `..` components.
pub fn validate_relative_path(relative_path: &str) -> Result<(), String> {
    if relative_path.contains('\0') {
        return Err("Path contains null bytes".to_string());
    }
    if relative_path.starts_with('/') || relative_path.starts_with('\\') {
        return Err("Absolute path not allowed in relative context".to_string());
    }
    // Check for Windows drive letters (e.g., C:)
    if relative_path.len() >= 2 && relative_path.as_bytes()[1] == b':' {
        return Err("Drive letter paths not allowed".to_string());
    }
    for component in std::path::Path::new(relative_path).components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err("Path traversal (..) not allowed".to_string());
        }
    }
    Ok(())
}

/// Check if a path matches any exclude pattern
pub fn should_exclude(path: &str, patterns: &[String]) -> bool {
    let path_lower = path.to_lowercase();
    let path_segments: Vec<&str> = path_lower.split(&['/', '\\'][..]).collect();

    for pattern in patterns {
        let pattern_lower = pattern.to_lowercase();

        // Simple glob matching
        if let Some(ext) = pattern_lower.strip_prefix('*') {
            // *.ext pattern
            if path_lower.ends_with(ext) {
                return true;
            }
        } else {
            // Match against path segments (not just substring)
            // This prevents false positives like "node" matching "node_modules"
            for segment in &path_segments {
                if segment == &pattern_lower {
                    return true;
                }
            }
        }
    }

    false
}

/// Check if a file should be filtered out by size/age constraints.
/// Returns true if the file should be SKIPPED.
fn should_filter(info: Option<&FileInfo>, options: &CompareOptions) -> bool {
    if let Some(f) = info {
        // Size filters
        if let Some(min) = options.min_size {
            if !f.is_dir && f.size < min {
                return true;
            }
        }
        if let Some(max) = options.max_size {
            if !f.is_dir && f.size > max {
                return true;
            }
        }
        // Age filters (seconds since now)
        if let Some(modified) = f.modified {
            let age = (Utc::now() - modified).num_seconds().max(0) as u64;
            if let Some(min_age) = options.min_age_secs {
                if age < min_age {
                    return true;
                } // Too new
            }
            if let Some(max_age) = options.max_age_secs {
                if age > max_age {
                    return true;
                } // Too old
            }
        }
    }
    false
}

/// Compare two timestamps with tolerance.
/// When both timestamps are absent, returns true (cannot distinguish — treat as equal).
pub fn timestamps_equal(local: Option<DateTime<Utc>>, remote: Option<DateTime<Utc>>) -> bool {
    match (local, remote) {
        (Some(l), Some(r)) => {
            (l.signed_duration_since(r)).num_seconds().abs() <= TIMESTAMP_TOLERANCE_SECS
        }
        (None, None) => true, // Both absent — cannot distinguish, treat as equal
        _ => false,           // One present, one absent — not equal
    }
}

/// Determine which timestamp is newer
pub fn compare_timestamps(
    local: Option<DateTime<Utc>>,
    remote: Option<DateTime<Utc>>,
) -> Option<SyncStatus> {
    match (local, remote) {
        (Some(l), Some(r)) => {
            let diff = l.signed_duration_since(r).num_seconds();
            if diff.abs() <= TIMESTAMP_TOLERANCE_SECS {
                None // Equal within tolerance
            } else if diff > 0 {
                Some(SyncStatus::LocalNewer)
            } else {
                Some(SyncStatus::RemoteNewer)
            }
        }
        _ => None, // Can't compare if timestamps missing
    }
}

/// Compare a single file pair and determine status
pub fn compare_file_pair(
    local: Option<&FileInfo>,
    remote: Option<&FileInfo>,
    options: &CompareOptions,
) -> SyncStatus {
    match (local, remote) {
        (None, None) => SyncStatus::Identical, // Shouldn't happen
        (Some(_), None) => SyncStatus::LocalOnly,
        (None, Some(_)) => SyncStatus::RemoteOnly,
        (Some(l), Some(r)) => {
            // Both exist - compare attributes

            // Directories that exist on both sides are always identical
            if l.is_dir && r.is_dir {
                return SyncStatus::Identical;
            }

            // ──── Checksum Comparison (when enabled) ────
            // Local file checksums are computed via SHA-256 in get_local_files_recursive
            // when options.compare_checksum is true. Remote checksums are provider-dependent
            // and may not always be available.
            if options.compare_checksum {
                match (&l.checksum, &r.checksum) {
                    (Some(l_hash), Some(r_hash)) => {
                        if l_hash == r_hash {
                            return SyncStatus::Identical;
                        }
                        // Hashes differ - determine which is newer by timestamp
                        if options.compare_timestamp {
                            return compare_timestamps(l.modified, r.modified)
                                .unwrap_or(SyncStatus::Conflict);
                        } else {
                            // No timestamp comparison, but hashes differ
                            return SyncStatus::Conflict;
                        }
                    }
                    (None, None) => {
                        // Checksums not available, fall through to size/timestamp
                    }
                    _ => {
                        // One has checksum, one doesn't - can't use checksum comparison
                        // Fall through to size/timestamp
                    }
                }
            }

            // ──── Size-only fallback when timestamps absent ────
            // Providers like FileLu may return modified=None for folders or files.
            // When timestamps are unavailable, fall back to size-only comparison
            // to avoid infinite re-sync loops.
            let both_timestamps_present = l.modified.is_some() && r.modified.is_some();

            // First check size if enabled
            if options.compare_size && l.size != r.size {
                // Different sizes - determine which is newer
                if options.compare_timestamp && both_timestamps_present {
                    match compare_timestamps(l.modified, r.modified) {
                        Some(status) => return status,
                        None => return SyncStatus::SizeMismatch,
                    }
                } else {
                    return SyncStatus::SizeMismatch;
                }
            }

            // Size is same (or not comparing size), check timestamp
            if options.compare_timestamp {
                if !both_timestamps_present {
                    // One or both timestamps absent — size already matched (or not compared),
                    // treat as identical to avoid spurious re-syncs
                    SyncStatus::Identical
                } else if timestamps_equal(l.modified, r.modified) {
                    SyncStatus::Identical
                } else {
                    match compare_timestamps(l.modified, r.modified) {
                        Some(status) => status,
                        None => SyncStatus::Identical,
                    }
                }
            } else {
                // Not comparing anything else, assume identical
                SyncStatus::Identical
            }
        }
    }
}

/// Generate a human-readable explanation for why a file needs syncing
pub fn generate_sync_reason(
    status: &SyncStatus,
    local_info: Option<&FileInfo>,
    remote_info: Option<&FileInfo>,
    is_dir: bool,
) -> String {
    if is_dir && *status == SyncStatus::Identical {
        return "Directory".to_string();
    }

    match status {
        SyncStatus::Identical => "Files are identical".to_string(),
        SyncStatus::LocalNewer => {
            if let (Some(l), Some(r)) = (local_info, remote_info) {
                let mut parts = Vec::new();
                if let (Some(l_mod), Some(r_mod)) = (l.modified, r.modified) {
                    let diff_secs = l_mod.signed_duration_since(r_mod).num_seconds();
                    if diff_secs > 0 {
                        parts.push(format!("Local is {} newer", format_duration(diff_secs)));
                    }
                }
                if l.size != r.size {
                    parts.push(format!("size: {} vs {} bytes", l.size, r.size));
                }
                if parts.is_empty() {
                    "Local file is newer".to_string()
                } else {
                    parts.join(", ")
                }
            } else {
                "Local file is newer".to_string()
            }
        }
        SyncStatus::RemoteNewer => {
            if let (Some(l), Some(r)) = (local_info, remote_info) {
                let mut parts = Vec::new();
                if let (Some(l_mod), Some(r_mod)) = (l.modified, r.modified) {
                    let diff_secs = r_mod.signed_duration_since(l_mod).num_seconds();
                    if diff_secs > 0 {
                        parts.push(format!("Remote is {} newer", format_duration(diff_secs)));
                    }
                }
                if l.size != r.size {
                    parts.push(format!("size: {} vs {} bytes", l.size, r.size));
                }
                if parts.is_empty() {
                    "Remote file is newer".to_string()
                } else {
                    parts.join(", ")
                }
            } else {
                "Remote file is newer".to_string()
            }
        }
        SyncStatus::LocalOnly => {
            if let Some(l) = local_info {
                if l.is_dir {
                    "Directory exists only locally".to_string()
                } else {
                    format!("File exists only locally ({} bytes)", l.size)
                }
            } else {
                "File exists only locally".to_string()
            }
        }
        SyncStatus::RemoteOnly => {
            if let Some(r) = remote_info {
                if r.is_dir {
                    "Directory exists only on remote".to_string()
                } else {
                    format!("File exists only on remote ({} bytes)", r.size)
                }
            } else {
                "File exists only on remote".to_string()
            }
        }
        SyncStatus::Conflict => {
            if let (Some(l), Some(r)) = (local_info, remote_info) {
                let mut parts = vec!["Both modified since last sync".to_string()];
                if l.size != r.size {
                    parts.push(format!("local: {} bytes, remote: {} bytes", l.size, r.size));
                }
                if let (Some(lc), Some(rc)) = (&l.checksum, &r.checksum) {
                    if lc != rc {
                        parts.push("checksums differ".to_string());
                    }
                }
                parts.join(", ")
            } else {
                "Both files have been modified since last sync".to_string()
            }
        }
        SyncStatus::SizeMismatch => {
            if let (Some(l), Some(r)) = (local_info, remote_info) {
                format!(
                    "Same timestamp but different size (local: {} bytes, remote: {} bytes)",
                    l.size, r.size
                )
            } else {
                "Same timestamp but different file size".to_string()
            }
        }
    }
}

/// Format a duration in seconds into a human-readable string
fn format_duration(secs: i64) -> String {
    let abs = secs.unsigned_abs();
    if abs < 60 {
        format!("{}s", abs)
    } else if abs < 3600 {
        format!("{}m {}s", abs / 60, abs % 60)
    } else if abs < 86400 {
        format!("{}h {}m", abs / 3600, (abs % 3600) / 60)
    } else {
        format!("{}d {}h", abs / 86400, (abs % 86400) / 3600)
    }
}

/// Build comparison results from local and remote file maps
pub fn build_comparison_results(
    local_files: HashMap<String, FileInfo>,
    remote_files: HashMap<String, FileInfo>,
    options: &CompareOptions,
) -> Vec<FileComparison> {
    let mut results = Vec::new();
    let mut all_paths: std::collections::HashSet<String> = local_files.keys().cloned().collect();
    all_paths.extend(remote_files.keys().cloned());

    for path in all_paths {
        // Reject paths with traversal components
        if validate_relative_path(&path).is_err() {
            tracing::warn!("Skipping entry with invalid relative path: {}", path);
            continue;
        }

        // Skip excluded paths
        if should_exclude(&path, &options.exclude_patterns) {
            continue;
        }

        let local = local_files.get(&path);
        let remote = remote_files.get(&path);

        // Apply size/age filters
        if should_filter(local, options) || should_filter(remote, options) {
            continue;
        }

        let status = compare_file_pair(local, remote, options);

        // Skip identical files unless they're directories we need to show
        let is_dir =
            local.map(|f| f.is_dir).unwrap_or(false) || remote.map(|f| f.is_dir).unwrap_or(false);

        if status != SyncStatus::Identical || is_dir {
            let sync_reason = generate_sync_reason(&status, local, remote, is_dir);
            results.push(FileComparison {
                relative_path: path,
                status,
                local_info: local.cloned(),
                remote_info: remote.cloned(),
                is_dir,
                sync_reason,
                previously_synced: false,
            });
        }
    }

    // Sort by path for consistent display
    results.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));

    results
}

/// Determine the recommended action based on comparison status and direction
#[allow(dead_code)]
pub fn get_recommended_action(status: &SyncStatus, direction: &CompareDirection) -> SyncAction {
    match (status, direction) {
        // Bidirectional
        (SyncStatus::LocalNewer, CompareDirection::Bidirectional) => SyncAction::Upload,
        (SyncStatus::RemoteNewer, CompareDirection::Bidirectional) => SyncAction::Download,
        (SyncStatus::LocalOnly, CompareDirection::Bidirectional) => SyncAction::Upload,
        (SyncStatus::RemoteOnly, CompareDirection::Bidirectional) => SyncAction::Download,
        (SyncStatus::Conflict, _) => SyncAction::AskUser,
        (SyncStatus::SizeMismatch, _) => SyncAction::AskUser,

        // Local to Remote
        (SyncStatus::LocalNewer, CompareDirection::LocalToRemote) => SyncAction::Upload,
        (SyncStatus::LocalOnly, CompareDirection::LocalToRemote) => SyncAction::Upload,
        (SyncStatus::RemoteNewer, CompareDirection::LocalToRemote) => SyncAction::Skip,
        (SyncStatus::RemoteOnly, CompareDirection::LocalToRemote) => SyncAction::DeleteRemote,

        // Remote to Local
        (SyncStatus::RemoteNewer, CompareDirection::RemoteToLocal) => SyncAction::Download,
        (SyncStatus::RemoteOnly, CompareDirection::RemoteToLocal) => SyncAction::Download,
        (SyncStatus::LocalNewer, CompareDirection::RemoteToLocal) => SyncAction::Skip,
        (SyncStatus::LocalOnly, CompareDirection::RemoteToLocal) => SyncAction::DeleteLocal,

        // Identical - no action needed
        (SyncStatus::Identical, _) => SyncAction::Skip,
    }
}

/// Run a sync between `local_root` and `remote_root` using `provider` and
/// record progress via `sink`.
pub async fn sync_tree_core(
    provider: &mut Box<dyn StorageProvider>,
    local_root: &str,
    remote_root: &str,
    opts: &SyncOptions,
    sink: &mut dyn SyncProgressSink,
) -> SyncReport {
    let start = std::time::Instant::now();
    sink.on_phase(SyncPhase::Scanning);
    let locals = scan_local_tree(local_root, &opts.scan);

    if !opts.dry_run
        && !locals.is_empty()
        && matches!(opts.direction, SyncDirection::Upload | SyncDirection::Both)
    {
        ensure_remote_dir(provider, remote_root).await;
    }

    let remotes = scan_remote_tree(provider, remote_root, &opts.scan).await;

    sink.on_phase(SyncPhase::Planning);
    let mut report = SyncReport {
        dry_run: opts.dry_run,
        direction: Some(opts.direction),
        delta_policy: Some(opts.delta_policy),
        ..SyncReport::default()
    };

    use std::collections::{HashMap as Map, HashSet};
    let mut seen: HashSet<String> = HashSet::new();
    let local_entries_by_path: Map<&str, &crate::sync_core::LocalEntry> = locals
        .iter()
        .map(|entry| (entry.rel_path.as_str(), entry))
        .collect();
    let remote_entries_by_path: Map<&str, &crate::sync_core::RemoteEntry> = remotes
        .iter()
        .map(|entry| (entry.rel_path.as_str(), entry))
        .collect();

    sink.on_phase(SyncPhase::Executing);

    if matches!(opts.direction, SyncDirection::Upload | SyncDirection::Both) {
        for local_entry in &locals {
            if !seen.insert(local_entry.rel_path.clone()) {
                continue;
            }
            let remote_entry = remote_entries_by_path
                .get(local_entry.rel_path.as_str())
                .copied();
            let decision = decide_upload(
                local_entry,
                remote_entry,
                opts.delta_policy,
                opts.conflict_mode,
            );
            match decision.action {
                SyncTreeAction::Copy => {
                    let outcome = perform_upload(
                        provider,
                        local_root,
                        remote_root,
                        SyncTransferSpec {
                            rel: &local_entry.rel_path,
                            total: local_entry.size,
                            decision_policy: decision.decision_policy,
                            requested_policy: opts.delta_policy,
                        },
                        opts.dry_run,
                        sink,
                    )
                    .await;
                    apply_sync_tree_outcome(
                        &mut report,
                        &local_entry.rel_path,
                        "upload",
                        outcome,
                        decision.decision_policy,
                        sink,
                    );
                }
                SyncTreeAction::Skip(reason) => {
                    sink.on_file_start(
                        &local_entry.rel_path,
                        0,
                        "skip",
                        decision.decision_policy,
                    );
                    let outcome = FileOutcome::Skipped { reason };
                    apply_sync_tree_outcome(
                        &mut report,
                        &local_entry.rel_path,
                        "upload",
                        outcome,
                        decision.decision_policy,
                        sink,
                    );
                }
            }
        }
    }

    if matches!(opts.direction, SyncDirection::Download | SyncDirection::Both) {
        for remote_entry in &remotes {
            let already_seen = !seen.insert(remote_entry.rel_path.clone());
            let decision = decide_download(
                remote_entry,
                local_entries_by_path.get(remote_entry.rel_path.as_str()).copied(),
                opts.delta_policy,
                opts.conflict_mode,
                already_seen && matches!(opts.direction, SyncDirection::Both),
            );
            match decision.action {
                SyncTreeAction::Copy => {
                    let outcome = perform_download(
                        provider,
                        local_root,
                        remote_root,
                        SyncTransferSpec {
                            rel: &remote_entry.rel_path,
                            total: remote_entry.size,
                            decision_policy: decision.decision_policy,
                            requested_policy: opts.delta_policy,
                        },
                        opts.dry_run,
                        sink,
                    )
                    .await;
                    apply_sync_tree_outcome(
                        &mut report,
                        &remote_entry.rel_path,
                        "download",
                        outcome,
                        decision.decision_policy,
                        sink,
                    );
                }
                SyncTreeAction::Skip(reason) => {
                    sink.on_file_start(
                        &remote_entry.rel_path,
                        0,
                        "skip",
                        decision.decision_policy,
                    );
                    let outcome = FileOutcome::Skipped { reason };
                    apply_sync_tree_outcome(
                        &mut report,
                        &remote_entry.rel_path,
                        "download",
                        outcome,
                        decision.decision_policy,
                        sink,
                    );
                }
            }
        }
    }

    if opts.delete_orphans {
        match opts.direction {
            SyncDirection::Upload => {
                for remote_entry in &remotes {
                    if !local_entries_by_path.contains_key(remote_entry.rel_path.as_str()) {
                        let outcome = perform_remote_delete(
                            provider,
                            remote_root,
                            &remote_entry.rel_path,
                            opts.delta_policy,
                            opts.dry_run,
                            sink,
                        )
                        .await;
                        apply_sync_tree_outcome(
                            &mut report,
                            &remote_entry.rel_path,
                            "delete_remote",
                            outcome,
                            opts.delta_policy,
                            sink,
                        );
                    }
                }
            }
            SyncDirection::Download => {
                for local_entry in &locals {
                    if !remote_entries_by_path.contains_key(local_entry.rel_path.as_str()) {
                        let outcome = perform_local_delete(
                            local_root,
                            &local_entry.rel_path,
                            opts.delta_policy,
                            opts.dry_run,
                            sink,
                        );
                        apply_sync_tree_outcome(
                            &mut report,
                            &local_entry.rel_path,
                            "delete_local",
                            outcome,
                            opts.delta_policy,
                            sink,
                        );
                    }
                }
            }
            SyncDirection::Both => {}
        }
    }

    report.elapsed_secs = start.elapsed().as_secs_f64();
    sink.on_phase(SyncPhase::Done);
    report
}

fn decide_upload(
    local_entry: &crate::sync_core::LocalEntry,
    remote_entry: Option<&crate::sync_core::RemoteEntry>,
    policy: DeltaPolicy,
    mode: ConflictMode,
) -> SyncTreeDecision {
    let Some(remote_entry) = remote_entry else {
        return SyncTreeDecision {
            action: SyncTreeAction::Copy,
            decision_policy: policy,
        };
    };

    match policy {
        DeltaPolicy::SizeOnly => decide_upload_by_size(local_entry.size, remote_entry.size, mode),
        DeltaPolicy::Mtime | DeltaPolicy::Disabled => decide_upload_by_mtime(
            local_entry.size,
            local_entry.mtime.as_deref(),
            remote_entry.size,
            remote_entry.mtime.as_deref(),
            mode,
            policy,
        ),
        DeltaPolicy::Hash | DeltaPolicy::Delta => decide_upload_by_hash(
            SyncFileMeta {
                size: local_entry.size,
                mtime: local_entry.mtime.as_deref(),
                hash: local_entry.sha256.as_deref(),
            },
            SyncFileMeta {
                size: remote_entry.size,
                mtime: remote_entry.mtime.as_deref(),
                hash: remote_entry.checksum_hex.as_deref(),
            },
            mode,
            policy,
        ),
    }
}

fn decide_download(
    remote_entry: &crate::sync_core::RemoteEntry,
    local_entry: Option<&crate::sync_core::LocalEntry>,
    policy: DeltaPolicy,
    mode: ConflictMode,
    already_handled_by_upload: bool,
) -> SyncTreeDecision {
    if already_handled_by_upload {
        return SyncTreeDecision {
            action: SyncTreeAction::Skip("resolved by upload pass".to_string()),
            decision_policy: policy,
        };
    }
    let Some(local_entry) = local_entry else {
        return SyncTreeDecision {
            action: SyncTreeAction::Copy,
            decision_policy: policy,
        };
    };

    match policy {
        DeltaPolicy::SizeOnly => decide_download_by_size(remote_entry.size, local_entry.size, mode),
        DeltaPolicy::Mtime | DeltaPolicy::Disabled => decide_download_by_mtime(
            remote_entry.size,
            remote_entry.mtime.as_deref(),
            local_entry.size,
            local_entry.mtime.as_deref(),
            mode,
            policy,
        ),
        DeltaPolicy::Hash | DeltaPolicy::Delta => decide_download_by_hash(
            SyncFileMeta {
                size: remote_entry.size,
                mtime: remote_entry.mtime.as_deref(),
                hash: remote_entry.checksum_hex.as_deref(),
            },
            SyncFileMeta {
                size: local_entry.size,
                mtime: local_entry.mtime.as_deref(),
                hash: local_entry.sha256.as_deref(),
            },
            mode,
            policy,
        ),
    }
}

fn decide_upload_by_size(
    local_size: u64,
    remote_size: u64,
    mode: ConflictMode,
) -> SyncTreeDecision {
    let action = if remote_size == local_size {
        SyncTreeAction::Skip("identical size".to_string())
    } else {
        match mode {
            ConflictMode::Larger if local_size > remote_size => SyncTreeAction::Copy,
            ConflictMode::Larger => SyncTreeAction::Skip("remote is larger".to_string()),
            ConflictMode::Newer => SyncTreeAction::Copy,
            ConflictMode::Skip => SyncTreeAction::Skip("conflict skip".to_string()),
        }
    };
    SyncTreeDecision {
        action,
        decision_policy: DeltaPolicy::SizeOnly,
    }
}

fn decide_download_by_size(
    remote_size: u64,
    local_size: u64,
    mode: ConflictMode,
) -> SyncTreeDecision {
    let action = if remote_size == local_size {
        SyncTreeAction::Skip("identical size".to_string())
    } else {
        match mode {
            ConflictMode::Larger if remote_size > local_size => SyncTreeAction::Copy,
            ConflictMode::Larger => SyncTreeAction::Skip("local is larger".to_string()),
            ConflictMode::Newer => {
                SyncTreeAction::Skip("newer mode prefers existing local".to_string())
            }
            ConflictMode::Skip => SyncTreeAction::Skip("conflict skip".to_string()),
        }
    };
    SyncTreeDecision {
        action,
        decision_policy: DeltaPolicy::SizeOnly,
    }
}

fn decide_upload_by_mtime(
    local_size: u64,
    local_mtime: Option<&str>,
    remote_size: u64,
    remote_mtime: Option<&str>,
    mode: ConflictMode,
    requested_policy: DeltaPolicy,
) -> SyncTreeDecision {
    if let Some(ordering) = compare_scan_mtimes(local_mtime, remote_mtime) {
        let action = match ordering {
            std::cmp::Ordering::Greater => SyncTreeAction::Copy,
            std::cmp::Ordering::Less => SyncTreeAction::Skip("remote is newer".to_string()),
            std::cmp::Ordering::Equal if local_size == remote_size => {
                SyncTreeAction::Skip("identical mtime and size".to_string())
            }
            std::cmp::Ordering::Equal => return decide_upload_by_size(local_size, remote_size, mode),
        };
        return SyncTreeDecision {
            action,
            decision_policy: if matches!(requested_policy, DeltaPolicy::Disabled) {
                DeltaPolicy::Disabled
            } else {
                DeltaPolicy::Mtime
            },
        };
    }

    decide_upload_by_size(local_size, remote_size, mode)
}

fn decide_download_by_mtime(
    remote_size: u64,
    remote_mtime: Option<&str>,
    local_size: u64,
    local_mtime: Option<&str>,
    mode: ConflictMode,
    requested_policy: DeltaPolicy,
) -> SyncTreeDecision {
    if let Some(ordering) = compare_scan_mtimes(remote_mtime, local_mtime) {
        let action = match ordering {
            std::cmp::Ordering::Greater => SyncTreeAction::Copy,
            std::cmp::Ordering::Less => SyncTreeAction::Skip("local is newer".to_string()),
            std::cmp::Ordering::Equal if remote_size == local_size => {
                SyncTreeAction::Skip("identical mtime and size".to_string())
            }
            std::cmp::Ordering::Equal => return decide_download_by_size(remote_size, local_size, mode),
        };
        return SyncTreeDecision {
            action,
            decision_policy: if matches!(requested_policy, DeltaPolicy::Disabled) {
                DeltaPolicy::Disabled
            } else {
                DeltaPolicy::Mtime
            },
        };
    }

    decide_download_by_size(remote_size, local_size, mode)
}

fn decide_upload_by_hash(
    local: SyncFileMeta<'_>,
    remote: SyncFileMeta<'_>,
    mode: ConflictMode,
    requested_policy: DeltaPolicy,
) -> SyncTreeDecision {
    if let (Some(local_hash), Some(remote_hash)) = (local.hash, remote.hash) {
        if local_hash.eq_ignore_ascii_case(remote_hash) {
            return SyncTreeDecision {
                action: SyncTreeAction::Skip("identical hash".to_string()),
                decision_policy: DeltaPolicy::Hash,
            };
        }
    }

    decide_upload_by_mtime(
        local.size,
        local.mtime,
        remote.size,
        remote.mtime,
        mode,
        requested_policy,
    )
}

fn decide_download_by_hash(
    remote: SyncFileMeta<'_>,
    local: SyncFileMeta<'_>,
    mode: ConflictMode,
    requested_policy: DeltaPolicy,
) -> SyncTreeDecision {
    if let (Some(remote_hash), Some(local_hash)) = (remote.hash, local.hash) {
        if remote_hash.eq_ignore_ascii_case(local_hash) {
            return SyncTreeDecision {
                action: SyncTreeAction::Skip("identical hash".to_string()),
                decision_policy: DeltaPolicy::Hash,
            };
        }
    }

    decide_download_by_mtime(
        remote.size,
        remote.mtime,
        local.size,
        local.mtime,
        mode,
        requested_policy,
    )
}

fn compare_scan_mtimes(left: Option<&str>, right: Option<&str>) -> Option<std::cmp::Ordering> {
    let left = left.and_then(parse_scan_mtime)?;
    let right = right.and_then(parse_scan_mtime)?;
    Some(left.cmp(&right))
}

fn parse_scan_mtime(raw: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            let trimmed = raw.strip_suffix('Z').unwrap_or(raw);
            chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S"))
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M"))
                .ok()
                .map(|naive| chrono::DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        })
}

fn apply_sync_tree_outcome(
    report: &mut SyncReport,
    rel: &str,
    operation: &'static str,
    outcome: FileOutcome,
    decision_policy: DeltaPolicy,
    sink: &mut dyn SyncProgressSink,
) {
    match &outcome {
        FileOutcome::Uploaded { .. } => report.uploaded += 1,
        FileOutcome::Downloaded { .. } => report.downloaded += 1,
        FileOutcome::Deleted => report.deleted += 1,
        FileOutcome::Skipped { .. } => report.skipped += 1,
        FileOutcome::Failed { error } => {
            report.errors.push(SyncError {
                rel_path: rel.to_string(),
                operation,
                message: error.clone(),
                decision_policy,
            });
        }
    }
    sink.on_file_done(rel, &outcome);
}

async fn perform_upload(
    provider: &mut Box<dyn StorageProvider>,
    local_root: &str,
    remote_root: &str,
    transfer: SyncTransferSpec<'_>,
    dry_run: bool,
    sink: &mut dyn SyncProgressSink,
) -> FileOutcome {
    sink.on_file_start(
        transfer.rel,
        transfer.total,
        "upload",
        transfer.decision_policy,
    );
    if dry_run {
        return FileOutcome::Skipped {
            reason: "dry-run".to_string(),
        };
    }
    let local_path = join_clean(local_root, transfer.rel);
    let remote_path = join_clean_remote(remote_root, transfer.rel);
    ensure_remote_parent(provider, &remote_path).await;

    // P1-T01: try the native delta wrapper before the classic provider path.
    // The wrapper gates eligibility internally (SFTP downcast + active SSH
    // handle + rsync availability). `None` means the session is not delta
    // eligible (non-SFTP provider, password-only auth, no handle): fall
    // through silently to the classic upload. A `hard_error` (e.g. SSH
    // host-key mismatch) surfaces as `FileOutcome::Failed` without falling
    // back, so security failures never get masked by the classic path.
    #[cfg(unix)]
    if matches!(transfer.requested_policy, DeltaPolicy::Delta) {
        let local_path_buf = std::path::PathBuf::from(&local_path);
        match crate::delta_sync_rsync::try_delta_transfer(
            &mut **provider,
            crate::delta_sync_rsync::SyncDirection::Upload,
            &local_path_buf,
            &remote_path,
        )
        .await
        {
            Some(result) if result.used_delta => {
                tracing::info!(
                    "sync.delta: used delta path (direction=Upload, remote={})",
                    remote_path
                );
                return FileOutcome::Uploaded {
                    bytes: transfer.total,
                };
            }
            Some(result) if result.hard_error.is_some() => {
                let msg = result.hard_error.unwrap_or_default();
                return FileOutcome::Failed {
                    error: format!("delta hard rejection: {}", msg),
                };
            }
            Some(result) => {
                if let Some(reason) = result.fallback_reason {
                    tracing::info!(
                        "sync.delta: fallback to classic (direction=Upload, remote={}, reason={})",
                        remote_path,
                        reason
                    );
                }
            }
            None => {}
        }
    }

    match provider.upload(&local_path, &remote_path, None).await {
        Ok(()) => FileOutcome::Uploaded {
            bytes: transfer.total,
        },
        Err(e) => FileOutcome::Failed {
            error: format!("upload failed: {}", e),
        },
    }
}

async fn perform_download(
    provider: &mut Box<dyn StorageProvider>,
    local_root: &str,
    remote_root: &str,
    transfer: SyncTransferSpec<'_>,
    dry_run: bool,
    sink: &mut dyn SyncProgressSink,
) -> FileOutcome {
    sink.on_file_start(
        transfer.rel,
        transfer.total,
        "download",
        transfer.decision_policy,
    );
    if dry_run {
        return FileOutcome::Skipped {
            reason: "dry-run".to_string(),
        };
    }
    let remote_path = join_clean_remote(remote_root, transfer.rel);
    let local_path = join_clean(local_root, transfer.rel);
    if let Some(parent) = Path::new(&local_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // P1-T01: see `perform_upload` above. Same contract, direction=Download.
    #[cfg(unix)]
    if matches!(transfer.requested_policy, DeltaPolicy::Delta) {
        let local_path_buf = std::path::PathBuf::from(&local_path);
        match crate::delta_sync_rsync::try_delta_transfer(
            &mut **provider,
            crate::delta_sync_rsync::SyncDirection::Download,
            &local_path_buf,
            &remote_path,
        )
        .await
        {
            Some(result) if result.used_delta => {
                tracing::info!(
                    "sync.delta: used delta path (direction=Download, remote={})",
                    remote_path
                );
                return FileOutcome::Downloaded {
                    bytes: transfer.total,
                };
            }
            Some(result) if result.hard_error.is_some() => {
                let msg = result.hard_error.unwrap_or_default();
                return FileOutcome::Failed {
                    error: format!("delta hard rejection: {}", msg),
                };
            }
            Some(result) => {
                if let Some(reason) = result.fallback_reason {
                    tracing::info!(
                        "sync.delta: fallback to classic (direction=Download, remote={}, reason={})",
                        remote_path,
                        reason
                    );
                }
            }
            None => {}
        }
    }

    match provider.download(&remote_path, &local_path, None).await {
        Ok(()) => FileOutcome::Downloaded {
            bytes: transfer.total,
        },
        Err(e) => FileOutcome::Failed {
            error: format!("download failed: {}", e),
        },
    }
}

async fn perform_remote_delete(
    provider: &mut Box<dyn StorageProvider>,
    remote_root: &str,
    rel: &str,
    decision_policy: DeltaPolicy,
    dry_run: bool,
    sink: &mut dyn SyncProgressSink,
) -> FileOutcome {
    sink.on_file_start(rel, 0, "delete_remote", decision_policy);
    if dry_run {
        return FileOutcome::Skipped {
            reason: "dry-run".to_string(),
        };
    }
    let remote_path = join_clean_remote(remote_root, rel);
    match provider.delete(&remote_path).await {
        Ok(()) => FileOutcome::Deleted,
        Err(e) => FileOutcome::Failed {
            error: format!("delete failed: {}", e),
        },
    }
}

fn perform_local_delete(
    local_root: &str,
    rel: &str,
    decision_policy: DeltaPolicy,
    dry_run: bool,
    sink: &mut dyn SyncProgressSink,
) -> FileOutcome {
    sink.on_file_start(rel, 0, "delete_local", decision_policy);
    if dry_run {
        return FileOutcome::Skipped {
            reason: "dry-run".to_string(),
        };
    }
    let path = join_clean(local_root, rel);
    match std::fs::remove_file(&path) {
        Ok(()) => FileOutcome::Deleted,
        Err(e) => FileOutcome::Failed {
            error: format!("delete failed: {}", e),
        },
    }
}

async fn ensure_remote_parent(provider: &mut Box<dyn StorageProvider>, remote_path: &str) {
    if let Some(idx) = remote_path.rfind('/') {
        let parent = &remote_path[..idx];
        if !parent.is_empty() {
            ensure_remote_dir(provider, parent).await;
        }
    }
}

fn remote_dir_chain(dir: &str) -> Vec<String> {
    let trimmed = dir.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" {
        return Vec::new();
    }

    let leading_slash = trimmed.starts_with('/');
    let mut accumulated = String::new();
    let mut chain = Vec::new();

    for part in trimmed.split('/') {
        if part.is_empty() {
            continue;
        }
        if leading_slash || !accumulated.is_empty() {
            accumulated.push('/');
        }
        accumulated.push_str(part);
        chain.push(accumulated.clone());
    }

    chain
}

fn mkdir_error_is_idempotent(err: &ProviderError) -> bool {
    match err {
        ProviderError::AlreadyExists(_) => true,
        ProviderError::ServerError(msg) | ProviderError::Other(msg) => {
            let lower = msg.to_ascii_lowercase();
            lower.contains("already exists")
                || lower.contains("file exists")
                || lower.contains("eexist")
                || lower.contains("550")
        }
        _ => false,
    }
}

async fn ensure_remote_dir(provider: &mut Box<dyn StorageProvider>, dir: &str) {
    // Walk the parent chain top-down. A failure on an intermediate level
    // (e.g. mkdir on `/mnt` denied because the user has no write access on
    // the SFTP root) must NOT short-circuit the chain: the leaf mkdir may
    // still succeed if the caller is authorized only on the target subtree.
    // Any genuine permission/path error will surface naturally on the
    // following `provider.upload`/`download` call. Idempotent errors (the
    // directory already exists) are silently absorbed at every level.
    for path in remote_dir_chain(dir) {
        match provider.mkdir(&path).await {
            Ok(()) => {}
            Err(err) if mkdir_error_is_idempotent(&err) => {}
            Err(err) => {
                tracing::debug!(
                    "ensure_remote_dir: non-idempotent mkdir error on '{}': {}; continuing chain",
                    path,
                    err
                );
            }
        }
    }
}

fn join_clean(root: &str, rel: &str) -> String {
    let rel = rel.trim_start_matches('/');
    if root.is_empty() {
        rel.to_string()
    } else if root.ends_with('/') || root.ends_with('\\') {
        format!("{}{}", root, rel)
    } else {
        format!("{}/{}", root, rel)
    }
}

fn join_clean_remote(root: &str, rel: &str) -> String {
    let rel = rel.trim_start_matches('/');
    if root.is_empty() || root == "/" {
        format!("/{}", rel)
    } else if root.ends_with('/') {
        format!("{}{}", root, rel)
    } else {
        format!("{}/{}", root, rel)
    }
}

// ============ Sync Index (cache for faster subsequent syncs) ============

/// Snapshot of a file's state at the time of last successful sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncIndexEntry {
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
    pub is_dir: bool,
}

/// Persistent index storing the state of files after a successful sync.
/// Used to detect true conflicts (both sides changed since last sync)
/// and to skip unchanged files for faster re-scans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncIndex {
    /// Version for future migrations
    pub version: u32,
    /// When this index was last updated
    pub last_sync: DateTime<Utc>,
    /// Local root path at time of sync
    pub local_path: String,
    /// Remote root path at time of sync
    pub remote_path: String,
    /// File states at time of last sync (key = relative_path)
    pub files: HashMap<String, SyncIndexEntry>,
}

impl SyncIndex {
    #[allow(dead_code)]
    pub fn new(local_path: String, remote_path: String) -> Self {
        Self {
            version: 1,
            last_sync: Utc::now(),
            local_path,
            remote_path,
            files: HashMap::new(),
        }
    }
}

/// Atomic write: write to temp file, then rename to target path.
/// Prevents corruption from crash/power-loss during write.
fn atomic_write(path: &std::path::Path, data: &[u8]) -> Result<(), String> {
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

/// Get the directory where sync indices are stored
fn sync_index_dir() -> Result<std::path::PathBuf, String> {
    let base = dirs::config_dir().ok_or_else(|| "Cannot determine config directory".to_string())?;
    let dir = base.join("aeroftp").join("sync-index");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create sync index directory: {}", e))?;
    Ok(dir)
}

/// Stable SHA-256 hash — collision-resistant filename generation (replaces DJB2)
/// Returns first 16 hex characters (64 bits) of SHA-256 digest.
fn stable_path_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let result = hasher.finalize();
    hex::encode(&result[..8]) // 16 hex chars = 64 bits, collision-resistant
}

/// Generate a stable filename from a local+remote path pair
fn index_filename(local_path: &str, remote_path: &str) -> String {
    let combined = format!("{}|{}", local_path, remote_path);
    format!("{}.json", stable_path_hash(&combined))
}

/// Load a sync index for a given path pair (returns None if not found)
pub fn load_sync_index(local_path: &str, remote_path: &str) -> Result<Option<SyncIndex>, String> {
    let dir = sync_index_dir()?;
    let path = dir.join(index_filename(local_path, remote_path));
    if !path.exists() {
        return Ok(None);
    }
    let data =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read sync index: {}", e))?;
    let index: SyncIndex =
        serde_json::from_str(&data).map_err(|e| format!("Failed to parse sync index: {}", e))?;
    Ok(Some(index))
}

/// Save a sync index for a given path pair
pub fn save_sync_index(index: &SyncIndex) -> Result<(), String> {
    let dir = sync_index_dir()?;
    let path = dir.join(index_filename(&index.local_path, &index.remote_path));
    let data = serde_json::to_string(index)
        .map_err(|e| format!("Failed to serialize sync index: {}", e))?;
    atomic_write(&path, data.as_bytes())?;
    Ok(())
}

/// Enhanced comparison that uses the sync index to detect true conflicts.
/// If both local and remote changed since the index snapshot, it's a Conflict.
pub fn build_comparison_results_with_index(
    local_files: HashMap<String, FileInfo>,
    remote_files: HashMap<String, FileInfo>,
    options: &CompareOptions,
    index: Option<&SyncIndex>,
) -> Vec<FileComparison> {
    let mut results = Vec::new();
    let mut all_paths: std::collections::HashSet<String> = local_files.keys().cloned().collect();
    all_paths.extend(remote_files.keys().cloned());

    for path in all_paths {
        // Reject paths with traversal components
        if validate_relative_path(&path).is_err() {
            tracing::warn!("Skipping entry with invalid relative path: {}", path);
            continue;
        }

        if should_exclude(&path, &options.exclude_patterns) {
            continue;
        }

        let local = local_files.get(&path);
        let remote = remote_files.get(&path);

        // Apply size/age filters
        if should_filter(local, options) || should_filter(remote, options) {
            continue;
        }

        let is_dir =
            local.map(|f| f.is_dir).unwrap_or(false) || remote.map(|f| f.is_dir).unwrap_or(false);

        // Check if we can use the index for conflict detection
        let status = if let (Some(idx), Some(l), Some(r)) = (index, local, remote) {
            if let Some(cached) = idx.files.get(&path) {
                // When cached timestamp is None (provider didn't return mtime),
                // fall back to size-only comparison to avoid false conflicts.
                let local_changed = l.size != cached.size
                    || (l.modified.is_some()
                        && cached.modified.is_some()
                        && !timestamps_equal(l.modified, cached.modified));
                let remote_changed = r.size != cached.size
                    || (r.modified.is_some()
                        && cached.modified.is_some()
                        && !timestamps_equal(r.modified, cached.modified));

                if local_changed && remote_changed {
                    // Both sides changed since last sync → true conflict
                    SyncStatus::Conflict
                } else if !local_changed && !remote_changed {
                    SyncStatus::Identical
                } else if local_changed {
                    SyncStatus::LocalNewer
                } else {
                    SyncStatus::RemoteNewer
                }
            } else {
                // File not in index → fall back to normal comparison
                compare_file_pair(local, remote, options)
            }
        } else {
            compare_file_pair(local, remote, options)
        };

        // Check if this file was in the sync index (previously synced).
        // For bidirectional sync: a local_only/remote_only file that was previously synced
        // means the OTHER side intentionally deleted it.
        let previously_synced = index
            .map(|idx| idx.files.contains_key(&path))
            .unwrap_or(false);

        if status != SyncStatus::Identical || is_dir {
            let sync_reason = generate_sync_reason(&status, local, remote, is_dir);
            results.push(FileComparison {
                relative_path: path,
                status,
                local_info: local.cloned(),
                remote_info: remote.cloned(),
                is_dir,
                sync_reason,
                previously_synced,
            });
        }
    }

    results.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    results
}

// ============ Phase 2: Error Taxonomy ============

/// Classification of sync errors for structured handling
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SyncErrorKind {
    /// Network connectivity issue (timeout, DNS, connection reset)
    Network,
    /// Authentication failure (invalid credentials, expired token)
    Auth,
    /// Path not found (file/directory doesn't exist)
    PathNotFound,
    /// Permission denied (insufficient privileges)
    PermissionDenied,
    /// Storage quota exceeded
    QuotaExceeded,
    /// Rate limit hit (too many requests)
    RateLimit,
    /// Operation timed out
    Timeout,
    /// File is locked or in use
    FileLocked,
    /// Disk full or I/O error
    DiskError,
    /// Unclassified error
    Unknown,
}

/// Structured sync error with classification and retry hint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncErrorInfo {
    pub kind: SyncErrorKind,
    pub message: String,
    pub retryable: bool,
    pub file_path: Option<String>,
}

/// Classify a raw error message into a structured SyncErrorInfo
pub fn classify_sync_error(raw: &str, file_path: Option<&str>) -> SyncErrorInfo {
    let lower = raw.to_lowercase();

    let (kind, retryable) = if lower.contains("timeout") || lower.contains("timed out") {
        (SyncErrorKind::Timeout, true)
    } else if lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("429")
    {
        (SyncErrorKind::RateLimit, true)
    } else if lower.contains("quota")
        || lower.contains("storage full")
        || lower.contains("insufficient storage")
        || lower.contains("552 ")
    {
        (SyncErrorKind::QuotaExceeded, false)
    } else if lower.contains("permission denied")
        || lower.contains("access denied")
        || lower.contains("403 ")
        || lower.contains("550 ")
    {
        (SyncErrorKind::PermissionDenied, false)
    } else if lower.contains("not found")
        || lower.contains("no such file")
        || lower.contains("404 ")
        || lower.contains("550 ")
    {
        // 550 can be either permission or not-found; prefer permission if already matched
        (SyncErrorKind::PathNotFound, false)
    } else if lower.contains("auth")
        || lower.contains("login")
        || lower.contains("credential")
        || lower.contains("401 ")
        || lower.contains("530 ")
    {
        (SyncErrorKind::Auth, false)
    } else if lower.contains("locked") || lower.contains("in use") {
        (SyncErrorKind::FileLocked, true)
    } else if lower.contains("disk full")
        || lower.contains("no space")
        || lower.contains("i/o error")
        || lower.contains("broken pipe")
    {
        (SyncErrorKind::DiskError, false)
    } else if lower.contains("connection")
        || lower.contains("network")
        || lower.contains("dns")
        || lower.contains("refused")
        || lower.contains("reset")
        || lower.contains("eof")
        || lower.contains("data connection")
    {
        (SyncErrorKind::Network, true)
    } else {
        (SyncErrorKind::Unknown, true)
    };

    SyncErrorInfo {
        kind,
        message: raw.to_string(),
        retryable,
        file_path: file_path.map(|s| s.to_string()),
    }
}

// ============ Phase 2: Retry Policy ============

/// Configurable retry policy for sync operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts per file
    pub max_retries: u32,
    /// Base delay between retries in milliseconds
    pub base_delay_ms: u64,
    /// Maximum delay cap in milliseconds
    pub max_delay_ms: u64,
    /// Per-file transfer timeout in milliseconds (0 = no timeout)
    pub timeout_ms: u64,
    /// Backoff multiplier (e.g. 2.0 for exponential)
    pub backoff_multiplier: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 500,
            max_delay_ms: 10_000,
            timeout_ms: 120_000, // 2 minutes per file
            backoff_multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    /// Calculate delay for a given attempt (1-indexed)
    #[allow(dead_code)] // Used in unit tests
    pub fn delay_for_attempt(&self, attempt: u32) -> u64 {
        let delay = (self.base_delay_ms as f64)
            * self
                .backoff_multiplier
                .powi(attempt.saturating_sub(1) as i32);
        if !delay.is_finite() || delay < 0.0 {
            return self.max_delay_ms;
        }
        (delay as u64).min(self.max_delay_ms)
    }
}

// ============ Phase 2: Post-Transfer Verification ============

/// Policy for verifying transfers after completion
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerifyPolicy {
    /// No verification (fastest)
    None,
    /// Verify file size matches
    #[default]
    SizeOnly,
    /// Verify size and modification time
    SizeAndMtime,
    /// Verify size + SHA-256 hash (slowest, most accurate)
    Full,
}

/// Result of a post-transfer verification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyResult {
    pub path: String,
    pub passed: bool,
    pub policy: VerifyPolicy,
    pub expected_size: u64,
    pub actual_size: Option<u64>,
    pub size_match: bool,
    pub mtime_match: Option<bool>,
    pub hash_match: Option<bool>,
    pub message: Option<String>,
}

/// Compute SHA-256 hash of a local file synchronously (64KB streaming chunks).
/// Returns lowercase hex-encoded hash string, or None on I/O error.
fn compute_sha256_sync(path: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 65_536];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// Verify a local file after download.
///
/// When `policy` is `Full`, performs SHA-256 hash verification in addition
/// to size and mtime checks. The `expected_hash` parameter should contain
/// the expected SHA-256 hex digest (e.g. from `FileInfo.checksum`).
/// If `expected_hash` is None under Full policy, hash verification is skipped
/// with an informational note.
pub fn verify_local_file(
    local_path: &str,
    expected_size: u64,
    expected_mtime: Option<DateTime<Utc>>,
    policy: &VerifyPolicy,
    expected_hash: Option<&str>,
) -> VerifyResult {
    let path = std::path::Path::new(local_path);
    let metadata = path.metadata().ok();

    let actual_size = metadata.as_ref().map(|m| m.len());
    let size_match = actual_size.map(|s| s == expected_size).unwrap_or(false);

    let mtime_match = if *policy == VerifyPolicy::SizeAndMtime || *policy == VerifyPolicy::Full {
        if let (Some(meta), Some(expected)) = (&metadata, expected_mtime) {
            meta.modified().ok().map(|t| {
                let actual: DateTime<Utc> = t.into();
                timestamps_equal(Some(actual), Some(expected))
            })
        } else {
            None
        }
    } else {
        None
    };

    // H8 fix: Full policy now performs SHA-256 hash verification
    let hash_match = if *policy == VerifyPolicy::Full && size_match {
        match expected_hash {
            Some(expected_hex) if !expected_hex.is_empty() => {
                // Compute actual hash and compare
                compute_sha256_sync(path)
                    .map(|actual_hex| actual_hex.eq_ignore_ascii_case(expected_hex))
            }
            _ => {
                // No expected hash available — cannot verify, treat as None (unknown)
                None
            }
        }
    } else {
        None
    };

    let passed = match policy {
        VerifyPolicy::None => true,
        VerifyPolicy::SizeOnly => size_match,
        VerifyPolicy::SizeAndMtime => size_match && mtime_match.unwrap_or(true),
        VerifyPolicy::Full => {
            size_match && mtime_match.unwrap_or(true) && hash_match.unwrap_or(true)
            // If no hash available, pass on size+mtime
        }
    };

    let message = if !passed {
        if !size_match {
            Some(format!(
                "Size mismatch: expected {} bytes, got {} bytes",
                expected_size,
                actual_size
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            ))
        } else if hash_match == Some(false) {
            Some("SHA-256 hash mismatch after transfer".to_string())
        } else if mtime_match == Some(false) {
            Some("Modification time mismatch after transfer".to_string())
        } else {
            Some("File not found after transfer".to_string())
        }
    } else if *policy == VerifyPolicy::Full && expected_hash.is_none() {
        Some("Full verification: hash check skipped (no expected hash available)".to_string())
    } else {
        None
    };

    VerifyResult {
        path: local_path.to_string(),
        passed,
        policy: policy.clone(),
        expected_size,
        actual_size,
        size_match,
        mtime_match,
        hash_match, // H8 fix: Now populated when VerifyPolicy::Full and expected_hash is provided
        message,
    }
}

// ============ Phase 2: Transfer Journal ============

/// Status of a single journal entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum JournalEntryStatus {
    /// Waiting to be processed
    Pending,
    /// Currently transferring
    InProgress,
    /// Completed successfully
    Completed,
    /// Failed after all retries
    Failed,
    /// Skipped by user or policy
    Skipped,
    /// Verification failed after transfer
    VerifyFailed,
}

/// A single entry in the transfer journal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncJournalEntry {
    pub relative_path: String,
    pub action: String, // "upload" | "download" | "mkdir"
    pub status: JournalEntryStatus,
    pub attempts: u32,
    pub last_error: Option<SyncErrorInfo>,
    pub verified: Option<bool>,
    pub bytes_transferred: u64,
}

/// Persistent transfer journal for checkpoint/resume
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncJournal {
    /// Unique journal ID
    pub id: String,
    /// When the journal was created
    pub created_at: DateTime<Utc>,
    /// When the journal was last updated
    pub updated_at: DateTime<Utc>,
    /// Local root path
    pub local_path: String,
    /// Remote root path
    pub remote_path: String,
    /// Sync direction
    pub direction: CompareDirection,
    /// Retry policy used
    pub retry_policy: RetryPolicy,
    /// Verify policy used
    pub verify_policy: VerifyPolicy,
    /// Ordered list of operations
    pub entries: Vec<SyncJournalEntry>,
    /// Whether the journal is complete (all entries processed)
    pub completed: bool,
}

impl SyncJournal {
    #[allow(dead_code)] // Used in unit tests
    pub fn new(
        local_path: String,
        remote_path: String,
        direction: CompareDirection,
        retry_policy: RetryPolicy,
        verify_policy: VerifyPolicy,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            local_path,
            remote_path,
            direction,
            retry_policy,
            verify_policy,
            entries: Vec::new(),
            completed: false,
        }
    }

    /// Count entries by status
    #[allow(dead_code)] // Used in unit tests
    pub fn count_by_status(&self, status: &JournalEntryStatus) -> usize {
        self.entries.iter().filter(|e| e.status == *status).count()
    }

    /// Check if there are pending or failed-retryable entries
    #[allow(dead_code)] // Used in unit tests
    pub fn has_resumable_entries(&self) -> bool {
        self.entries.iter().any(|e| {
            e.status == JournalEntryStatus::Pending
                || e.status == JournalEntryStatus::InProgress
                || (e.status == JournalEntryStatus::Failed
                    && e.last_error
                        .as_ref()
                        .map(|err| err.retryable)
                        .unwrap_or(true)
                    && e.attempts < self.retry_policy.max_retries)
        })
    }
}

/// Get the directory where sync journals are stored
fn sync_journal_dir() -> Result<PathBuf, String> {
    let base = dirs::config_dir().ok_or_else(|| "Cannot determine config directory".to_string())?;
    let dir = base.join("aeroftp").join("sync-journal");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create sync journal directory: {}", e))?;
    Ok(dir)
}

/// Generate a journal filename from path pair
fn journal_filename(local_path: &str, remote_path: &str) -> String {
    let combined = format!("{}|{}", local_path, remote_path);
    format!("journal_{}.json", stable_path_hash(&combined))
}

/// Load an existing journal for a path pair
pub fn load_sync_journal(
    local_path: &str,
    remote_path: &str,
) -> Result<Option<SyncJournal>, String> {
    let dir = sync_journal_dir()?;
    let path = dir.join(journal_filename(local_path, remote_path));
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read sync journal: {}", e))?;
    let journal: SyncJournal =
        serde_json::from_str(&data).map_err(|e| format!("Failed to parse sync journal: {}", e))?;
    Ok(Some(journal))
}

/// Save a journal (creates or overwrites). Uses a mutex to prevent concurrent write corruption (M38).
pub fn save_sync_journal(journal: &SyncJournal) -> Result<(), String> {
    let _lock = JOURNAL_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = sync_journal_dir()?;
    let path = dir.join(journal_filename(&journal.local_path, &journal.remote_path));
    let mut journal_to_save = journal.clone();
    journal_to_save.updated_at = Utc::now();
    let data = serde_json::to_string(&journal_to_save)
        .map_err(|e| format!("Failed to serialize sync journal: {}", e))?;
    atomic_write(&path, data.as_bytes())?;
    Ok(())
}

/// Delete a journal for a path pair
pub fn delete_sync_journal(local_path: &str, remote_path: &str) -> Result<(), String> {
    let dir = sync_journal_dir()?;
    let path = dir.join(journal_filename(local_path, remote_path));
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Failed to delete sync journal: {}", e))?;
    }
    Ok(())
}

/// Summary info for a stored journal
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalSummary {
    pub local_path: String,
    pub remote_path: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub total_entries: usize,
    pub completed_entries: usize,
    pub completed: bool,
}

/// List all sync journals with summary info
pub fn list_sync_journals() -> Result<Vec<JournalSummary>, String> {
    let dir = sync_journal_dir()?;
    let mut summaries = Vec::new();
    let entries =
        std::fs::read_dir(&dir).map_err(|e| format!("Failed to read journal directory: {}", e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(journal) = serde_json::from_str::<SyncJournal>(&data) {
                    let completed_entries = journal
                        .entries
                        .iter()
                        .filter(|e| e.status == JournalEntryStatus::Completed)
                        .count();
                    summaries.push(JournalSummary {
                        local_path: journal.local_path,
                        remote_path: journal.remote_path,
                        created_at: journal.created_at,
                        updated_at: journal.updated_at,
                        total_entries: journal.entries.len(),
                        completed_entries,
                        completed: journal.completed,
                    });
                }
            }
        }
    }
    summaries.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
    Ok(summaries)
}

/// Delete journals older than the given number of days.
/// Returns the number of journals deleted.
pub fn cleanup_old_journals(max_age_days: u32) -> Result<u32, String> {
    let dir = sync_journal_dir()?;
    let cutoff = Utc::now() - chrono::Duration::days(max_age_days as i64);
    let mut deleted = 0u32;
    let entries =
        std::fs::read_dir(&dir).map_err(|e| format!("Failed to read journal directory: {}", e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(journal) = serde_json::from_str::<SyncJournal>(&data) {
                    if journal.completed && journal.updated_at < cutoff {
                        let _ = std::fs::remove_file(&path);
                        deleted += 1;
                    }
                }
            }
        }
    }
    Ok(deleted)
}

/// Delete ALL sync journals (clear history).
/// Returns the number of journals deleted.
pub fn clear_all_journals() -> Result<u32, String> {
    let dir = sync_journal_dir()?;
    let mut deleted = 0u32;
    let entries =
        std::fs::read_dir(&dir).map_err(|e| format!("Failed to read journal directory: {}", e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            let _ = std::fs::remove_file(&path);
            deleted += 1;
        }
    }
    Ok(deleted)
}

// ============================================================================
// Sync Profiles — Named presets for sync configuration
// ============================================================================

/// A sync profile combines all sync settings into a named preset
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncProfile {
    pub id: String,
    pub name: String,
    pub builtin: bool,
    pub direction: CompareDirection,
    pub compare_timestamp: bool,
    pub compare_size: bool,
    pub compare_checksum: bool,
    pub exclude_patterns: Vec<String>,
    pub retry_policy: RetryPolicy,
    pub verify_policy: VerifyPolicy,
    pub delete_orphans: bool,
    /// Number of parallel transfer streams (1-8, default: 1 = sequential)
    #[serde(default = "default_parallel_streams")]
    pub parallel_streams: u8,
    /// Compression mode for transfers
    #[serde(default)]
    pub compression_mode: crate::transfer_pool::CompressionMode,
}

fn default_parallel_streams() -> u8 {
    1
}

impl SyncProfile {
    /// Mirror: local → remote, delete orphans on remote, verify size
    pub fn mirror() -> Self {
        Self {
            id: "mirror".to_string(),
            name: "Mirror".to_string(),
            builtin: true,
            direction: CompareDirection::LocalToRemote,
            compare_timestamp: true,
            compare_size: true,
            compare_checksum: false,
            exclude_patterns: vec![
                "node_modules".into(),
                ".git".into(),
                ".DS_Store".into(),
                "Thumbs.db".into(),
                "__pycache__".into(),
                "target".into(),
            ],
            retry_policy: RetryPolicy::default(),
            verify_policy: VerifyPolicy::SizeOnly,
            delete_orphans: true,
            parallel_streams: 3,
            compression_mode: crate::transfer_pool::CompressionMode::Off,
        }
    }

    /// Two-way: bidirectional, keep newer, no deletes
    pub fn two_way() -> Self {
        Self {
            id: "two_way".to_string(),
            name: "Two-way".to_string(),
            builtin: true,
            direction: CompareDirection::Bidirectional,
            compare_timestamp: true,
            compare_size: true,
            compare_checksum: false,
            exclude_patterns: vec![
                "node_modules".into(),
                ".git".into(),
                ".DS_Store".into(),
                "Thumbs.db".into(),
                "__pycache__".into(),
                "target".into(),
            ],
            retry_policy: RetryPolicy::default(),
            verify_policy: VerifyPolicy::SizeOnly,
            delete_orphans: false,
            parallel_streams: 3,
            compression_mode: crate::transfer_pool::CompressionMode::Off,
        }
    }

    /// Backup: local → remote, checksum verify, no deletes
    pub fn backup() -> Self {
        Self {
            id: "backup".to_string(),
            name: "Backup".to_string(),
            builtin: true,
            direction: CompareDirection::LocalToRemote,
            compare_timestamp: false,
            compare_size: true,
            compare_checksum: true,
            exclude_patterns: vec![
                "node_modules".into(),
                ".git".into(),
                ".DS_Store".into(),
                "Thumbs.db".into(),
                "__pycache__".into(),
                "target".into(),
            ],
            retry_policy: RetryPolicy {
                max_retries: 5,
                ..RetryPolicy::default()
            },
            verify_policy: VerifyPolicy::Full,
            delete_orphans: false,
            parallel_streams: 1,
            compression_mode: crate::transfer_pool::CompressionMode::Off,
        }
    }

    /// Pull: remote → local, delete local orphans, verify size (reverse mirror)
    pub fn pull() -> Self {
        Self {
            id: "pull".to_string(),
            name: "Pull".to_string(),
            builtin: true,
            direction: CompareDirection::RemoteToLocal,
            compare_timestamp: true,
            compare_size: true,
            compare_checksum: false,
            exclude_patterns: vec![
                "node_modules".into(),
                ".git".into(),
                ".DS_Store".into(),
                "Thumbs.db".into(),
                "__pycache__".into(),
                "target".into(),
            ],
            retry_policy: RetryPolicy::default(),
            verify_policy: VerifyPolicy::SizeOnly,
            delete_orphans: true,
            parallel_streams: 3,
            compression_mode: crate::transfer_pool::CompressionMode::Off,
        }
    }

    /// Remote Backup: remote → local, checksum verify, no deletes
    pub fn remote_backup() -> Self {
        Self {
            id: "remote_backup".to_string(),
            name: "Remote Backup".to_string(),
            builtin: true,
            direction: CompareDirection::RemoteToLocal,
            compare_timestamp: false,
            compare_size: true,
            compare_checksum: true,
            exclude_patterns: vec![
                "node_modules".into(),
                ".git".into(),
                ".DS_Store".into(),
                "Thumbs.db".into(),
                "__pycache__".into(),
                "target".into(),
            ],
            retry_policy: RetryPolicy {
                max_retries: 5,
                ..RetryPolicy::default()
            },
            verify_policy: VerifyPolicy::Full,
            delete_orphans: false,
            parallel_streams: 1,
            compression_mode: crate::transfer_pool::CompressionMode::Off,
        }
    }

    /// All built-in profiles
    pub fn builtins() -> Vec<Self> {
        vec![
            Self::mirror(),
            Self::two_way(),
            Self::backup(),
            Self::pull(),
            Self::remote_backup(),
        ]
    }
}

/// Directory for custom sync profiles
fn sync_profiles_dir() -> Result<PathBuf, String> {
    let base = dirs::config_dir().ok_or_else(|| "Cannot determine config directory".to_string())?;
    let dir = base.join("aeroftp").join("sync-profiles");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create sync profiles directory: {}", e))?;
    Ok(dir)
}

/// Load all profiles (built-in + custom)
pub fn load_sync_profiles() -> Result<Vec<SyncProfile>, String> {
    let mut profiles = SyncProfile::builtins();
    let dir = sync_profiles_dir()?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    if let Ok(profile) = serde_json::from_str::<SyncProfile>(&data) {
                        if !profile.builtin {
                            profiles.push(profile);
                        }
                    }
                }
            }
        }
    }
    Ok(profiles)
}

/// Validate that an ID is safe for use in filesystem paths (alphanumeric, hyphens, underscores only)
fn validate_filesystem_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 256 {
        return Err("Invalid ID length".to_string());
    }
    if id.contains('/') || id.contains('\\') || id.contains("..") || id.contains('\0') {
        return Err("ID contains forbidden characters".to_string());
    }
    // Only allow UUID-like chars: alphanumeric, hyphens, underscores
    if !id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err("ID contains invalid characters".to_string());
    }
    Ok(())
}

/// Save a custom profile using atomic write (temp + rename) to prevent corruption (M41)
pub fn save_sync_profile(profile: &SyncProfile) -> Result<(), String> {
    if profile.builtin {
        return Err("Cannot save built-in profiles".to_string());
    }
    validate_filesystem_id(&profile.id)?;
    let dir = sync_profiles_dir()?;
    let path = dir.join(format!("{}.json", profile.id));
    let data = serde_json::to_string(profile)
        .map_err(|e| format!("Failed to serialize sync profile: {}", e))?;
    atomic_write(&path, data.as_bytes())?;
    Ok(())
}

/// Delete a custom profile
pub fn delete_sync_profile(id: &str) -> Result<(), String> {
    validate_filesystem_id(id)?;
    let dir = sync_profiles_dir()?;
    let path = dir.join(format!("{}.json", id));
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Failed to delete sync profile: {}", e))?;
    }
    Ok(())
}

// =============================
// Multi-Path Sync (#52)
// =============================

/// A pair of local and remote paths for multi-path sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathPair {
    pub id: String,
    pub name: String,
    pub local_path: PathBuf,
    pub remote_path: String,
    pub enabled: bool,
    #[serde(default)]
    pub exclude_overrides: Vec<String>,
}

/// Configuration for multi-path sync
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MultiPathConfig {
    pub pairs: Vec<PathPair>,
    #[serde(default)]
    pub parallel_pairs: bool,
}

/// Load multi-path config from disk
pub fn load_multi_path_config() -> MultiPathConfig {
    let dir = dirs::config_dir().unwrap_or_default().join("aeroftp");
    let path = dir.join("multi_path.json");
    if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    } else {
        MultiPathConfig::default()
    }
}

/// Save multi-path config to disk (atomic temp+rename)
pub fn save_multi_path_config(config: &MultiPathConfig) -> Result<(), String> {
    let dir = dirs::config_dir().unwrap_or_default().join("aeroftp");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let data = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    atomic_write(&dir.join("multi_path.json"), data.as_bytes())
}

// =============================
// Sync Templates (#153)
// =============================

/// Shareable sync configuration template (.aerosync format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncTemplate {
    pub schema_version: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub created_by: String,
    /// Path patterns with variables ($HOME, $DOCUMENTS, $DESKTOP)
    pub path_patterns: Vec<TemplatePathPattern>,
    /// Embedded profile settings (without id/builtin)
    pub profile: SyncTemplateProfile,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    #[serde(default)]
    pub schedule: Option<crate::sync_scheduler::SyncSchedule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplatePathPattern {
    pub local: String,
    pub remote: String,
}

/// Profile settings embedded in a template (no credentials, no id)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncTemplateProfile {
    pub direction: CompareDirection,
    pub compare_timestamp: bool,
    pub compare_size: bool,
    pub compare_checksum: bool,
    pub delete_orphans: bool,
    #[serde(default = "default_parallel_streams")]
    pub parallel_streams: u8,
    #[serde(default)]
    pub compression_mode: crate::transfer_pool::CompressionMode,
}

/// Export current sync config as a template
pub fn export_sync_template(
    name: &str,
    description: &str,
    profile: &SyncProfile,
    local_path: &str,
    remote_path: &str,
    exclude_patterns: &[String],
    schedule: Option<&crate::sync_scheduler::SyncSchedule>,
) -> Result<SyncTemplate, String> {
    // Replace absolute paths with portable variables
    let local_portable = portable_path(local_path);

    Ok(SyncTemplate {
        schema_version: 1,
        name: name.to_string(),
        description: description.to_string(),
        created_by: format!("AeroFTP v{}", env!("CARGO_PKG_VERSION")),
        path_patterns: vec![TemplatePathPattern {
            local: local_portable,
            remote: remote_path.to_string(),
        }],
        profile: SyncTemplateProfile {
            direction: profile.direction,
            compare_timestamp: profile.compare_timestamp,
            compare_size: profile.compare_size,
            compare_checksum: profile.compare_checksum,
            delete_orphans: profile.delete_orphans,
            parallel_streams: profile.parallel_streams,
            compression_mode: profile.compression_mode.clone(),
        },
        exclude_patterns: exclude_patterns.to_vec(),
        schedule: schedule.cloned(),
    })
}

/// Replace absolute paths with portable variables
fn portable_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        let home_ref: &str = home_str.as_ref();
        if let Some(docs) = dirs::document_dir() {
            let docs_str = docs.to_string_lossy();
            let docs_ref: &str = docs_str.as_ref();
            if path.starts_with(docs_ref) {
                return path.replacen(docs_ref, "$DOCUMENTS", 1);
            }
        }
        if let Some(desktop) = dirs::desktop_dir() {
            let desk_str = desktop.to_string_lossy();
            let desk_ref: &str = desk_str.as_ref();
            if path.starts_with(desk_ref) {
                return path.replacen(desk_ref, "$DESKTOP", 1);
            }
        }
        if path.starts_with(home_ref) {
            return path.replacen(home_ref, "$HOME", 1);
        }
    }
    path.to_string()
}

// =============================
// Metadata-Aware Rollback (#154)
// =============================

/// Pre-sync snapshot for rollback capability
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSnapshot {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub local_path: String,
    pub remote_path: String,
    pub files: HashMap<String, FileSnapshotEntry>,
}

/// Per-file state captured in a snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshotEntry {
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
    pub checksum: Option<String>,
    pub action_taken: String,
}

/// Create a pre-sync snapshot from the current sync index
pub fn create_sync_snapshot(
    local_path: &str,
    remote_path: &str,
    index: &SyncIndex,
) -> SyncSnapshot {
    let files: HashMap<String, FileSnapshotEntry> = index
        .files
        .iter()
        .filter(|(_, entry)| !entry.is_dir)
        .map(|(path, entry)| {
            (
                path.clone(),
                FileSnapshotEntry {
                    size: entry.size,
                    modified: entry.modified,
                    checksum: None,
                    action_taken: String::new(),
                },
            )
        })
        .collect();

    SyncSnapshot {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: Utc::now(),
        local_path: local_path.to_string(),
        remote_path: remote_path.to_string(),
        files,
    }
}

/// Directory where snapshots are stored
fn snapshots_dir() -> Result<PathBuf, String> {
    let dir = dirs::config_dir()
        .unwrap_or_default()
        .join("aeroftp")
        .join("sync-snapshots");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Save a snapshot to disk
pub fn save_sync_snapshot(snapshot: &SyncSnapshot) -> Result<(), String> {
    let dir = snapshots_dir()?;
    let data = serde_json::to_string(snapshot).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{}.json", snapshot.id)), data).map_err(|e| e.to_string())
}

/// List all snapshots, sorted by date (newest first), max 10
pub fn list_sync_snapshots() -> Result<Vec<SyncSnapshot>, String> {
    let dir = snapshots_dir()?;
    let mut snapshots: Vec<SyncSnapshot> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            std::fs::read_to_string(entry.path())
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        })
        .collect();
    snapshots.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    snapshots.truncate(10);

    // Cleanup: keep only last 5 snapshots on disk
    let all_files: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    if all_files.len() > 5 {
        let mut by_time: Vec<_> = all_files
            .into_iter()
            .filter_map(|e| {
                e.metadata().ok().map(|m| {
                    (
                        e.path(),
                        m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                    )
                })
            })
            .collect();
        by_time.sort_by_key(|b| std::cmp::Reverse(b.1));
        for (path, _) in by_time.into_iter().skip(5) {
            let _ = std::fs::remove_file(path);
        }
    }

    Ok(snapshots)
}

/// Delete a specific snapshot by ID
pub fn delete_sync_snapshot(id: &str) -> Result<(), String> {
    validate_filesystem_id(id)?;
    let dir = snapshots_dir()?;
    let path = dir.join(format!("{}.json", id));
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Load a specific snapshot by ID
pub fn load_sync_snapshot(id: &str) -> Result<SyncSnapshot, String> {
    validate_filesystem_id(id)?;
    let dir = snapshots_dir()?;
    let path = dir.join(format!("{}.json", id));
    let data =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read snapshot: {}", e))?;
    serde_json::from_str(&data).map_err(|e| format!("Failed to parse snapshot: {}", e))
}

// ============================================================================
// Canary Sync — Sample-based dry-run analysis
// ============================================================================

/// Configuration for canary (sample) sync
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryConfig {
    /// Percentage of files to sample (5-50, default 10)
    pub percent: u8,
    /// Selection strategy: "random", "newest", "largest"
    pub selection: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanarySampleResult {
    pub relative_path: String,
    pub action: String, // "upload", "download", "delete"
    pub success: bool,
    pub error: Option<String>,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanarySummary {
    pub would_upload: usize,
    pub would_download: usize,
    pub would_delete: usize,
    pub conflicts: usize,
    pub errors: usize,
    pub estimated_transfer_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryResult {
    pub sampled_files: usize,
    pub total_files: usize,
    pub results: Vec<CanarySampleResult>,
    pub summary: CanarySummary,
}

/// Select a sample of files based on the given strategy.
/// Returns the selected files as (relative_path, FileInfo) tuples.
pub fn select_canary_sample(
    files: &HashMap<String, FileInfo>,
    sample_size: usize,
    selection: &str,
) -> Vec<(String, FileInfo)> {
    let mut file_list: Vec<(String, FileInfo)> = files
        .iter()
        .filter(|(_, info)| !info.is_dir)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    if file_list.is_empty() {
        return Vec::new();
    }

    match selection {
        "newest" => {
            file_list.sort_by(|a, b| {
                let a_mod = a.1.modified.unwrap_or(chrono::DateTime::<Utc>::MIN_UTC);
                let b_mod = b.1.modified.unwrap_or(chrono::DateTime::<Utc>::MIN_UTC);
                b_mod.cmp(&a_mod)
            });
        }
        "largest" => {
            file_list.sort_by_key(|b| std::cmp::Reverse(b.1.size));
        }
        _ => {
            // "random" or default: shuffle using Fisher-Yates via rand
            use rand::seq::SliceRandom;
            let mut rng = rand::thread_rng();
            file_list.shuffle(&mut rng);
        }
    }

    file_list.truncate(sample_size);
    file_list
}

// ============================================================================
// Signed Audit Log — HMAC-SHA256 journal signing and verification
// ============================================================================

/// Sign a sync journal with HMAC-SHA256
pub fn sign_journal(journal: &SyncJournal, key: &[u8]) -> Result<String, String> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let canonical = serde_json::to_string(journal)
        .map_err(|e| format!("Failed to serialize journal: {}", e))?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).map_err(|e| format!("HMAC key error: {}", e))?;
    mac.update(canonical.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

/// Generate the .sig filename for a journal path pair
pub fn journal_sig_filename(local_path: &str, remote_path: &str) -> String {
    let combined = format!("{}|{}", local_path, remote_path);
    format!("journal_{}.sig", stable_path_hash(&combined))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_exclude() {
        let patterns = vec!["node_modules".to_string(), "*.pyc".to_string()];

        assert!(should_exclude("node_modules/package/file.js", &patterns));
        assert!(should_exclude("src/__pycache__/module.pyc", &patterns));
        assert!(!should_exclude("src/main.rs", &patterns));
    }

    #[test]
    fn test_compare_file_pair_local_only() {
        let local = FileInfo {
            name: "test.txt".to_string(),
            path: "/local/test.txt".to_string(),
            size: 100,
            modified: Some(Utc::now()),
            is_dir: false,
            checksum: None,
        };

        let options = CompareOptions::default();
        let status = compare_file_pair(Some(&local), None, &options);

        assert_eq!(status, SyncStatus::LocalOnly);
    }

    #[test]
    fn test_classify_sync_error_network() {
        let err = classify_sync_error("Connection refused by remote host", Some("test.txt"));
        assert_eq!(err.kind, SyncErrorKind::Network);
        assert!(err.retryable);
    }

    #[test]
    fn test_classify_sync_error_timeout() {
        let err = classify_sync_error("Operation timed out after 30s", None);
        assert_eq!(err.kind, SyncErrorKind::Timeout);
        assert!(err.retryable);
    }

    #[test]
    fn test_classify_sync_error_quota() {
        let err = classify_sync_error("552 Insufficient storage space", Some("/path"));
        assert_eq!(err.kind, SyncErrorKind::QuotaExceeded);
        assert!(!err.retryable);
    }

    #[test]
    fn test_classify_sync_error_rate_limit() {
        let err = classify_sync_error("429 Too Many Requests", None);
        assert_eq!(err.kind, SyncErrorKind::RateLimit);
        assert!(err.retryable);
    }

    #[test]
    fn test_classify_sync_error_auth() {
        let err = classify_sync_error("530 Login authentication failed", None);
        assert_eq!(err.kind, SyncErrorKind::Auth);
        assert!(!err.retryable);
    }

    #[test]
    fn test_retry_policy_delay() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.delay_for_attempt(1), 500);
        assert_eq!(policy.delay_for_attempt(2), 1000);
        assert_eq!(policy.delay_for_attempt(3), 2000);
    }

    #[test]
    fn test_retry_policy_max_cap() {
        let policy = RetryPolicy {
            max_retries: 10,
            base_delay_ms: 1000,
            max_delay_ms: 5000,
            timeout_ms: 60_000,
            backoff_multiplier: 3.0,
        };
        // 1000 * 3^4 = 81000, capped at 5000
        assert_eq!(policy.delay_for_attempt(5), 5000);
    }

    #[test]
    fn test_verify_local_file_missing() {
        let result = verify_local_file(
            "/nonexistent/path/file.txt",
            100,
            None,
            &VerifyPolicy::SizeOnly,
            None,
        );
        assert!(!result.passed);
        assert!(!result.size_match);
    }

    #[test]
    fn test_journal_has_resumable() {
        let mut journal = SyncJournal::new(
            "/local".to_string(),
            "/remote".to_string(),
            CompareDirection::Bidirectional,
            RetryPolicy::default(),
            VerifyPolicy::default(),
        );
        journal.entries.push(SyncJournalEntry {
            relative_path: "file.txt".to_string(),
            action: "upload".to_string(),
            status: JournalEntryStatus::Pending,
            attempts: 0,
            last_error: None,
            verified: None,
            bytes_transferred: 0,
        });
        assert!(journal.has_resumable_entries());

        // Mark as completed
        journal.entries[0].status = JournalEntryStatus::Completed;
        assert!(!journal.has_resumable_entries());
    }

    #[test]
    fn test_journal_count_by_status() {
        let mut journal = SyncJournal::new(
            "/a".to_string(),
            "/b".to_string(),
            CompareDirection::LocalToRemote,
            RetryPolicy::default(),
            VerifyPolicy::default(),
        );
        journal.entries.push(SyncJournalEntry {
            relative_path: "a.txt".to_string(),
            action: "upload".to_string(),
            status: JournalEntryStatus::Completed,
            attempts: 1,
            last_error: None,
            verified: Some(true),
            bytes_transferred: 1024,
        });
        journal.entries.push(SyncJournalEntry {
            relative_path: "b.txt".to_string(),
            action: "upload".to_string(),
            status: JournalEntryStatus::Failed,
            attempts: 3,
            last_error: None,
            verified: None,
            bytes_transferred: 0,
        });
        assert_eq!(journal.count_by_status(&JournalEntryStatus::Completed), 1);
        assert_eq!(journal.count_by_status(&JournalEntryStatus::Failed), 1);
        assert_eq!(journal.count_by_status(&JournalEntryStatus::Pending), 0);
    }

    #[test]
    fn test_path_pair_serialization() {
        let pair = PathPair {
            id: "test-1".to_string(),
            name: "Documents".to_string(),
            local_path: PathBuf::from("/home/user/docs"),
            remote_path: "/remote/docs".to_string(),
            enabled: true,
            exclude_overrides: vec!["*.tmp".to_string()],
        };
        let json = serde_json::to_string(&pair).unwrap();
        let deserialized: PathPair = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "test-1");
        assert!(deserialized.enabled);
        assert_eq!(deserialized.exclude_overrides.len(), 1);
    }

    #[test]
    fn test_multi_path_config_default() {
        let config = MultiPathConfig::default();
        assert!(config.pairs.is_empty());
        assert!(!config.parallel_pairs);
    }

    #[test]
    fn test_portable_path_home() {
        if let Some(home) = dirs::home_dir() {
            let abs = format!("{}/projects/test", home.to_string_lossy());
            let portable = portable_path(&abs);
            assert!(
                portable.starts_with("$HOME")
                    || portable.starts_with("$DOCUMENTS")
                    || portable.starts_with("$DESKTOP")
            );
        }
    }

    #[test]
    fn test_portable_path_no_match() {
        let abs = "/tmp/random/path";
        let portable = portable_path(abs);
        assert_eq!(portable, abs);
    }

    #[test]
    fn test_sync_snapshot_creation() {
        let mut index = SyncIndex {
            version: 1,
            last_sync: Utc::now(),
            local_path: "/local".to_string(),
            remote_path: "/remote".to_string(),
            files: HashMap::new(),
        };
        index.files.insert(
            "file.txt".to_string(),
            SyncIndexEntry {
                size: 1024,
                modified: Some(Utc::now()),
                is_dir: false,
            },
        );

        let snapshot = create_sync_snapshot("/local", "/remote", &index);
        assert_eq!(snapshot.files.len(), 1);
        assert!(snapshot.files.contains_key("file.txt"));
        assert_eq!(snapshot.files["file.txt"].size, 1024);
        assert!(!snapshot.id.is_empty());
    }

    #[test]
    fn test_sync_template_export() {
        let profile = SyncProfile::mirror();
        let template = export_sync_template(
            "Test Template",
            "A test",
            &profile,
            "/home/user/docs",
            "/remote/docs",
            &["*.tmp".to_string()],
            None,
        )
        .unwrap();
        assert_eq!(template.schema_version, 1);
        assert_eq!(template.name, "Test Template");
        assert_eq!(template.path_patterns.len(), 1);
        assert!(template.schedule.is_none());
        assert!(template.created_by.contains("AeroFTP"));
    }

    #[test]
    fn test_parse_sync_tree_direction_accepts_common_aliases() {
        assert_eq!(SyncDirection::parse("upload"), Some(SyncDirection::Upload));
        assert_eq!(SyncDirection::parse("push"), Some(SyncDirection::Upload));
        assert_eq!(SyncDirection::parse("pull"), Some(SyncDirection::Download));
        assert_eq!(SyncDirection::parse("both"), Some(SyncDirection::Both));
        assert_eq!(SyncDirection::parse("wat"), None);
    }

    #[test]
    fn test_parse_sync_tree_conflict_mode() {
        assert_eq!(ConflictMode::parse("larger"), Some(ConflictMode::Larger));
        assert_eq!(ConflictMode::parse("skip"), Some(ConflictMode::Skip));
        assert_eq!(ConflictMode::parse("newer"), Some(ConflictMode::Newer));
        assert_eq!(ConflictMode::parse("foo"), None);
    }

    #[test]
    fn test_join_clean_handles_trailing_slash_and_leading_slash_on_rel() {
        assert_eq!(join_clean("/base", "/rel.txt"), "/base/rel.txt");
        assert_eq!(join_clean("/base/", "rel.txt"), "/base/rel.txt");
        assert_eq!(join_clean("", "rel.txt"), "rel.txt");
    }

    #[test]
    fn test_join_clean_remote_keeps_leading_slash() {
        assert_eq!(join_clean_remote("/", "rel.txt"), "/rel.txt");
        assert_eq!(join_clean_remote("/foo", "rel.txt"), "/foo/rel.txt");
        assert_eq!(join_clean_remote("/foo/", "/rel.txt"), "/foo/rel.txt");
    }

    #[test]
    fn test_remote_dir_chain_builds_absolute_segments() {
        assert_eq!(
            remote_dir_chain("/www.aeroftp.app/playground/run"),
            vec![
                "/www.aeroftp.app".to_string(),
                "/www.aeroftp.app/playground".to_string(),
                "/www.aeroftp.app/playground/run".to_string(),
            ]
        );
    }

    #[test]
    fn test_remote_dir_chain_ignores_root_and_trailing_slash() {
        assert!(remote_dir_chain("/").is_empty());
        assert_eq!(
            remote_dir_chain("/foo/bar/"),
            vec!["/foo".to_string(), "/foo/bar".to_string()]
        );
    }

    fn local_entry(size: u64, mtime: Option<&str>, sha256: Option<&str>) -> crate::sync_core::LocalEntry {
        crate::sync_core::LocalEntry {
            rel_path: "file.txt".to_string(),
            size,
            mtime: mtime.map(str::to_string),
            sha256: sha256.map(str::to_string),
        }
    }

    fn remote_entry(
        size: u64,
        mtime: Option<&str>,
        checksum_hex: Option<&str>,
    ) -> crate::sync_core::RemoteEntry {
        crate::sync_core::RemoteEntry {
            rel_path: "file.txt".to_string(),
            size,
            mtime: mtime.map(str::to_string),
            checksum_alg: checksum_hex.map(|_| "sha256".to_string()),
            checksum_hex: checksum_hex.map(str::to_string),
        }
    }

    #[test]
    fn test_decide_upload_copies_missing_remote() {
        let local = local_entry(10, Some("2026-04-21T10:00:00"), None);
        let decision = decide_upload(&local, None, DeltaPolicy::Mtime, ConflictMode::Larger);
        assert!(matches!(decision.action, SyncTreeAction::Copy));
        assert_eq!(decision.decision_policy, DeltaPolicy::Mtime);
    }

    #[test]
    fn test_decide_upload_skips_same_size() {
        let local = local_entry(10, Some("2026-04-21T10:00:00"), None);
        let remote = remote_entry(10, None, None);
        let decision = decide_upload(&local, Some(&remote), DeltaPolicy::SizeOnly, ConflictMode::Larger);
        assert!(matches!(decision.action, SyncTreeAction::Skip(_)));
        assert_eq!(decision.decision_policy, DeltaPolicy::SizeOnly);
    }

    #[test]
    fn test_decide_upload_larger_mode_picks_larger_side() {
        let larger_local = local_entry(20, None, None);
        let smaller_local = local_entry(5, None, None);
        let remote = remote_entry(10, None, None);

        assert!(matches!(
            decide_upload(&larger_local, Some(&remote), DeltaPolicy::SizeOnly, ConflictMode::Larger)
                .action,
            SyncTreeAction::Copy
        ));
        assert!(matches!(
            decide_upload(&smaller_local, Some(&remote), DeltaPolicy::SizeOnly, ConflictMode::Larger)
                .action,
            SyncTreeAction::Skip(_)
        ));
    }

    #[test]
    fn test_decide_download_respects_both_direction_dedup() {
        let local = local_entry(10, None, None);
        let remote = remote_entry(10, None, None);
        let decision = decide_download(
            &remote,
            Some(&local),
            DeltaPolicy::Mtime,
            ConflictMode::Larger,
            true,
        );
        assert!(matches!(decision.action, SyncTreeAction::Skip(_)));
    }

    #[test]
    fn test_parse_delta_policy() {
        assert_eq!(DeltaPolicy::parse("size_only"), Some(DeltaPolicy::SizeOnly));
        assert_eq!(DeltaPolicy::parse("mtime"), Some(DeltaPolicy::Mtime));
        assert_eq!(DeltaPolicy::parse("hash"), Some(DeltaPolicy::Hash));
        assert_eq!(DeltaPolicy::parse("delta"), Some(DeltaPolicy::Delta));
        assert_eq!(DeltaPolicy::parse("wat"), None);
    }

    #[test]
    fn test_decide_upload_mtime_prefers_newer_local_even_if_size_matches() {
        let local = local_entry(10, Some("2026-04-22T10:00:00"), None);
        let remote = remote_entry(10, Some("2026-04-21T10:00:00"), None);

        let mtime_decision = decide_upload(&local, Some(&remote), DeltaPolicy::Mtime, ConflictMode::Larger);
        let size_decision = decide_upload(&local, Some(&remote), DeltaPolicy::SizeOnly, ConflictMode::Larger);

        assert!(matches!(mtime_decision.action, SyncTreeAction::Copy));
        assert_eq!(mtime_decision.decision_policy, DeltaPolicy::Mtime);
        assert!(matches!(size_decision.action, SyncTreeAction::Skip(_)));
        assert_eq!(size_decision.decision_policy, DeltaPolicy::SizeOnly);
    }

    #[test]
    fn test_decide_upload_hash_skips_identical_checksum_even_if_mtime_differs() {
        let local = local_entry(
            10,
            Some("2026-04-22T10:00:00"),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let remote = remote_entry(
            10,
            Some("2026-04-21T10:00:00"),
            Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        );

        let decision = decide_upload(&local, Some(&remote), DeltaPolicy::Hash, ConflictMode::Larger);
        assert!(matches!(decision.action, SyncTreeAction::Skip(_)));
        assert_eq!(decision.decision_policy, DeltaPolicy::Hash);
    }

    #[test]
    fn test_decide_upload_hash_falls_back_to_mtime_without_checksums() {
        let local = local_entry(10, Some("2026-04-22 10:00:00"), None);
        let remote = remote_entry(10, Some("2026-04-21T10:00:00Z"), None);

        let decision = decide_upload(&local, Some(&remote), DeltaPolicy::Hash, ConflictMode::Larger);
        assert!(matches!(decision.action, SyncTreeAction::Copy));
        assert_eq!(decision.decision_policy, DeltaPolicy::Mtime);
    }

    #[test]
    fn sync_transfer_spec_separates_requested_and_decision_policy() {
        // P1-T01 invariant: `requested_policy` (caller intent) and
        // `decision_policy` (decide layer outcome) are independent fields.
        // The native delta wrapper consults `requested_policy`; the sync
        // report and progress sink consume `decision_policy`. Without this
        // separation a `Hash`-requested transfer that the decide layer
        // downgrades to `Mtime` would silently bypass the native path.
        let spec = SyncTransferSpec {
            rel: "alpha.txt",
            total: 1234,
            decision_policy: DeltaPolicy::Mtime,
            requested_policy: DeltaPolicy::Delta,
        };
        assert_eq!(spec.requested_policy, DeltaPolicy::Delta);
        assert_eq!(spec.decision_policy, DeltaPolicy::Mtime);
        assert_ne!(spec.requested_policy, spec.decision_policy);
    }
}
