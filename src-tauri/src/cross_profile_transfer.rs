// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Cross-profile transfer engine — MVP backend.
//!
//! Copies files between two remote profiles using a local temp-file bridge.
//! No destructive operations (no delete, no move, no sync).

// Phase 1: module not yet consumed by CLI (Phase 3). Suppress dead_code until then.
#![allow(dead_code)]

use crate::providers::{ProviderError, StorageProvider};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tempfile::NamedTempFile;

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
pub async fn copy_one_file(
    source: &mut dyn StorageProvider,
    dest: &mut dyn StorageProvider,
    source_path: &str,
    dest_path: &str,
) -> Result<(), ProviderError> {
    // Create a temp file that auto-deletes on drop
    let tmp = NamedTempFile::new()
        .map_err(|e| ProviderError::TransferFailed(format!("temp file creation failed: {e}")))?;
    let tmp_path = tmp.path().to_string_lossy().to_string();

    // Download from source to temp file
    source.download(source_path, &tmp_path, None).await?;

    // Ensure parent directory exists on destination
    ensure_parent_dir(dest, dest_path).await;

    // Upload from temp file to destination
    dest.upload(&tmp_path, dest_path, None).await?;

    // tmp is dropped here, removing the temp file
    Ok(())
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
        assert_eq!(res.planned_files, res.transferred_files + res.skipped_files + res.failed_files);
    }
}
