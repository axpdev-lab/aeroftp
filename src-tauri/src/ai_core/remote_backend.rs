//! RemoteBackend trait — abstracts the active remote connection
//!
//! Tauri implementation wraps ProviderState + AppState mutexes.
//! CLI implementation owns a single StorageProvider directly.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use crate::providers::RemoteEntry;

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

    /// Delete a remote file or directory.
    async fn delete(&self, path: &str) -> Result<(), String>;

    /// Create a remote directory.
    async fn mkdir(&self, path: &str) -> Result<(), String>;

    /// Rename/move a remote file.
    async fn rename(&self, from: &str, to: &str) -> Result<(), String>;

    /// Search for files matching a pattern.
    async fn search(&self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, String>;

    /// Get storage quota information.
    async fn storage_info(&self) -> Result<StorageQuota, String>;
}
