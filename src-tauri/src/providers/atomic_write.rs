//! Atomic file write helper for safe downloads.
//!
//! Prevents 0-byte files by writing to a `.aerotmp` temporary file first,
//! then atomically renaming to the final path only on success.
//! If the download fails mid-stream, the temp file is cleaned up.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// A guard that writes to a temp file and renames on commit.
/// If dropped without calling `commit()`, the temp file is deleted.
pub struct AtomicFile {
    temp_path: PathBuf,
    final_path: PathBuf,
    file: tokio::fs::File,
    committed: bool,
}

impl AtomicFile {
    /// Create a new atomic file writer. The temp file is created immediately.
    pub async fn new(final_path: &str) -> Result<Self, std::io::Error> {
        let final_path = PathBuf::from(final_path);
        let temp_path = Self::temp_path_for(&final_path);

        // Ensure parent directory exists
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let file = fs::File::create(&temp_path).await?;

        Ok(Self {
            temp_path,
            final_path,
            file,
            committed: false,
        })
    }

    /// Get a mutable reference to the underlying file for writing.
    pub fn file_mut(&mut self) -> &mut tokio::fs::File {
        &mut self.file
    }

    /// Write data to the temp file.
    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), std::io::Error> {
        self.file.write_all(buf).await
    }

    /// Flush and commit: rename temp file to final path.
    /// This is the only way to produce the final file.
    pub async fn commit(mut self) -> Result<(), std::io::Error> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        // Shutdown the file to release the handle before rename
        self.file.shutdown().await?;

        fs::rename(&self.temp_path, &self.final_path).await?;
        self.committed = true;
        Ok(())
    }

    /// Generate temp path for a given final path.
    fn temp_path_for(path: &Path) -> PathBuf {
        let mut temp = path.as_os_str().to_owned();
        temp.push(".aerotmp");
        PathBuf::from(temp)
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        if !self.committed {
            // Best-effort cleanup of temp file
            let temp = self.temp_path.clone();
            // Use std::fs since we're in Drop (not async)
            let _ = std::fs::remove_file(&temp);
        }
    }
}
