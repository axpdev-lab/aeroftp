// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

// AeroCloud Sync Service
// Background synchronization between local and remote folders
// Supports multi-protocol providers: FTP, WebDAV, S3, etc.
// NOTE: Some items prepared for Phase 5+ background sync loop
#![allow(dead_code)]
#![allow(unused_imports)]

use crate::cloud_config::{CloudConfig, CloudSyncStatus, ConflictStrategy};
use crate::ftp::FtpManager;
use crate::providers::{ProviderError, RemoteEntry as ProviderRemoteEntry, StorageProvider};
use crate::sync::{
    build_comparison_results, validate_relative_path, CompareOptions, FileComparison, FileInfo,
    SyncAction, SyncDirection, SyncStatus,
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

/// Result of a sync operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncedFileDetail {
    pub path: String,
    pub direction: String,
    pub size: u64,
}

/// Result of a sync operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncOperationResult {
    pub uploaded: u32,
    pub downloaded: u32,
    pub deleted: u32,
    pub skipped: u32,
    pub conflicts: u32,
    pub errors: Vec<String>,
    pub duration_secs: u64,
    pub file_details: Vec<SyncedFileDetail>,
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
}

impl CloudService {
    /// Create a new cloud service
    pub fn new() -> Self {
        Self {
            config: Arc::new(RwLock::new(CloudConfig::default())),
            status: Arc::new(RwLock::new(CloudSyncStatus::NotConfigured)),
            conflicts: Arc::new(RwLock::new(Vec::new())),
            task_tx: None,
            app_handle: None,
        }
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

    /// Get pending conflicts
    pub async fn get_conflicts(&self) -> Vec<FileConflict> {
        self.conflicts.read().await.clone()
    }

    /// Clear conflicts
    pub async fn clear_conflicts(&self) {
        let mut conflicts = self.conflicts.write().await;
        conflicts.clear();
    }

    /// Perform a full sync between local and remote folders
    pub async fn perform_full_sync(
        &self,
        ftp_manager: &mut FtpManager,
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

        // Get file listings
        let local_files = self.scan_local_folder(&config).await?;
        let remote_files = self.scan_remote_folder(ftp_manager, &config).await?;

        // Build comparison
        let options = CompareOptions {
            compare_timestamp: true,
            compare_size: true,
            compare_checksum: false,
            exclude_patterns: config.exclude_patterns.clone(),
            direction: SyncDirection::Bidirectional,
            ..Default::default()
        };

        let comparisons = build_comparison_results(local_files, remote_files, &options);

        let total_files = comparisons.len() as u32;
        let mut result = SyncOperationResult {
            uploaded: 0,
            downloaded: 0,
            deleted: 0,
            skipped: 0,
            conflicts: 0,
            errors: Vec::new(),
            duration_secs: 0,
            file_details: Vec::new(),
        };

        // Process each comparison
        // P1-7: Throttle status updates to every 100 files or 500ms to reduce lock contention
        let mut last_status_update = std::time::Instant::now();
        let status_interval = std::time::Duration::from_millis(500);
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
                .process_comparison(ftp_manager, &config, comparison)
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
                }
            }
        }

        result.duration_secs = start_time.elapsed().as_secs();

        // Update config with last sync time
        {
            let mut cfg = self.config.write().await;
            cfg.last_sync = Some(Utc::now());
            let _ = crate::cloud_config::save_cloud_config(&cfg);
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

        // Ensure remote folder exists before scanning (check first — some providers
        // like FileLu create duplicates if mkdir is called on an existing folder)
        if provider.cd(&config.remote_folder).await.is_err() {
            if let Err(e) = provider.mkdir(&config.remote_folder).await {
                tracing::warn!(
                    "Failed to create remote folder {}: {}",
                    config.remote_folder,
                    e
                );
            }
        }

        // Get file listings
        let local_files = self.scan_local_folder(&config).await?;
        let remote_files = self
            .scan_remote_folder_with_provider(provider, &config)
            .await?;

        // Enable checksum comparison when provider supplies content hashes (e.g. FileLu)
        let has_checksums = remote_files.values().any(|f| f.checksum.is_some());

        // Build comparison
        let options = CompareOptions {
            compare_timestamp: true,
            compare_size: true,
            compare_checksum: has_checksums,
            exclude_patterns: config.exclude_patterns.clone(),
            direction: SyncDirection::Bidirectional,
            ..Default::default()
        };

        let comparisons = build_comparison_results(local_files, remote_files, &options);

        let total_files = comparisons.len() as u32;
        let mut result = SyncOperationResult {
            uploaded: 0,
            downloaded: 0,
            deleted: 0,
            skipped: 0,
            conflicts: 0,
            errors: Vec::new(),
            duration_secs: 0,
            file_details: Vec::new(),
        };

        // Process each comparison
        // P1-7: Throttle status updates to every 100 files or 500ms to reduce lock contention
        let mut last_status_update = std::time::Instant::now();
        let status_interval = std::time::Duration::from_millis(500);
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
                    // OAuth 1.0a tokens cannot be refreshed — abort sync and prompt user.
                    if e.contains("token_revoked") {
                        if let Some(app) = &self.app_handle {
                            let _ = app.emit(
                                "cloud-reauth-required",
                                serde_json::json!({
                                    "provider": config.protocol_type,
                                    "reason": "token_revoked",
                                    "message": e,
                                }),
                            );
                        }
                        result
                            .errors
                            .push("Sync aborted: re-authorization required".to_string());
                        break;
                    }
                }
            }
        }

        result.duration_secs = start_time.elapsed().as_secs();

        // Update config with last sync time
        {
            let mut cfg = self.config.write().await;
            cfg.last_sync = Some(Utc::now());
            let _ = crate::cloud_config::save_cloud_config(&cfg);
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
            SyncAction::Skip => result.skipped += 1,
            SyncAction::AskUser => {}
        }
    }

    /// Scan local folder and build file info map
    async fn scan_local_folder(
        &self,
        config: &CloudConfig,
    ) -> Result<HashMap<String, FileInfo>, String> {
        let mut files = HashMap::new();
        let base_path = &config.local_folder;

        if !base_path.exists() {
            return Ok(files);
        }

        // Load .aeroignore from sync root (if present)
        let aeroignore = crate::sync_ignore::AeroIgnore::load(base_path);

        // Use walkdir for recursive scanning
        fn scan_recursive(
            base: &PathBuf,
            current: &PathBuf,
            files: &mut HashMap<String, FileInfo>,
            exclude: &[String],
            excluded_folders: &[String],
            aeroignore: Option<&crate::sync_ignore::AeroIgnore>,
        ) -> Result<(), String> {
            let entries = std::fs::read_dir(current)
                .map_err(|e| format!("Failed to read directory: {}", e))?;

            for entry in entries.flatten() {
                let path = entry.path();

                // Use symlink_metadata to avoid following symlinks (CF-007)
                let metadata = match path.symlink_metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                // Skip symlinks entirely to prevent symlink-following attacks
                if metadata.file_type().is_symlink() {
                    continue;
                }

                let relative = path
                    .strip_prefix(base)
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .to_string();

                let is_dir = metadata.is_dir();

                // Check exclusions: .aeroignore first (with negation), then config patterns
                let excluded = if let Some(ai) = aeroignore {
                    ai.should_exclude(&relative, is_dir, exclude)
                } else {
                    crate::sync::should_exclude(&relative, exclude)
                };
                if excluded {
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

                let modified = metadata.modified().ok().map(DateTime::<Utc>::from);

                let size = if is_dir { 0 } else { metadata.len() };

                // P1-6: Cap file index at 100K to prevent unbounded memory growth
                if files.len() >= 100_000 {
                    tracing::warn!("Local file index cap reached (100K), truncating scan");
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
                        checksum: None,
                    },
                );

                if is_dir {
                    scan_recursive(base, &path, files, exclude, excluded_folders, aeroignore)?;
                }
            }

            Ok(())
        }

        scan_recursive(
            base_path,
            base_path,
            &mut files,
            &config.exclude_patterns,
            &config.excluded_folders,
            aeroignore.as_ref(),
        )?;
        Ok(files)
    }

    /// Scan remote folder and build file info map
    async fn scan_remote_folder(
        &self,
        ftp_manager: &mut FtpManager,
        config: &CloudConfig,
    ) -> Result<HashMap<String, FileInfo>, String> {
        let mut files = HashMap::new();
        let base_path = &config.remote_folder;
        let aeroignore = crate::sync_ignore::AeroIgnore::load(&config.local_folder);

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
                continue;
            }

            // Navigate to directory
            if ftp_manager.change_dir(&current_path).await.is_err() {
                continue;
            }

            // List files
            let entries = match ftp_manager.list_files().await {
                Ok(list) => list,
                Err(_) => continue,
            };

            for entry in entries {
                let relative_path = if relative_prefix.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", relative_prefix, entry.name)
                };

                // Check exclusions: .aeroignore first, then config patterns
                let excluded = if let Some(ref ai) = aeroignore {
                    ai.should_exclude(&relative_path, entry.is_dir, &config.exclude_patterns)
                } else {
                    crate::sync::should_exclude(&relative_path, &config.exclude_patterns)
                };
                if excluded {
                    continue;
                }

                // P1-6: Cap file index at 100K to prevent unbounded memory growth
                if files.len() >= 100_000 {
                    tracing::warn!("Remote file index cap reached (100K), truncating scan");
                    return Ok(files);
                }

                files.insert(
                    relative_path.clone(),
                    FileInfo {
                        name: entry.name.clone(),
                        path: format!("{}/{}", current_path, entry.name),
                        size: entry.size.unwrap_or(0),
                        modified: entry.modified.and_then(|s| {
                            // Try RFC 3339 first (with T separator)
                            DateTime::parse_from_rfc3339(&s)
                                .ok()
                                .map(|dt| dt.with_timezone(&Utc))
                                .or_else(|| {
                                    // Fallback: replace space with T for timestamps like "2026-03-12 00:00:00Z"
                                    let fixed = s.replacen(' ', "T", 1);
                                    DateTime::parse_from_rfc3339(&fixed)
                                        .ok()
                                        .map(|dt| dt.with_timezone(&Utc))
                                })
                                .or_else(|| {
                                    // Fallback: parse without timezone (assume UTC)
                                    chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S")
                                        .ok()
                                        .map(|naive| naive.and_utc())
                                })
                        }),
                        is_dir: entry.is_dir,
                        checksum: None,
                    },
                );

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

        Ok(files)
    }

    /// Process a single file comparison and perform the appropriate action
    async fn process_comparison(
        &self,
        ftp_manager: &mut FtpManager,
        config: &CloudConfig,
        comparison: &FileComparison,
    ) -> Result<SyncAction, String> {
        // Validate relative_path against traversal attacks (CF-004)
        validate_relative_path(&comparison.relative_path)?;

        // Determine action based on status and conflict strategy
        let action = match &comparison.status {
            SyncStatus::Identical => SyncAction::Skip,
            SyncStatus::LocalNewer => SyncAction::Upload,
            SyncStatus::RemoteNewer => SyncAction::Download,
            SyncStatus::LocalOnly => SyncAction::Upload,
            SyncStatus::RemoteOnly => SyncAction::Download,
            SyncStatus::Conflict | SyncStatus::SizeMismatch => {
                match config.conflict_strategy {
                    ConflictStrategy::AskUser => SyncAction::AskUser,
                    ConflictStrategy::KeepBoth => SyncAction::KeepBoth,
                    ConflictStrategy::PreferLocal => SyncAction::Upload,
                    ConflictStrategy::PreferRemote => SyncAction::Download,
                    ConflictStrategy::PreferNewer => {
                        // Compare timestamps
                        let local_time = comparison.local_info.as_ref().and_then(|i| i.modified);
                        let remote_time = comparison.remote_info.as_ref().and_then(|i| i.modified);
                        match (local_time, remote_time) {
                            (Some(l), Some(r)) if l > r => SyncAction::Upload,
                            (Some(l), Some(r)) if r > l => SyncAction::Download,
                            _ => SyncAction::AskUser,
                        }
                    }
                }
            }
        };

        // Execute action
        match &action {
            SyncAction::Upload => {
                let remote_path = format!(
                    "{}/{}",
                    config.remote_folder.trim_end_matches('/'),
                    comparison.relative_path
                );

                if comparison.is_dir {
                    // Create remote directory
                    if let Err(e) = ftp_manager.mkdir(&remote_path).await {
                        // Directory might already exist, log but don't fail
                        tracing::debug!("mkdir {} (may exist): {}", remote_path, e);
                    }
                } else if let Some(local_info) = &comparison.local_info {
                    // Ensure parent directory exists on remote
                    if let Some(parent) = std::path::Path::new(&comparison.relative_path).parent() {
                        let parent_path = format!(
                            "{}/{}",
                            config.remote_folder.trim_end_matches('/'),
                            parent.to_string_lossy()
                        );
                        let _ = ftp_manager.mkdir(&parent_path).await;
                    }

                    ftp_manager
                        .upload_file_with_progress(
                            &local_info.path,
                            &remote_path,
                            local_info.size,
                            |_| true,
                        )
                        .await
                        .map_err(|e| format!("Upload failed: {}", e))?;
                    // Do NOT modify local mtime after upload.
                    // SFTP/FTP providers now preserve mtime via setstat/MFMT.
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

                    ftp_manager
                        .download_file_with_progress(
                            &remote_info.path,
                            &local_path.to_string_lossy(),
                            |_| true,
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
            SyncAction::KeepBoth => {
                if !comparison.is_dir {
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
                        ftp_manager
                            .download_file_with_progress(
                                &remote_info.path,
                                &local_path.to_string_lossy(),
                                |_| true,
                            )
                            .await
                            .map_err(|e| format!("KeepBoth download failed: {}", e))?;
                    }
                }
            }
            _ => {}
        }

        Ok(action)
    }

    /// Scan remote folder using any StorageProvider (multi-protocol support)
    async fn scan_remote_folder_with_provider<P: StorageProvider + ?Sized>(
        &self,
        provider: &mut P,
        config: &CloudConfig,
    ) -> Result<HashMap<String, FileInfo>, String> {
        let mut files = HashMap::new();
        let base_path = &config.remote_folder;
        // Load .aeroignore from local sync root (applies to remote paths too)
        let aeroignore = crate::sync_ignore::AeroIgnore::load(&config.local_folder);

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
                continue;
            }

            // Navigate to directory
            if provider.cd(&current_path).await.is_err() {
                continue;
            }

            // List files using provider
            let entries = match provider.list(".").await {
                Ok(list) => list,
                Err(_) => continue,
            };

            for entry in entries {
                let relative_path = if relative_prefix.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", relative_prefix, entry.name)
                };

                // Check exclusions: .aeroignore first, then config patterns
                let excluded = if let Some(ref ai) = aeroignore {
                    ai.should_exclude(&relative_path, entry.is_dir, &config.exclude_patterns)
                } else {
                    crate::sync::should_exclude(&relative_path, &config.exclude_patterns)
                };
                if excluded {
                    continue;
                }

                // P1-6: Cap file index at 100K to prevent unbounded memory growth
                if files.len() >= 100_000 {
                    tracing::warn!("Remote file index cap reached (100K), truncating scan");
                    return Ok(files);
                }

                files.insert(
                    relative_path.clone(),
                    FileInfo {
                        name: entry.name.clone(),
                        path: format!("{}/{}", current_path, entry.name),
                        size: entry.size,
                        modified: entry.modified.and_then(|s| {
                            // Try RFC 3339 first (with T separator)
                            DateTime::parse_from_rfc3339(&s)
                                .ok()
                                .map(|dt| dt.with_timezone(&Utc))
                                .or_else(|| {
                                    // Fallback: replace space with T for timestamps like "2026-03-12 00:00:00Z"
                                    let fixed = s.replacen(' ', "T", 1);
                                    DateTime::parse_from_rfc3339(&fixed)
                                        .ok()
                                        .map(|dt| dt.with_timezone(&Utc))
                                })
                                .or_else(|| {
                                    // Fallback: parse without timezone (assume UTC)
                                    chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S")
                                        .ok()
                                        .map(|naive| naive.and_utc())
                                })
                        }),
                        is_dir: entry.is_dir,
                        // Use provider-supplied content hash if available (e.g. FileLu).
                        // Enables hash-based comparison for providers that don't preserve mtime.
                        checksum: entry.metadata.get("content_hash").cloned(),
                    },
                );

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

        Ok(files)
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

        // Determine action based on status and conflict strategy
        let action = match &comparison.status {
            SyncStatus::Identical => SyncAction::Skip,
            SyncStatus::LocalNewer => SyncAction::Upload,
            SyncStatus::RemoteNewer => SyncAction::Download,
            SyncStatus::LocalOnly => SyncAction::Upload,
            SyncStatus::RemoteOnly => SyncAction::Download,
            SyncStatus::Conflict | SyncStatus::SizeMismatch => {
                match config.conflict_strategy {
                    ConflictStrategy::AskUser => SyncAction::AskUser,
                    ConflictStrategy::KeepBoth => SyncAction::KeepBoth,
                    ConflictStrategy::PreferLocal => SyncAction::Upload,
                    ConflictStrategy::PreferRemote => SyncAction::Download,
                    ConflictStrategy::PreferNewer => {
                        // Compare timestamps
                        let local_time = comparison.local_info.as_ref().and_then(|i| i.modified);
                        let remote_time = comparison.remote_info.as_ref().and_then(|i| i.modified);
                        match (local_time, remote_time) {
                            (Some(l), Some(r)) if l > r => SyncAction::Upload,
                            (Some(l), Some(r)) if r > l => SyncAction::Download,
                            _ => SyncAction::AskUser,
                        }
                    }
                }
            }
        };

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
                                let _ = provider.mkdir(&parent_path).await;
                            }
                        }
                    }

                    tracing::info!(
                        "AeroCloud: uploading local '{}' ({} bytes) to remote '{}'",
                        local_info.path,
                        local_info.size,
                        remote_path
                    );
                    provider
                        .upload(&local_info.path, &remote_path, None)
                        .await
                        .map_err(|e| format!("Upload failed: {}", e))?;

                    // After upload, stat the remote file to get the server-assigned mtime,
                    // then apply it to the local file so both sides match.
                    // This prevents ping-pong re-sync on all providers (SFTP, FTP, WebDAV, S3, cloud APIs).
                    match provider.stat(&remote_path).await {
                        Ok(remote_entry) => {
                            if let Some(mtime_str) = &remote_entry.modified {
                                // Parse and apply remote mtime to local file
                                let remote_dt = chrono::DateTime::parse_from_rfc3339(mtime_str)
                                    .ok()
                                    .map(|dt| dt.with_timezone(&chrono::Utc))
                                    .or_else(|| {
                                        let fixed = mtime_str.replacen(' ', "T", 1);
                                        chrono::DateTime::parse_from_rfc3339(&fixed)
                                            .ok()
                                            .map(|dt| dt.with_timezone(&chrono::Utc))
                                    })
                                    .or_else(|| {
                                        chrono::NaiveDateTime::parse_from_str(
                                            mtime_str,
                                            "%Y-%m-%d %H:%M:%S",
                                        )
                                        .ok()
                                        .map(|naive| naive.and_utc())
                                    });
                                if let Some(dt) = remote_dt {
                                    let local_path = std::path::Path::new(&local_info.path);
                                    crate::preserve_remote_mtime_dt(local_path, Some(dt));
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                "Could not stat remote file after upload (non-fatal): {}",
                                e
                            );
                        }
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
            SyncAction::KeepBoth => {
                if !comparison.is_dir {
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
            }
            _ => {}
        }

        Ok(action)
    }
}

impl Default for CloudService {
    fn default() -> Self {
        Self::new()
    }
}
