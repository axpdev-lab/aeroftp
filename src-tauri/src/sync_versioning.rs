//! AeroCloud file versioning — automatic backup of overwritten/deleted files.
//!
//! When a sync operation would overwrite or delete a local file, the previous
//! version is archived in `.aeroversions/` with a timestamp suffix.
//! Three strategies inspired by Syncthing:
//! - **TrashCan**: keep all, auto-cleanup after N days
//! - **Simple**: keep last N copies per file
//! - **Staggered**: decreasing frequency (1/hour for 24h, 1/day for 30d, 1/week older)

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::info;

/// Versioning strategy for archived files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VersioningStrategy {
    /// No versioning — overwritten files are lost
    Disabled,
    /// Keep all versions, auto-delete after max_age_days
    TrashCan {
        #[serde(default = "default_max_age")]
        max_age_days: u32,
    },
    /// Keep last N versions per file
    Simple {
        #[serde(default = "default_max_copies")]
        max_copies: u32,
    },
    /// Decreasing frequency: 1/hour 24h, 1/day 30d, 1/week older
    Staggered,
}

fn default_max_age() -> u32 { 30 }
fn default_max_copies() -> u32 { 5 }

impl Default for VersioningStrategy {
    fn default() -> Self {
        Self::TrashCan { max_age_days: 30 }
    }
}

/// A single archived version of a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionEntry {
    /// Path to the archived file
    pub archive_path: PathBuf,
    /// Original relative path (from sync root)
    pub original_relative: String,
    /// Timestamp when archived
    pub archived_at: String,
    /// File size in bytes
    pub size: u64,
}

/// Statistics from a cleanup operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupStats {
    pub deleted_count: u32,
    pub freed_bytes: u64,
}

/// File versioning engine for AeroCloud sync.
pub struct SyncVersioning {
    strategy: VersioningStrategy,
    versions_dir: PathBuf,
    sync_root: PathBuf,
}

impl SyncVersioning {
    /// Create a new versioning engine for the given sync root.
    pub fn new(sync_root: &Path, strategy: VersioningStrategy) -> Self {
        let versions_dir = sync_root.join(".aeroversions");
        Self {
            strategy,
            versions_dir,
            sync_root: sync_root.to_path_buf(),
        }
    }

    /// Check if versioning is enabled.
    pub fn is_enabled(&self) -> bool {
        self.strategy != VersioningStrategy::Disabled
    }

    /// Archive a file before it gets overwritten or deleted.
    /// Returns the path where the archived copy was stored.
    pub fn archive(&self, file_path: &Path) -> Result<PathBuf, String> {
        if !self.is_enabled() {
            return Err("Versioning is disabled".to_string());
        }

        if !file_path.exists() {
            return Err(format!("File does not exist: {}", file_path.display()));
        }

        // Compute relative path from sync root
        let relative = file_path
            .strip_prefix(&self.sync_root)
            .map_err(|_| "File is not inside sync root".to_string())?;

        // Build archive path: .aeroversions/<relative_dir>/<stem>~<timestamp>.<ext>
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let stem = relative
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let ext = relative
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let archive_name = format!("{}~{}{}", stem, ts, ext);

        let archive_dir = if let Some(parent) = relative.parent() {
            if parent.as_os_str().is_empty() {
                self.versions_dir.clone()
            } else {
                self.versions_dir.join(parent)
            }
        } else {
            self.versions_dir.clone()
        };

        std::fs::create_dir_all(&archive_dir)
            .map_err(|e| format!("Failed to create versions dir: {}", e))?;

        let archive_path = archive_dir.join(&archive_name);
        std::fs::copy(file_path, &archive_path)
            .map_err(|e| format!("Failed to archive file: {}", e))?;

        info!(
            "[Versioning] Archived {} -> {}",
            relative.display(),
            archive_path.display()
        );

        Ok(archive_path)
    }

    /// List all archived versions of a specific file.
    pub fn list_versions(&self, relative_path: &str) -> Result<Vec<VersionEntry>, String> {
        let path = Path::new(relative_path);
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let prefix = format!("{}~", stem);

        let archive_dir = if let Some(parent) = path.parent() {
            if parent.as_os_str().is_empty() {
                self.versions_dir.clone()
            } else {
                self.versions_dir.join(parent)
            }
        } else {
            self.versions_dir.clone()
        };

        if !archive_dir.exists() {
            return Ok(Vec::new());
        }

        let mut versions = Vec::new();
        let entries = std::fs::read_dir(&archive_dir)
            .map_err(|e| format!("Failed to read versions dir: {}", e))?;

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) && name.ends_with(&ext) {
                let meta = entry.metadata().ok();
                let ts_part = name
                    .strip_prefix(&prefix)
                    .and_then(|rest| rest.strip_suffix(&ext))
                    .unwrap_or("");

                versions.push(VersionEntry {
                    archive_path: entry.path(),
                    original_relative: relative_path.to_string(),
                    archived_at: ts_part.to_string(),
                    size: meta.map(|m| m.len()).unwrap_or(0),
                });
            }
        }

        versions.sort_by(|a, b| b.archived_at.cmp(&a.archived_at));
        Ok(versions)
    }

    /// List ALL archived versions across all files (for browse-all UI mode).
    pub fn list_all_versions(&self) -> Result<Vec<VersionEntry>, String> {
        let mut all = Vec::new();
        if !self.versions_dir.exists() {
            return Ok(all);
        }
        self.walk_versions(|path, meta| {
            if meta.is_file() {
                let rel = path.strip_prefix(&self.versions_dir)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                // Parse "stem~timestamp.ext" → extract original_relative and archived_at
                let filename = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                if let Some(tilde_pos) = filename.find('~') {
                    let parent_dir = path.parent()
                        .and_then(|p| p.strip_prefix(&self.versions_dir).ok())
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let stem = &filename[..tilde_pos];
                    let after_tilde = &filename[tilde_pos + 1..];
                    // after_tilde = "20260327-143022.ext" or "20260327-143022"
                    let (ts, ext) = match after_tilde.rfind('.') {
                        Some(dot) => (&after_tilde[..dot], &after_tilde[dot..]),
                        None => (after_tilde, ""),
                    };
                    let original = if parent_dir.is_empty() {
                        format!("{}{}", stem, ext)
                    } else {
                        format!("{}/{}{}", parent_dir, stem, ext)
                    };
                    all.push(VersionEntry {
                        archive_path: path.to_path_buf(),
                        original_relative: original,
                        archived_at: ts.to_string(),
                        size: meta.len(),
                    });
                } else {
                    all.push(VersionEntry {
                        archive_path: path.to_path_buf(),
                        original_relative: rel,
                        archived_at: String::new(),
                        size: meta.len(),
                    });
                }
            }
        })?;
        all.sort_by(|a, b| b.archived_at.cmp(&a.archived_at));
        Ok(all)
    }

    /// Restore an archived version to its original location.
    /// Archives the current file first (if it exists) to prevent data loss.
    pub fn restore(&self, version: &VersionEntry) -> Result<(), String> {
        let target = self.sync_root.join(&version.original_relative);

        // Archive current file before overwriting (prevent data loss)
        if target.exists() && self.is_enabled() {
            if let Err(e) = self.archive(&target) {
                tracing::warn!("Failed to archive current file before restore: {}", e);
            }
        }

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create target dir: {}", e))?;
        }

        std::fs::copy(&version.archive_path, &target)
            .map_err(|e| format!("Failed to restore file: {}", e))?;

        info!(
            "[Versioning] Restored {} from {}",
            version.original_relative,
            version.archive_path.display()
        );

        Ok(())
    }

    /// Run cleanup based on the configured strategy.
    pub fn cleanup(&self) -> Result<CleanupStats, String> {
        if !self.versions_dir.exists() {
            return Ok(CleanupStats { deleted_count: 0, freed_bytes: 0 });
        }

        match &self.strategy {
            VersioningStrategy::Disabled => Ok(CleanupStats { deleted_count: 0, freed_bytes: 0 }),
            VersioningStrategy::TrashCan { max_age_days } => self.cleanup_by_age(*max_age_days),
            VersioningStrategy::Simple { max_copies } => self.cleanup_by_count(*max_copies),
            VersioningStrategy::Staggered => self.cleanup_staggered(),
        }
    }

    /// Delete versions older than max_age_days.
    fn cleanup_by_age(&self, max_age_days: u32) -> Result<CleanupStats, String> {
        let cutoff = std::time::SystemTime::now()
            - std::time::Duration::from_secs(u64::from(max_age_days) * 86400);
        let mut stats = CleanupStats { deleted_count: 0, freed_bytes: 0 };

        self.walk_versions(|path, meta| {
            if let Ok(modified) = meta.modified() {
                if modified < cutoff {
                    stats.freed_bytes += meta.len();
                    if std::fs::remove_file(path).is_ok() {
                        stats.deleted_count += 1;
                    }
                }
            }
        })?;

        self.cleanup_empty_dirs()?;
        Ok(stats)
    }

    /// Keep only the newest max_copies versions per original file.
    fn cleanup_by_count(&self, max_copies: u32) -> Result<CleanupStats, String> {
        let mut stats = CleanupStats { deleted_count: 0, freed_bytes: 0 };

        // Group by original file (stem before ~)
        let mut groups: std::collections::HashMap<String, Vec<(PathBuf, u64)>> =
            std::collections::HashMap::new();

        self.walk_versions(|path, meta| {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(tilde_pos) = name.find('~') {
                    // Group key: parent dir + stem
                    let parent = path.parent().unwrap_or(Path::new(""));
                    let key = format!("{}/{}", parent.display(), &name[..tilde_pos]);
                    groups.entry(key).or_default().push((path.to_path_buf(), meta.len()));
                }
            }
        })?;

        for (_key, mut versions) in groups {
            // Sort by filename (timestamp is in the name) — newest first
            versions.sort_by(|a, b| b.0.cmp(&a.0));
            for (path, size) in versions.into_iter().skip(max_copies as usize) {
                if std::fs::remove_file(&path).is_ok() {
                    stats.deleted_count += 1;
                    stats.freed_bytes += size;
                }
            }
        }

        self.cleanup_empty_dirs()?;
        Ok(stats)
    }

    /// Staggered cleanup: 1/hour for 24h, 1/day for 30d, 1/week for older.
    fn cleanup_staggered(&self) -> Result<CleanupStats, String> {
        let now = std::time::SystemTime::now();
        let mut stats = CleanupStats { deleted_count: 0, freed_bytes: 0 };

        let mut groups: std::collections::HashMap<String, Vec<(PathBuf, std::time::SystemTime, u64)>> =
            std::collections::HashMap::new();

        self.walk_versions(|path, meta| {
            if let (Some(name), Ok(modified)) = (path.file_name().and_then(|n| n.to_str()), meta.modified()) {
                if let Some(tilde_pos) = name.find('~') {
                    let parent = path.parent().unwrap_or(Path::new(""));
                    let key = format!("{}/{}", parent.display(), &name[..tilde_pos]);
                    groups.entry(key).or_default().push((path.to_path_buf(), modified, meta.len()));
                }
            }
        })?;

        for (_key, mut versions) in groups {
            versions.sort_by(|a, b| b.1.cmp(&a.1)); // Newest first

            let mut kept_in_hour: std::collections::HashMap<u64, bool> = std::collections::HashMap::new();
            let mut kept_in_day: std::collections::HashMap<u64, bool> = std::collections::HashMap::new();
            let mut kept_in_week: std::collections::HashMap<u64, bool> = std::collections::HashMap::new();

            for (path, modified, size) in &versions {
                let age_secs = now.duration_since(*modified).map(|d| d.as_secs()).unwrap_or(0);
                let keep = if age_secs < 86400 {
                    // < 24h: keep 1 per hour
                    let hour_bucket = age_secs / 3600;
                    kept_in_hour.insert(hour_bucket, true).is_none()
                } else if age_secs < 86400 * 30 {
                    // < 30d: keep 1 per day
                    let day_bucket = age_secs / 86400;
                    kept_in_day.insert(day_bucket, true).is_none()
                } else {
                    // > 30d: keep 1 per week
                    let week_bucket = age_secs / (86400 * 7);
                    kept_in_week.insert(week_bucket, true).is_none()
                };

                if !keep && std::fs::remove_file(path).is_ok() {
                    stats.deleted_count += 1;
                    stats.freed_bytes += size;
                }
            }
        }

        self.cleanup_empty_dirs()?;
        Ok(stats)
    }

    /// Walk all files in .aeroversions/ recursively.
    fn walk_versions(&self, mut callback: impl FnMut(&Path, std::fs::Metadata)) -> Result<(), String> {
        fn walk(dir: &Path, cb: &mut dyn FnMut(&Path, std::fs::Metadata)) -> Result<(), String> {
            if !dir.exists() {
                return Ok(());
            }
            let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
            for entry in entries.flatten() {
                let path = entry.path();
                if let Ok(meta) = path.metadata() {
                    if meta.is_dir() {
                        walk(&path, cb)?;
                    } else {
                        cb(&path, meta);
                    }
                }
            }
            Ok(())
        }
        walk(&self.versions_dir, &mut callback)
    }

    /// Remove empty directories inside .aeroversions/.
    fn cleanup_empty_dirs(&self) -> Result<(), String> {
        fn remove_empty(dir: &Path) -> Result<bool, String> {
            if !dir.exists() {
                return Ok(true);
            }
            let entries: Vec<_> = std::fs::read_dir(dir).map_err(|e| e.to_string())?.collect();
            let mut all_empty = true;
            for entry in entries.into_iter().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if !remove_empty(&path)? {
                        all_empty = false;
                    }
                } else {
                    all_empty = false;
                }
            }
            if all_empty {
                let _ = std::fs::remove_dir(dir);
            }
            Ok(all_empty)
        }
        remove_empty(&self.versions_dir)?;
        Ok(())
    }

    /// Calculate total disk usage of .aeroversions/.
    pub fn disk_usage(&self) -> u64 {
        let mut total = 0u64;
        self.walk_versions(|_, meta| {
            total += meta.len();
        }).ok();
        total
    }
}
