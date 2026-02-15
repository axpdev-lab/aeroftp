// AeroFTP Sync Module
// File comparison and synchronization logic

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Tolerance for timestamp comparison (seconds)
/// Accounts for filesystem and timezone differences
const TIMESTAMP_TOLERANCE_SECS: i64 = 2;

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
    pub direction: SyncDirection,
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
            direction: SyncDirection::Bidirectional,
        }
    }
}

/// Direction of synchronization
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    /// Local -> Remote (upload changes)
    LocalToRemote,
    /// Remote -> Local (download changes)
    RemoteToLocal,
    /// Both directions (full sync)
    Bidirectional,
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

/// Check if a path matches any exclude pattern
pub fn should_exclude(path: &str, patterns: &[String]) -> bool {
    let path_lower = path.to_lowercase();
    let path_segments: Vec<&str> = path_lower.split(&['/', '\\'][..]).collect();
    
    for pattern in patterns {
        let pattern_lower = pattern.to_lowercase();
        
        // Simple glob matching
        if pattern_lower.starts_with('*') {
            // *.ext pattern
            let ext = &pattern_lower[1..];
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

/// Compare two timestamps with tolerance
pub fn timestamps_equal(local: Option<DateTime<Utc>>, remote: Option<DateTime<Utc>>) -> bool {
    match (local, remote) {
        (Some(l), Some(r)) => {
            (l.signed_duration_since(r)).num_seconds().abs() <= TIMESTAMP_TOLERANCE_SECS
        }
        _ => false,
    }
}

/// Determine which timestamp is newer
pub fn compare_timestamps(local: Option<DateTime<Utc>>, remote: Option<DateTime<Utc>>) -> Option<SyncStatus> {
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

            // First check size if enabled
            if options.compare_size && l.size != r.size {
                // Different sizes - determine which is newer
                if options.compare_timestamp {
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
                if timestamps_equal(l.modified, r.modified) {
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
        // Skip excluded paths
        if should_exclude(&path, &options.exclude_patterns) {
            continue;
        }
        
        let local = local_files.get(&path);
        let remote = remote_files.get(&path);
        
        let status = compare_file_pair(local, remote, options);
        
        // Skip identical files unless they're directories we need to show
        let is_dir = local.map(|f| f.is_dir).unwrap_or(false) 
                  || remote.map(|f| f.is_dir).unwrap_or(false);
        
        if status != SyncStatus::Identical || is_dir {
            results.push(FileComparison {
                relative_path: path,
                status,
                local_info: local.cloned(),
                remote_info: remote.cloned(),
                is_dir,
            });
        }
    }
    
    // Sort by path for consistent display
    results.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    
    results
}

/// Determine the recommended action based on comparison status and direction
#[allow(dead_code)]
pub fn get_recommended_action(status: &SyncStatus, direction: &SyncDirection) -> SyncAction {
    match (status, direction) {
        // Bidirectional
        (SyncStatus::LocalNewer, SyncDirection::Bidirectional) => SyncAction::Upload,
        (SyncStatus::RemoteNewer, SyncDirection::Bidirectional) => SyncAction::Download,
        (SyncStatus::LocalOnly, SyncDirection::Bidirectional) => SyncAction::Upload,
        (SyncStatus::RemoteOnly, SyncDirection::Bidirectional) => SyncAction::Download,
        (SyncStatus::Conflict, _) => SyncAction::AskUser,
        (SyncStatus::SizeMismatch, _) => SyncAction::AskUser,
        
        // Local to Remote
        (SyncStatus::LocalNewer, SyncDirection::LocalToRemote) => SyncAction::Upload,
        (SyncStatus::LocalOnly, SyncDirection::LocalToRemote) => SyncAction::Upload,
        (SyncStatus::RemoteNewer, SyncDirection::LocalToRemote) => SyncAction::Skip,
        (SyncStatus::RemoteOnly, SyncDirection::LocalToRemote) => SyncAction::DeleteRemote,
        
        // Remote to Local
        (SyncStatus::RemoteNewer, SyncDirection::RemoteToLocal) => SyncAction::Download,
        (SyncStatus::RemoteOnly, SyncDirection::RemoteToLocal) => SyncAction::Download,
        (SyncStatus::LocalNewer, SyncDirection::RemoteToLocal) => SyncAction::Skip,
        (SyncStatus::LocalOnly, SyncDirection::RemoteToLocal) => SyncAction::DeleteLocal,
        
        // Identical - no action needed
        (SyncStatus::Identical, _) => SyncAction::Skip,
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

/// Get the directory where sync indices are stored
fn sync_index_dir() -> Result<std::path::PathBuf, String> {
    let base = dirs::config_dir()
        .ok_or_else(|| "Cannot determine config directory".to_string())?;
    let dir = base.join("aeroftp").join("sync-index");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create sync index directory: {}", e))?;
    Ok(dir)
}

/// Generate a stable filename from a local+remote path pair
fn index_filename(local_path: &str, remote_path: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    local_path.hash(&mut hasher);
    remote_path.hash(&mut hasher);
    format!("{:016x}.json", hasher.finish())
}

/// Load a sync index for a given path pair (returns None if not found)
pub fn load_sync_index(local_path: &str, remote_path: &str) -> Result<Option<SyncIndex>, String> {
    let dir = sync_index_dir()?;
    let path = dir.join(index_filename(local_path, remote_path));
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read sync index: {}", e))?;
    let index: SyncIndex = serde_json::from_str(&data)
        .map_err(|e| format!("Failed to parse sync index: {}", e))?;
    Ok(Some(index))
}

/// Save a sync index for a given path pair
pub fn save_sync_index(index: &SyncIndex) -> Result<(), String> {
    let dir = sync_index_dir()?;
    let path = dir.join(index_filename(&index.local_path, &index.remote_path));
    let data = serde_json::to_string(index)
        .map_err(|e| format!("Failed to serialize sync index: {}", e))?;
    std::fs::write(&path, data)
        .map_err(|e| format!("Failed to write sync index: {}", e))?;
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
        if should_exclude(&path, &options.exclude_patterns) {
            continue;
        }

        let local = local_files.get(&path);
        let remote = remote_files.get(&path);
        let is_dir = local.map(|f| f.is_dir).unwrap_or(false)
            || remote.map(|f| f.is_dir).unwrap_or(false);

        // Check if we can use the index for conflict detection
        let status = if let (Some(idx), Some(l), Some(r)) = (index, local, remote) {
            if let Some(cached) = idx.files.get(&path) {
                let local_changed = l.size != cached.size
                    || !timestamps_equal(l.modified, cached.modified);
                let remote_changed = r.size != cached.size
                    || !timestamps_equal(r.modified, cached.modified);

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

        if status != SyncStatus::Identical || is_dir {
            results.push(FileComparison {
                relative_path: path,
                status,
                local_info: local.cloned(),
                remote_info: remote.cloned(),
                is_dir,
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
    } else if lower.contains("rate limit") || lower.contains("too many requests") || lower.contains("429") {
        (SyncErrorKind::RateLimit, true)
    } else if lower.contains("quota") || lower.contains("storage full") || lower.contains("insufficient storage")
        || lower.contains("552 ")
    {
        (SyncErrorKind::QuotaExceeded, false)
    } else if lower.contains("permission denied") || lower.contains("access denied")
        || lower.contains("403 ") || lower.contains("550 ")
    {
        (SyncErrorKind::PermissionDenied, false)
    } else if lower.contains("not found") || lower.contains("no such file")
        || lower.contains("404 ") || lower.contains("550 ")
    {
        // 550 can be either permission or not-found; prefer permission if already matched
        (SyncErrorKind::PathNotFound, false)
    } else if lower.contains("auth") || lower.contains("login") || lower.contains("credential")
        || lower.contains("401 ") || lower.contains("530 ")
    {
        (SyncErrorKind::Auth, false)
    } else if lower.contains("locked") || lower.contains("in use") {
        (SyncErrorKind::FileLocked, true)
    } else if lower.contains("disk full") || lower.contains("no space")
        || lower.contains("i/o error") || lower.contains("broken pipe")
    {
        (SyncErrorKind::DiskError, false)
    } else if lower.contains("connection") || lower.contains("network")
        || lower.contains("dns") || lower.contains("refused")
        || lower.contains("reset") || lower.contains("eof")
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
    pub fn delay_for_attempt(&self, attempt: u32) -> u64 {
        let delay = (self.base_delay_ms as f64) * self.backoff_multiplier.powi(attempt.saturating_sub(1) as i32);
        (delay as u64).min(self.max_delay_ms)
    }
}

// ============ Phase 2: Post-Transfer Verification ============

/// Policy for verifying transfers after completion
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerifyPolicy {
    /// No verification (fastest)
    None,
    /// Verify file size matches
    SizeOnly,
    /// Verify size and modification time
    SizeAndMtime,
    /// Verify size + SHA-256 hash (slowest, most accurate)
    Full,
}

impl Default for VerifyPolicy {
    fn default() -> Self {
        Self::SizeOnly
    }
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

/// Verify a local file after download
pub fn verify_local_file(
    local_path: &str,
    expected_size: u64,
    expected_mtime: Option<DateTime<Utc>>,
    policy: &VerifyPolicy,
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

    let passed = match policy {
        VerifyPolicy::None => true,
        VerifyPolicy::SizeOnly => size_match,
        VerifyPolicy::SizeAndMtime => size_match && mtime_match.unwrap_or(true),
        VerifyPolicy::Full => size_match && mtime_match.unwrap_or(true),
    };

    let message = if !passed {
        if !size_match {
            Some(format!(
                "Size mismatch: expected {} bytes, got {} bytes",
                expected_size,
                actual_size.map(|s| s.to_string()).unwrap_or_else(|| "unknown".to_string())
            ))
        } else if mtime_match == Some(false) {
            Some("Modification time mismatch after transfer".to_string())
        } else {
            Some("File not found after transfer".to_string())
        }
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
        hash_match: None, // Hash verification is done on-demand via separate command
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
    pub direction: SyncDirection,
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
    pub fn new(
        local_path: String,
        remote_path: String,
        direction: SyncDirection,
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
    pub fn count_by_status(&self, status: &JournalEntryStatus) -> usize {
        self.entries.iter().filter(|e| e.status == *status).count()
    }

    /// Check if there are pending or failed-retryable entries
    pub fn has_resumable_entries(&self) -> bool {
        self.entries.iter().any(|e| {
            e.status == JournalEntryStatus::Pending
                || e.status == JournalEntryStatus::InProgress
                || (e.status == JournalEntryStatus::Failed
                    && e.last_error.as_ref().map(|err| err.retryable).unwrap_or(true)
                    && e.attempts < self.retry_policy.max_retries)
        })
    }
}

/// Get the directory where sync journals are stored
fn sync_journal_dir() -> Result<PathBuf, String> {
    let base = dirs::config_dir()
        .ok_or_else(|| "Cannot determine config directory".to_string())?;
    let dir = base.join("aeroftp").join("sync-journal");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create sync journal directory: {}", e))?;
    Ok(dir)
}

/// Generate a journal filename from path pair
fn journal_filename(local_path: &str, remote_path: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    local_path.hash(&mut hasher);
    remote_path.hash(&mut hasher);
    format!("journal_{:016x}.json", hasher.finish())
}

/// Load an existing journal for a path pair
pub fn load_sync_journal(local_path: &str, remote_path: &str) -> Result<Option<SyncJournal>, String> {
    let dir = sync_journal_dir()?;
    let path = dir.join(journal_filename(local_path, remote_path));
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read sync journal: {}", e))?;
    let journal: SyncJournal = serde_json::from_str(&data)
        .map_err(|e| format!("Failed to parse sync journal: {}", e))?;
    Ok(Some(journal))
}

/// Save a journal (creates or overwrites)
pub fn save_sync_journal(journal: &SyncJournal) -> Result<(), String> {
    let dir = sync_journal_dir()?;
    let path = dir.join(journal_filename(&journal.local_path, &journal.remote_path));
    let mut journal_to_save = journal.clone();
    journal_to_save.updated_at = Utc::now();
    let data = serde_json::to_string(&journal_to_save)
        .map_err(|e| format!("Failed to serialize sync journal: {}", e))?;
    std::fs::write(&path, data)
        .map_err(|e| format!("Failed to write sync journal: {}", e))?;
    Ok(())
}

/// Delete a journal for a path pair
pub fn delete_sync_journal(local_path: &str, remote_path: &str) -> Result<(), String> {
    let dir = sync_journal_dir()?;
    let path = dir.join(journal_filename(local_path, remote_path));
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to delete sync journal: {}", e))?;
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
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("Failed to read journal directory: {}", e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            if let Ok(data) = std::fs::read_to_string(&path) {
                if let Ok(journal) = serde_json::from_str::<SyncJournal>(&data) {
                    let completed_entries = journal.entries.iter()
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
    summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(summaries)
}

/// Delete journals older than the given number of days.
/// Returns the number of journals deleted.
pub fn cleanup_old_journals(max_age_days: u32) -> Result<u32, String> {
    let dir = sync_journal_dir()?;
    let cutoff = Utc::now() - chrono::Duration::days(max_age_days as i64);
    let mut deleted = 0u32;
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("Failed to read journal directory: {}", e))?;
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
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("Failed to read journal directory: {}", e))?;
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
    pub direction: SyncDirection,
    pub compare_timestamp: bool,
    pub compare_size: bool,
    pub compare_checksum: bool,
    pub exclude_patterns: Vec<String>,
    pub retry_policy: RetryPolicy,
    pub verify_policy: VerifyPolicy,
    pub delete_orphans: bool,
}

impl SyncProfile {
    /// Mirror: local → remote, delete orphans on remote, verify size
    pub fn mirror() -> Self {
        Self {
            id: "mirror".to_string(),
            name: "Mirror".to_string(),
            builtin: true,
            direction: SyncDirection::LocalToRemote,
            compare_timestamp: true,
            compare_size: true,
            compare_checksum: false,
            exclude_patterns: vec![
                "node_modules".into(), ".git".into(), ".DS_Store".into(),
                "Thumbs.db".into(), "__pycache__".into(), "target".into(),
            ],
            retry_policy: RetryPolicy::default(),
            verify_policy: VerifyPolicy::SizeOnly,
            delete_orphans: true,
        }
    }

    /// Two-way: bidirectional, keep newer, no deletes
    pub fn two_way() -> Self {
        Self {
            id: "two_way".to_string(),
            name: "Two-way".to_string(),
            builtin: true,
            direction: SyncDirection::Bidirectional,
            compare_timestamp: true,
            compare_size: true,
            compare_checksum: false,
            exclude_patterns: vec![
                "node_modules".into(), ".git".into(), ".DS_Store".into(),
                "Thumbs.db".into(), "__pycache__".into(), "target".into(),
            ],
            retry_policy: RetryPolicy::default(),
            verify_policy: VerifyPolicy::SizeOnly,
            delete_orphans: false,
        }
    }

    /// Backup: local → remote, checksum verify, no deletes
    pub fn backup() -> Self {
        Self {
            id: "backup".to_string(),
            name: "Backup".to_string(),
            builtin: true,
            direction: SyncDirection::LocalToRemote,
            compare_timestamp: false,
            compare_size: true,
            compare_checksum: true,
            exclude_patterns: vec![
                "node_modules".into(), ".git".into(), ".DS_Store".into(),
                "Thumbs.db".into(), "__pycache__".into(), "target".into(),
            ],
            retry_policy: RetryPolicy {
                max_retries: 5,
                ..RetryPolicy::default()
            },
            verify_policy: VerifyPolicy::Full,
            delete_orphans: false,
        }
    }

    /// All built-in profiles
    pub fn builtins() -> Vec<Self> {
        vec![Self::mirror(), Self::two_way(), Self::backup()]
    }
}

/// Directory for custom sync profiles
fn sync_profiles_dir() -> Result<PathBuf, String> {
    let base = dirs::config_dir()
        .ok_or_else(|| "Cannot determine config directory".to_string())?;
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

/// Save a custom profile
pub fn save_sync_profile(profile: &SyncProfile) -> Result<(), String> {
    if profile.builtin {
        return Err("Cannot save built-in profiles".to_string());
    }
    let dir = sync_profiles_dir()?;
    let path = dir.join(format!("{}.json", profile.id));
    let data = serde_json::to_string(profile)
        .map_err(|e| format!("Failed to serialize sync profile: {}", e))?;
    std::fs::write(&path, data)
        .map_err(|e| format!("Failed to write sync profile: {}", e))?;
    Ok(())
}

/// Delete a custom profile
pub fn delete_sync_profile(id: &str) -> Result<(), String> {
    let dir = sync_profiles_dir()?;
    let path = dir.join(format!("{}.json", id));
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to delete sync profile: {}", e))?;
    }
    Ok(())
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
        let result = verify_local_file("/nonexistent/path/file.txt", 100, None, &VerifyPolicy::SizeOnly);
        assert!(!result.passed);
        assert!(!result.size_match);
    }

    #[test]
    fn test_journal_has_resumable() {
        let mut journal = SyncJournal::new(
            "/local".to_string(),
            "/remote".to_string(),
            SyncDirection::Bidirectional,
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
            SyncDirection::LocalToRemote,
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
}
