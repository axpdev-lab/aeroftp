// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Cross-profile transfer engine — MVP backend.
//!
//! Copies files between two remote profiles using a local temp-file bridge.
//! No destructive operations (no delete, no move, no sync).

use crate::delta_sync_rsync::{try_delta_transfer, SyncDirection};
use crate::providers::{ProviderError, StorageProvider};
use filetime::FileTime;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tempfile::NamedTempFile;

/// Maximum BFS scan depth — consistent with CLI's MAX_SCAN_DEPTH.
const MAX_SCAN_DEPTH: usize = 100;
/// Maximum entries to collect — consistent with CLI's MAX_SCAN_ENTRIES.
const MAX_SCAN_ENTRIES: usize = 500_000;

// ── Request / Plan / Result structs ────────────────────────────────────────

/// Describes a cross-profile transfer request coming from CLI or GUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossProfileTransferRequest {
    pub source_profile: String,
    pub dest_profile: String,
    pub source_path: String,
    pub dest_path: String,
    pub recursive: bool,
    pub dry_run: bool,
    pub skip_existing: bool,
}

/// A single entry in the transfer plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossProfileTransferEntry {
    pub source_path: String,
    pub dest_path: String,
    pub display_name: String,
    pub size: u64,
    pub modified: Option<String>,
    pub is_dir: bool,
}

/// The full transfer plan built before execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossProfileTransferPlan {
    pub source_profile: String,
    pub dest_profile: String,
    pub entries: Vec<CrossProfileTransferEntry>,
    pub total_files: u64,
    pub total_bytes: u64,
}

/// Summary returned after executing a transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossProfileTransferResult {
    pub planned_files: u64,
    pub transferred_files: u64,
    pub skipped_files: u64,
    pub failed_files: u64,
    pub total_bytes: u64,
    pub duration_ms: u64,
}

// ── Core: single-file copy ─────────────────────────────────────────────────

/// Copy a single file from `source` to `dest` using a local temp-file bridge.
///
/// Flow: source.download() -> temp file -> dest.upload()
///
/// The temp file is automatically cleaned up when `NamedTempFile` is dropped.
/// Parent directories on the destination are created if missing.
///
/// When `dest` is an SFTP provider with key-based auth and a remote rsync
/// helper, the upload step is replaced by AeroRsync delta transfer — only
/// the bytes that differ from any pre-existing file at `dest_path` go on the
/// wire. Hard errors (host-key mismatch, protocol invariant violation) are
/// propagated unchanged; soft fallbacks ("file too small", "no key on disk")
/// transparently route back to the classic upload.
pub async fn copy_one_file(
    source: &mut dyn StorageProvider,
    dest: &mut dyn StorageProvider,
    source_path: &str,
    dest_path: &str,
    source_modified: Option<&str>,
) -> Result<(), ProviderError> {
    // Create a temp file that auto-deletes on drop
    let tmp = NamedTempFile::new()
        .map_err(|e| ProviderError::TransferFailed(format!("temp file creation failed: {e}")))?;
    let tmp_path = tmp.path().to_string_lossy().to_string();

    // Download from source to temp file
    source.download(source_path, &tmp_path, None).await?;

    preserve_temp_mtime(tmp.path(), source_modified);

    // Ensure parent directory exists on destination
    ensure_parent_dir(dest, dest_path).await;

    // Try delta transfer first (SFTP-only today). Returns None for non-SFTP
    // destinations or when downcast/probe declines — in both cases we proceed
    // to the classic upload below.
    if let Some(result) = try_delta_transfer(dest, SyncDirection::Upload, tmp.path(), dest_path).await {
        if result.used_delta {
            // Delta path completed the transfer; we're done.
            return Ok(());
        }
        if let Some(err) = result.hard_error {
            // Hard error must surface — never silently retry the classic path
            // when the delta layer rejected for a security/protocol reason.
            return Err(ProviderError::TransferFailed(format!(
                "delta transfer rejected: {err}"
            )));
        }
        // fallback_reason set: declined gracefully, fall through to classic.
    }

    // Classic upload: from temp file to destination
    dest.upload(&tmp_path, dest_path, None).await?;

    // tmp is dropped here, removing the temp file
    Ok(())
}

// ── Planning: collect + filter + plan ──────────────────────────────────────

/// Recursively collect source entries using BFS, respecting depth and entry limits.
///
/// If `recursive` is false and the path is a directory, returns an error.
/// If `recursive` is true, performs a bounded BFS scan.
/// If the path is a file, returns a single entry.
pub async fn collect_source_entries(
    source: &mut dyn StorageProvider,
    root: &str,
    recursive: bool,
) -> Result<Vec<CrossProfileTransferEntry>, ProviderError> {
    if is_virtual_root_path(root) {
        return collect_virtual_root_entries(source, root, recursive).await;
    }

    // Check if root is a file (stat it)
    let root_stat = source.stat(root).await?;

    if !root_stat.is_dir {
        // Single file
        return Ok(vec![CrossProfileTransferEntry {
            source_path: root.to_string(),
            dest_path: String::new(), // filled by plan_transfer
            display_name: fallback_display_name(&root_stat.name, &root_stat.path),
            size: root_stat.size,
            modified: root_stat.modified.clone(),
            is_dir: false,
        }]);
    }

    // It's a directory — recursive must be true
    if !recursive {
        return Err(ProviderError::InvalidPath(format!(
            "'{}' is a directory; use --recursive to transfer directories",
            root
        )));
    }

    let mut entries = Vec::new();
    let mut queue: Vec<(String, usize)> = vec![(root_stat.path, 0)];

    while let Some((dir, depth)) = queue.pop() {
        if depth >= MAX_SCAN_DEPTH {
            continue;
        }
        if entries.len() >= MAX_SCAN_ENTRIES {
            break;
        }

        let listing = match source.list(&dir).await {
            Ok(l) => l,
            Err(e) => {
                // Skip unlistable dirs (permission errors, etc.) — same as CLI pattern
                tracing::warn!("cannot list {}: {}", dir, e);
                continue;
            }
        };

        for e in listing {
            if entries.len() >= MAX_SCAN_ENTRIES {
                break;
            }
            if e.is_dir {
                queue.push((e.path, depth + 1));
            } else {
                let display_name = fallback_display_name(&e.name, &e.path);
                entries.push(CrossProfileTransferEntry {
                    source_path: e.path,
                    dest_path: String::new(), // filled by plan_transfer
                    display_name,
                    size: e.size,
                    modified: e.modified,
                    is_dir: false,
                });
            }
        }
    }

    Ok(entries)
}

async fn collect_virtual_root_entries(
    source: &mut dyn StorageProvider,
    root: &str,
    recursive: bool,
) -> Result<Vec<CrossProfileTransferEntry>, ProviderError> {
    if !recursive {
        return Err(ProviderError::InvalidPath(format!(
            "'{}' is a directory; use --recursive to transfer directories",
            root
        )));
    }

    let mut entries = Vec::new();
    let mut queue: Vec<(String, usize)> = vec![(root.to_string(), 0)];

    while let Some((dir, depth)) = queue.pop() {
        if depth >= MAX_SCAN_DEPTH {
            continue;
        }
        if entries.len() >= MAX_SCAN_ENTRIES {
            break;
        }

        let listing = match source.list(&dir).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("cannot list {}: {}", dir, e);
                continue;
            }
        };

        for e in listing {
            if entries.len() >= MAX_SCAN_ENTRIES {
                break;
            }
            if e.is_dir {
                queue.push((e.path, depth + 1));
            } else {
                let display_name = fallback_display_name(&e.name, &e.path);
                entries.push(CrossProfileTransferEntry {
                    source_path: e.path,
                    dest_path: String::new(),
                    display_name,
                    size: e.size,
                    modified: e.modified,
                    is_dir: false,
                });
            }
        }
    }

    Ok(entries)
}

/// Check whether a file should be skipped because it already exists on the
/// destination with matching size and mtime.
///
/// Returns `Ok(true)` to skip, `Ok(false)` to transfer, or propagates errors
/// (except NotFound, which means "not present → transfer").
pub async fn should_skip_existing(
    dest: &mut dyn StorageProvider,
    dest_path: &str,
    source_entry: &CrossProfileTransferEntry,
) -> Result<bool, ProviderError> {
    match dest.stat(dest_path).await {
        Ok(existing) => {
            let same_size = existing.size == source_entry.size;
            let same_mtime = match (&existing.modified, &source_entry.modified) {
                (Some(a), Some(b)) => a == b,
                // If either side lacks mtime, we can't compare — don't skip
                _ => false,
            };
            Ok(same_size && same_mtime)
        }
        Err(ProviderError::NotFound(_)) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Build a transfer plan by collecting source entries and computing destination paths.
///
/// This function does NOT execute any transfer and does NOT filter skip-existing
/// (filtering happens at execution time so the caller can track skip counts).
/// The full plan is always returned, suitable for both dry-run display and execution.
pub async fn plan_transfer(
    source: &mut dyn StorageProvider,
    _dest: &mut dyn StorageProvider,
    request: &CrossProfileTransferRequest,
) -> Result<CrossProfileTransferPlan, ProviderError> {
    let source_path = resolved_source_path(source, &request.source_path).await?;
    let root_stat = effective_root_stat(source, &source_path).await?;

    // 1. Collect source entries
    let mut entries = collect_source_entries(source, &source_path, request.recursive).await?;

    // 2. Compute destination paths
    if !root_stat.is_dir {
        // Single file: dest_path is the request's dest_path directly
        entries[0].dest_path = request.dest_path.clone();
    } else {
        // Recursive: strip source root and append to dest_path.
        // Some providers return canonical paths that differ from the caller input,
        // so we accept both the request path and the provider-reported root path.
        let source_roots = source_root_candidates(&source_path, &root_stat.path);
        for entry in &mut entries {
            entry.dest_path = map_dest_path(&source_roots, &entry.source_path, &request.dest_path);
        }
    }

    let total_files = entries.len() as u64;
    let total_bytes = entries.iter().map(|e| e.size).sum();

    Ok(CrossProfileTransferPlan {
        source_profile: request.source_profile.clone(),
        dest_profile: request.dest_profile.clone(),
        entries,
        total_files,
        total_bytes,
    })
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Best-effort creation of the parent directory on the destination provider.
/// Failures are silently ignored because the directory may already exist
/// or the provider may not require explicit mkdir (e.g. S3).
async fn ensure_parent_dir(dest: &mut dyn StorageProvider, dest_path: &str) {
    if let Some(parent) = Path::new(dest_path).parent() {
        let parent_str = parent.to_string_lossy();
        if !parent_str.is_empty() && parent_str != "/" {
            let _ = dest.mkdir(&parent_str).await;
        }
    }
}

/// Ensure a directory path ends with '/'.
fn normalize_dir(path: &str) -> String {
    if path.ends_with('/') {
        path.to_string()
    } else {
        format!("{}/", path)
    }
}

fn is_virtual_root_path(path: &str) -> bool {
    let trimmed = path.trim();
    trimmed.is_empty() || trimmed == "/" || trimmed == "."
}

fn fallback_display_name(name: &str, path: &str) -> String {
    let trimmed = name.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }

    Path::new(path)
        .file_name()
        .map(|segment| segment.to_string_lossy().to_string())
        .filter(|segment| !segment.trim().is_empty())
        .unwrap_or_else(|| path.trim_matches('/').to_string())
}

async fn resolved_source_path(
    source: &mut dyn StorageProvider,
    requested_path: &str,
) -> Result<String, ProviderError> {
    if is_virtual_root_path(requested_path) {
        source.pwd().await
    } else {
        Ok(requested_path.to_string())
    }
}

async fn effective_root_stat(
    source: &mut dyn StorageProvider,
    source_path: &str,
) -> Result<crate::providers::types::RemoteEntry, ProviderError> {
    if is_virtual_root_path(source_path) {
        return Ok(crate::providers::types::RemoteEntry {
            name: "/".to_string(),
            path: source_path.to_string(),
            is_dir: true,
            size: 0,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: std::collections::HashMap::new(),
        });
    }

    let stat = source.stat(source_path).await?;
    if stat.name.trim().is_empty() && is_virtual_root_path(&stat.path) {
        Ok(crate::providers::types::RemoteEntry {
            name: "/".to_string(),
            path: source_path.to_string(),
            is_dir: true,
            size: 0,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: std::collections::HashMap::new(),
        })
    } else {
        Ok(stat)
    }
}

fn preserve_temp_mtime(path: &Path, source_modified: Option<&str>) {
    let Some(source_modified) = source_modified else {
        return;
    };

    let Some(file_time) = parse_file_time(source_modified) else {
        tracing::debug!(
            "cross-profile: unsupported mtime format '{}'",
            source_modified
        );
        return;
    };

    if let Err(err) = filetime::set_file_mtime(path, file_time) {
        tracing::debug!("cross-profile: failed to preserve temp-file mtime: {}", err);
    }
}

fn parse_file_time(value: &str) -> Option<FileTime> {
    if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(FileTime::from_unix_time(
            ts.timestamp(),
            ts.timestamp_subsec_nanos(),
        ));
    }
    if let Ok(ts) = chrono::DateTime::parse_from_rfc2822(value) {
        return Some(FileTime::from_unix_time(
            ts.timestamp(),
            ts.timestamp_subsec_nanos(),
        ));
    }
    if let Ok(ts) = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%SZ") {
        return Some(FileTime::from_unix_time(ts.and_utc().timestamp(), 0));
    }
    None
}

/// Map a source file path to its corresponding destination path.
///
/// For single files: dest_path is used directly.
/// For recursive: strips the source root prefix and appends to dest_path.
fn map_dest_path(source_roots: &[String], source_file: &str, dest_base: &str) -> String {
    let relative = source_roots
        .iter()
        .find_map(|root| source_file.strip_prefix(root))
        .unwrap_or(source_file)
        .trim_start_matches('/');

    if relative.is_empty() {
        // Single file case — source_file == source_root (without trailing /)
        dest_base.to_string()
    } else {
        let dest_base = if dest_base.ends_with('/') {
            dest_base.to_string()
        } else {
            format!("{}/", dest_base)
        };
        format!("{}{}", dest_base, relative)
    }
}

fn source_root_candidates(request_path: &str, provider_root: &str) -> Vec<String> {
    let mut roots = vec![normalize_dir(request_path)];
    let provider_root = normalize_dir(provider_root);
    if !roots.iter().any(|root| root == &provider_root) {
        roots.push(provider_root);
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serialization_roundtrip() {
        let req = CrossProfileTransferRequest {
            source_profile: "Source SFTP".into(),
            dest_profile: "Dest S3".into(),
            source_path: "/data/file.txt".into(),
            dest_path: "/backup/file.txt".into(),
            recursive: true,
            dry_run: false,
            skip_existing: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CrossProfileTransferRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.source_profile, "Source SFTP");
        assert!(back.recursive);
        assert!(back.skip_existing);
    }

    #[test]
    fn plan_empty_entries() {
        let plan = CrossProfileTransferPlan {
            source_profile: "A".into(),
            dest_profile: "B".into(),
            entries: vec![],
            total_files: 0,
            total_bytes: 0,
        };
        assert_eq!(plan.entries.len(), 0);
        assert_eq!(plan.total_files, 0);
    }

    #[test]
    fn plan_with_entries() {
        let entry = CrossProfileTransferEntry {
            source_path: "/src/a.txt".into(),
            dest_path: "/dst/a.txt".into(),
            display_name: "a.txt".into(),
            size: 1024,
            modified: Some("2026-01-01T00:00:00Z".into()),
            is_dir: false,
        };
        let plan = CrossProfileTransferPlan {
            source_profile: "A".into(),
            dest_profile: "B".into(),
            entries: vec![entry.clone()],
            total_files: 1,
            total_bytes: 1024,
        };
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].size, 1024);
        assert!(!plan.entries[0].is_dir);
    }

    #[test]
    fn result_defaults() {
        let res = CrossProfileTransferResult {
            planned_files: 10,
            transferred_files: 8,
            skipped_files: 1,
            failed_files: 1,
            total_bytes: 2048,
            duration_ms: 500,
        };
        assert_eq!(
            res.planned_files,
            res.transferred_files + res.skipped_files + res.failed_files
        );
    }

    // ── Phase 2 tests ──────────────────────────────────────────────────

    #[test]
    fn normalize_dir_adds_slash() {
        assert_eq!(normalize_dir("/data"), "/data/");
        assert_eq!(normalize_dir("/data/"), "/data/");
        assert_eq!(normalize_dir("/"), "/");
    }

    #[test]
    fn map_dest_path_root_file() {
        // File directly under source root
        assert_eq!(
            map_dest_path(&["/data/".to_string()], "/data/file.txt", "/backup/"),
            "/backup/file.txt"
        );
    }

    #[test]
    fn map_dest_path_recursive() {
        let source_roots = vec!["/data/".to_string()];
        assert_eq!(
            map_dest_path(&source_roots, "/data/a.txt", "/backup/"),
            "/backup/a.txt"
        );
        assert_eq!(
            map_dest_path(&source_roots, "/data/sub/b.txt", "/backup/"),
            "/backup/sub/b.txt"
        );
        assert_eq!(
            map_dest_path(&source_roots, "/data/sub/deep/c.txt", "/backup"),
            "/backup/sub/deep/c.txt"
        );
    }

    #[test]
    fn map_dest_path_dest_without_trailing_slash() {
        let dest = map_dest_path(&["/src/".to_string()], "/src/file.txt", "/dst");
        assert_eq!(dest, "/dst/file.txt");
    }

    #[test]
    fn map_dest_path_accepts_provider_canonical_root() {
        let roots = source_root_candidates("cli_test/copilot", "/home/ftp/cli_test/copilot");
        let dest = map_dest_path(&roots, "/home/ftp/cli_test/copilot/sub/file.txt", "/backup");
        assert_eq!(dest, "/backup/sub/file.txt");
    }

    #[test]
    fn parse_file_time_supports_rfc3339() {
        assert!(parse_file_time("2026-04-09T19:00:08Z").is_some());
    }

    #[test]
    fn parse_file_time_supports_rfc2822() {
        assert!(parse_file_time("Thu, 09 Apr 2026 19:00:08 GMT").is_some());
    }

    #[test]
    fn parse_file_time_supports_legacy_utc() {
        assert!(parse_file_time("2026-04-09 19:00:08Z").is_some());
    }
}
