// AeroCloud Sync Service
// Background synchronization between local and remote folders
// Supports multi-protocol providers: FTP, WebDAV, S3, etc.
// NOTE: Some items prepared for Phase 5+ background sync loop
#![allow(dead_code)]
#![allow(unused_imports)]

use crate::cloud_config::{CloudConfig, CloudSyncStatus, ConflictStrategy};
use crate::ftp::FtpManager;
use crate::providers::{StorageProvider, RemoteEntry as ProviderRemoteEntry, ProviderError};
use crate::sync::{
    build_comparison_results, CompareOptions, FileComparison, FileInfo, SyncAction, SyncDirection,
    SyncStatus,
};
use crate::watcher::{CloudWatcher, WatchEvent};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
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
    Download { remote_path: String, local_path: PathBuf },
    /// Upload specific file
    Upload { local_path: PathBuf, remote_path: String },
    /// Stop the service
    Stop,
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

    /// Initialize with config
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
        }).await;

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
        for (index, comparison) in comparisons.iter().enumerate() {
            // Update progress
            self.set_status(CloudSyncStatus::Syncing {
                current_file: comparison.relative_path.clone(),
                progress: (index as f64 / total_files.max(1) as f64) * 100.0,
                files_done: index as u32,
                files_total: total_files,
            }).await;

            match self.process_comparison(ftp_manager, &config, comparison).await {
                Ok(action) => match action {
                    SyncAction::AskUser => {
                        result.conflicts += 1;
                        // Add to conflicts list
                        let mut conflicts = self.conflicts.write().await;
                        conflicts.push(FileConflict {
                            relative_path: comparison.relative_path.clone(),
                            local_modified: comparison.local_info.as_ref().and_then(|i| i.modified),
                            remote_modified: comparison.remote_info.as_ref().and_then(|i| i.modified),
                            local_size: comparison.local_info.as_ref().map(|i| i.size).unwrap_or(0),
                            remote_size: comparison.remote_info.as_ref().map(|i| i.size).unwrap_or(0),
                            status: comparison.status.clone(),
                        });
                    }
                    _ => Self::record_sync_action(&mut result, comparison, &action),
                },
                Err(e) => {
                    result.errors.push(format!("{}: {}", comparison.relative_path, e));
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
            }).await;
        } else if !result.errors.is_empty() {
            self.set_status(CloudSyncStatus::Error {
                message: format!("{} errors during sync", result.errors.len()),
            }).await;
        } else {
            self.set_status(CloudSyncStatus::Idle {
                last_sync: Some(Utc::now()),
                next_sync: None,
            }).await;
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
        }).await;

        // Get file listings
        let local_files = self.scan_local_folder(&config).await?;
        let remote_files = self.scan_remote_folder_with_provider(provider, &config).await?;

        // Build comparison
        let options = CompareOptions {
            compare_timestamp: true,
            compare_size: true,
            compare_checksum: false,
            exclude_patterns: config.exclude_patterns.clone(),
            direction: SyncDirection::Bidirectional,
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
        for (index, comparison) in comparisons.iter().enumerate() {
            // Update progress
            self.set_status(CloudSyncStatus::Syncing {
                current_file: comparison.relative_path.clone(),
                progress: (index as f64 / total_files.max(1) as f64) * 100.0,
                files_done: index as u32,
                files_total: total_files,
            }).await;

            match self.process_comparison_with_provider(provider, &config, comparison).await {
                Ok(action) => match action {
                    SyncAction::AskUser => {
                        result.conflicts += 1;
                        // Add to conflicts list
                        let mut conflicts = self.conflicts.write().await;
                        conflicts.push(FileConflict {
                            relative_path: comparison.relative_path.clone(),
                            local_modified: comparison.local_info.as_ref().and_then(|i| i.modified),
                            remote_modified: comparison.remote_info.as_ref().and_then(|i| i.modified),
                            local_size: comparison.local_info.as_ref().map(|i| i.size).unwrap_or(0),
                            remote_size: comparison.remote_info.as_ref().map(|i| i.size).unwrap_or(0),
                            status: comparison.status.clone(),
                        });
                    }
                    _ => Self::record_sync_action(&mut result, comparison, &action),
                },
                Err(e) => {
                    result.errors.push(format!("{}: {}", comparison.relative_path, e));
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
            }).await;
        } else if !result.errors.is_empty() {
            self.set_status(CloudSyncStatus::Error {
                message: format!("{} errors during sync", result.errors.len()),
            }).await;
        } else {
            self.set_status(CloudSyncStatus::Idle {
                last_sync: Some(Utc::now()),
                next_sync: None,
            }).await;
        }

        // Emit sync complete event
        if let Some(app) = &self.app_handle {
            let _ = app.emit("cloud_sync_complete", &result);
        }

        Ok(result)
    }

    fn record_sync_action(result: &mut SyncOperationResult, comparison: &FileComparison, action: &SyncAction) {
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

        // Use walkdir for recursive scanning
        fn scan_recursive(
            base: &PathBuf,
            current: &PathBuf,
            files: &mut HashMap<String, FileInfo>,
            exclude: &[String],
        ) -> Result<(), String> {
            let entries = std::fs::read_dir(current)
                .map_err(|e| format!("Failed to read directory: {}", e))?;

            for entry in entries.flatten() {
                let path = entry.path();
                let relative = path
                    .strip_prefix(base)
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .to_string();

                // Check exclusions
                if crate::sync::should_exclude(&relative, exclude) {
                    continue;
                }

                let metadata = entry.metadata().ok();
                let modified = metadata.as_ref().and_then(|m| {
                    m.modified().ok().map(|t| DateTime::<Utc>::from(t))
                });

                let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                let size = if is_dir {
                    0
                } else {
                    metadata.as_ref().map(|m| m.len()).unwrap_or(0)
                };

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
                    scan_recursive(base, &path, files, exclude)?;
                }
            }

            Ok(())
        }

        scan_recursive(base_path, base_path, &mut files, &config.exclude_patterns)?;
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

        // Stack-based recursive scan
        let mut stack = vec![(base_path.clone(), String::new())];

        while let Some((current_path, relative_prefix)) = stack.pop() {
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

                // Check exclusions
                if crate::sync::should_exclude(&relative_path, &config.exclude_patterns) {
                    continue;
                }

                files.insert(
                    relative_path.clone(),
                    FileInfo {
                        name: entry.name.clone(),
                        path: format!("{}/{}", current_path, entry.name),
                        size: entry.size.unwrap_or(0),
                        modified: entry.modified.and_then(|s| {
                            DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
                        }),
                        is_dir: entry.is_dir,
                        checksum: None,
                    },
                );

                if entry.is_dir {
                    stack.push((
                        format!("{}/{}", current_path, entry.name),
                        relative_path,
                    ));
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
                        .upload_file_with_progress(&local_info.path, &remote_path, local_info.size, |_| {})
                        .await
                        .map_err(|e| format!("Upload failed: {}", e))?;
                }
            }
            SyncAction::Download => {
                let local_path = config.local_folder.join(&comparison.relative_path);

                if comparison.is_dir {
                    // Create local directory
                    std::fs::create_dir_all(&local_path).ok();
                } else if let Some(remote_info) = &comparison.remote_info {
                    // Ensure parent directory exists
                    if let Some(parent) = local_path.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }

                    ftp_manager
                        .download_file_with_progress(
                            &remote_info.path,
                            &local_path.to_string_lossy(),
                            |_| {},
                        )
                        .await
                        .map_err(|e| format!("Download failed: {}", e))?;
                }
            }
            SyncAction::KeepBoth => {
                if !comparison.is_dir {
                    let local_path = config.local_folder.join(&comparison.relative_path);
                    // Rename local file with _conflict suffix to preserve both versions
                    if local_path.exists() {
                        let stem = local_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        let ext = local_path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
                        let ts = chrono::Utc::now().format("%Y%m%d%H%M%S");
                        let conflict_name = format!("{}_conflict_{}{}", stem, ts, ext);
                        let conflict_path = local_path.with_file_name(&conflict_name);
                        std::fs::rename(&local_path, &conflict_path).ok();
                    }
                    // Download remote version to original path
                    if let Some(remote_info) = &comparison.remote_info {
                        if let Some(parent) = local_path.parent() {
                            std::fs::create_dir_all(parent).ok();
                        }
                        ftp_manager
                            .download_file_with_progress(
                                &remote_info.path,
                                &local_path.to_string_lossy(),
                                |_| {},
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

        // Stack-based recursive scan
        let mut stack = vec![(base_path.clone(), String::new())];

        while let Some((current_path, relative_prefix)) = stack.pop() {
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

                // Check exclusions
                if crate::sync::should_exclude(&relative_path, &config.exclude_patterns) {
                    continue;
                }

                files.insert(
                    relative_path.clone(),
                    FileInfo {
                        name: entry.name.clone(),
                        path: format!("{}/{}", current_path, entry.name),
                        size: entry.size,
                        modified: entry.modified.and_then(|s| {
                            DateTime::parse_from_rfc3339(&s).ok().map(|dt| dt.with_timezone(&Utc))
                        }),
                        is_dir: entry.is_dir,
                        checksum: None,
                    },
                );

                if entry.is_dir {
                    stack.push((
                        format!("{}/{}", current_path, entry.name),
                        relative_path,
                    ));
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
                    // Ensure parent directory exists on remote
                    if let Some(parent) = std::path::Path::new(&comparison.relative_path).parent() {
                        let parent_path = format!(
                            "{}/{}",
                            config.remote_folder.trim_end_matches('/'),
                            parent.to_string_lossy()
                        );
                        let _ = provider.mkdir(&parent_path).await;
                    }
                    
                    provider
                        .upload(&local_info.path, &remote_path, None)
                        .await
                        .map_err(|e| format!("Upload failed: {}", e))?;
                }
            }
            SyncAction::Download => {
                let local_path = config.local_folder.join(&comparison.relative_path);

                if comparison.is_dir {
                    // Create local directory
                    std::fs::create_dir_all(&local_path).ok();
                } else if let Some(remote_info) = &comparison.remote_info {
                    // Ensure parent directory exists
                    if let Some(parent) = local_path.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }

                    provider
                        .download(&remote_info.path, &local_path.to_string_lossy(), None)
                        .await
                        .map_err(|e| format!("Download failed: {}", e))?;
                }
            }
            SyncAction::KeepBoth => {
                if !comparison.is_dir {
                    let local_path = config.local_folder.join(&comparison.relative_path);
                    // Rename local file with _conflict suffix to preserve both versions
                    if local_path.exists() {
                        let stem = local_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        let ext = local_path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
                        let ts = chrono::Utc::now().format("%Y%m%d%H%M%S");
                        let conflict_name = format!("{}_conflict_{}{}", stem, ts, ext);
                        let conflict_path = local_path.with_file_name(&conflict_name);
                        std::fs::rename(&local_path, &conflict_path).ok();
                    }
                    // Download remote version to original path
                    if let Some(remote_info) = &comparison.remote_info {
                        if let Some(parent) = local_path.parent() {
                            std::fs::create_dir_all(parent).ok();
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
