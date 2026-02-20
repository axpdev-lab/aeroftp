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
    /// Human-readable explanation of why this file needs syncing
    pub sync_reason: String,
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
            let sync_reason = generate_sync_reason(&status, local, remote, is_dir);
            results.push(FileComparison {
                relative_path: path,
                status,
                local_info: local.cloned(),
                remote_info: remote.cloned(),
                is_dir,
                sync_reason,
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

/// Atomic write: write to temp file, then rename to target path.
/// Prevents corruption from crash/power-loss during write.
fn atomic_write(path: &std::path::Path, data: &[u8]) -> Result<(), String> {
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, data)
        .map_err(|e| format!("Failed to write temp file {}: {}", tmp_path.display(), e))?;
    std::fs::rename(&tmp_path, path)
        .map_err(|e| format!("Failed to rename {} to {}: {}", tmp_path.display(), path.display(), e))?;
    Ok(())
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

/// Stable DJB2 hash — deterministic across Rust versions (unlike DefaultHasher)
fn stable_path_hash(s: &str) -> u64 {
    let mut hash: u64 = 5381;
    for byte in s.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
    }
    hash
}

/// Generate a stable filename from a local+remote path pair
fn index_filename(local_path: &str, remote_path: &str) -> String {
    let combined = format!("{}|{}", local_path, remote_path);
    format!("{:016x}.json", stable_path_hash(&combined))
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
            let sync_reason = generate_sync_reason(&status, local, remote, is_dir);
            results.push(FileComparison {
                relative_path: path,
                status,
                local_info: local.cloned(),
                remote_info: remote.cloned(),
                is_dir,
                sync_reason,
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
    #[allow(dead_code)] // Used in unit tests
    pub fn delay_for_attempt(&self, attempt: u32) -> u64 {
        let delay = (self.base_delay_ms as f64) * self.backoff_multiplier.powi(attempt.saturating_sub(1) as i32);
        if !delay.is_finite() || delay < 0.0 {
            return self.max_delay_ms;
        }
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
    #[allow(dead_code)] // Used in unit tests
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
    let combined = format!("{}|{}", local_path, remote_path);
    format!("journal_{:016x}.json", stable_path_hash(&combined))
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
    atomic_write(&path, data.as_bytes())?;
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
            parallel_streams: 1,
            compression_mode: crate::transfer_pool::CompressionMode::Off,
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

/// Validate that an ID is safe for use in filesystem paths (alphanumeric, hyphens, underscores only)
fn validate_filesystem_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 256 {
        return Err("Invalid ID length".to_string());
    }
    if id.contains('/') || id.contains('\\') || id.contains("..") || id.contains('\0') {
        return Err("ID contains forbidden characters".to_string());
    }
    // Only allow UUID-like chars: alphanumeric, hyphens, underscores
    if !id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Err("ID contains invalid characters".to_string());
    }
    Ok(())
}

/// Save a custom profile
pub fn save_sync_profile(profile: &SyncProfile) -> Result<(), String> {
    if profile.builtin {
        return Err("Cannot save built-in profiles".to_string());
    }
    validate_filesystem_id(&profile.id)?;
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
    validate_filesystem_id(id)?;
    let dir = sync_profiles_dir()?;
    let path = dir.join(format!("{}.json", id));
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to delete sync profile: {}", e))?;
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

/// Save multi-path config to disk
pub fn save_multi_path_config(config: &MultiPathConfig) -> Result<(), String> {
    let dir = dirs::config_dir().unwrap_or_default().join("aeroftp");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let data = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("multi_path.json"), data).map_err(|e| e.to_string())
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
    pub direction: SyncDirection,
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
            direction: profile.direction.clone(),
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
        if let Some(docs) = dirs::document_dir() {
            let docs_str = docs.to_string_lossy();
            if path.starts_with(docs_str.as_ref()) {
                return path.replacen(docs_str.as_ref(), "$DOCUMENTS", 1);
            }
        }
        if let Some(desktop) = dirs::desktop_dir() {
            let desk_str = desktop.to_string_lossy();
            if path.starts_with(desk_str.as_ref()) {
                return path.replacen(desk_str.as_ref(), "$DESKTOP", 1);
            }
        }
        if path.starts_with(home_str.as_ref()) {
            return path.replacen(home_str.as_ref(), "$HOME", 1);
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
    let files: HashMap<String, FileSnapshotEntry> = index.files.iter().map(|(path, entry)| {
        (path.clone(), FileSnapshotEntry {
            size: entry.size,
            modified: entry.modified,
            checksum: None,
            action_taken: String::new(),
        })
    }).collect();

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
    std::fs::write(dir.join(format!("{}.json", snapshot.id)), data)
        .map_err(|e| e.to_string())
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
    snapshots.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    snapshots.truncate(10);

    // Cleanup: keep only last 5 snapshots on disk
    let all_files: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    if all_files.len() > 5 {
        let mut by_time: Vec<_> = all_files.into_iter()
            .filter_map(|e| e.metadata().ok().map(|m| (e.path(), m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH))))
            .collect();
        by_time.sort_by(|a, b| b.1.cmp(&a.1));
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
                let a_mod = a.1.modified.unwrap_or_else(|| chrono::DateTime::<Utc>::MIN_UTC);
                let b_mod = b.1.modified.unwrap_or_else(|| chrono::DateTime::<Utc>::MIN_UTC);
                b_mod.cmp(&a_mod)
            });
        }
        "largest" => {
            file_list.sort_by(|a, b| b.1.size.cmp(&a.1.size));
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
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|e| format!("HMAC key error: {}", e))?;
    mac.update(canonical.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

/// Generate the .sig filename for a journal path pair
pub fn journal_sig_filename(local_path: &str, remote_path: &str) -> String {
    let combined = format!("{}|{}", local_path, remote_path);
    format!("journal_{:016x}.sig", stable_path_hash(&combined))
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
            assert!(portable.starts_with("$HOME") || portable.starts_with("$DOCUMENTS") || portable.starts_with("$DESKTOP"));
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
        index.files.insert("file.txt".to_string(), SyncIndexEntry {
            size: 1024,
            modified: Some(Utc::now()),
            is_dir: false,
        });

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
            "Test Template", "A test", &profile, "/home/user/docs", "/remote/docs",
            &["*.tmp".to_string()], None,
        ).unwrap();
        assert_eq!(template.schema_version, 1);
        assert_eq!(template.name, "Test Template");
        assert_eq!(template.path_patterns.len(), 1);
        assert!(template.schedule.is_none());
        assert!(template.created_by.contains("AeroFTP"));
    }
}
