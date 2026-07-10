//! RemoteBackend trait: abstracts the active remote connection
//!
//! Tauri implementation wraps ProviderState + AppState mutexes.
//! CLI implementation owns a single StorageProvider directly.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::providers::{FileVersion, RemoteEntry, TrashEntry};
use crate::transfer_dag::TransferCapabilities;
use async_trait::async_trait;

/// Storage quota information
#[derive(Debug, Clone, serde::Serialize)]
pub struct StorageQuota {
    pub used: u64,
    pub total: u64,
    pub available: u64,
}

/// Abstraction over the active remote connection.
#[async_trait]
pub trait RemoteBackend: Send + Sync {
    /// Whether any remote provider is connected.
    async fn is_connected(&self) -> bool;

    /// List entries at a remote path.
    async fn list(&self, path: &str) -> Result<Vec<RemoteEntry>, String>;

    /// Get metadata for a single entry.
    async fn stat(&self, path: &str) -> Result<RemoteEntry, String>;

    /// Download a file to bytes (with 50MB guard).
    async fn download_to_bytes(&self, path: &str) -> Result<Vec<u8>, String>;

    /// Upload bytes to remote path.
    async fn upload_from_bytes(&self, data: &[u8], path: &str) -> Result<(), String>;

    /// Download a file to local path.
    async fn download(&self, remote: &str, local: &str) -> Result<(), String>;

    /// Upload a local file to remote path.
    async fn upload(&self, local: &str, remote: &str) -> Result<(), String>;

    /// Delete a remote file, or an EMPTY directory. A non-empty directory
    /// must fail here: recursion is opt-in through `delete_recursive`.
    async fn delete(&self, path: &str) -> Result<(), String>;

    /// Delete a directory and everything under it. The default refuses, so a
    /// backend that cannot recurse says so instead of silently deleting one
    /// entry and reporting success.
    async fn delete_recursive(&self, _path: &str) -> Result<(), String> {
        Err("recursive delete is not supported by this backend".to_string())
    }

    /// Create a remote directory.
    async fn mkdir(&self, path: &str) -> Result<(), String>;

    /// Rename/move a remote file.
    async fn rename(&self, from: &str, to: &str) -> Result<(), String>;

    /// Search for files matching a pattern.
    async fn search(&self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, String>;

    /// Get storage quota information.
    async fn storage_info(&self) -> Result<StorageQuota, String>;

    /// Transfer scheduler capabilities for the live provider, when the
    /// backend can expose them without credentials leaving the surface.
    async fn transfer_capabilities(&self) -> Result<Option<TransferCapabilities>, String> {
        Ok(None)
    }

    /// List previous versions of a file, newest first (empty when the provider
    /// has no version history). Defaults to unsupported for backends without a
    /// live provider.
    async fn list_versions(&self, _path: &str) -> Result<Vec<FileVersion>, String> {
        Err("version operations are not available on this backend".to_string())
    }

    /// Restore a file to a previous version (copy-forward, keeping history).
    async fn restore_version(&self, _path: &str, _version_id: &str) -> Result<(), String> {
        Err("version operations are not available on this backend".to_string())
    }

    /// Permanently delete (purge) one version or delete marker of a file.
    /// Irreversible.
    async fn delete_version(&self, _path: &str, _version_id: &str) -> Result<(), String> {
        Err("version operations are not available on this backend".to_string())
    }

    /// Browse the soft-delete trash: every version and delete marker under a
    /// prefix. With `include_noncurrent` false, only delete markers and each
    /// key's current version. S3-family only.
    async fn list_object_versions(
        &self,
        _prefix: &str,
        _include_noncurrent: bool,
    ) -> Result<Vec<TrashEntry>, String> {
        Err("trash operations are not available on this backend".to_string())
    }

    /// Empty the trash under a prefix, returning `(count, bytes)` of what was
    /// (or, with `dry_run`, would be) purged. Irreversible when executed.
    async fn empty_object_versions(
        &self,
        _prefix: &str,
        _include_noncurrent: bool,
        _dry_run: bool,
    ) -> Result<(u64, u64), String> {
        Err("trash operations are not available on this backend".to_string())
    }
}
