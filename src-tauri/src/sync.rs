// AeroFTP Sync Module
// File comparison and synchronization logic

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    
    for pattern in patterns {
        let pattern_lower = pattern.to_lowercase();
        
        // Simple glob matching
        if pattern_lower.starts_with('*') {
            // *.ext pattern
            let ext = &pattern_lower[1..];
            if path_lower.ends_with(ext) {
                return true;
            }
        } else if path_lower.contains(&pattern_lower) {
            // Direct name match
            return true;
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
    let data = serde_json::to_string_pretty(index)
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
}
