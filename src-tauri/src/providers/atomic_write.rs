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

/// A download file guard that preserves partial data on failure for later resume.
///
/// On fresh download: writes to `.aerotmp` (like `AtomicFile`).
/// On resume: detects existing `.aerotmp`, opens in append mode at the existing offset.
/// On failure: keeps `.aerotmp` intact so the next download can resume from where it left off.
/// On success: renames `.aerotmp` to the final path (same as `AtomicFile`).
pub struct ResumableFile {
    temp_path: PathBuf,
    final_path: PathBuf,
    file: tokio::fs::File,
    committed: bool,
    /// Byte offset we are resuming from (0 = fresh download).
    offset: u64,
}

impl ResumableFile {
    /// Open a resumable file writer.
    ///
    /// If a `.aerotmp` file already exists, it is opened in append mode and the
    /// existing byte count is returned as the offset. The caller should send an
    /// HTTP `Range: bytes=<offset>-` header and append the response body.
    ///
    /// If no `.aerotmp` exists, a fresh file is created (offset = 0).
    pub async fn open(final_path: &str) -> Result<Self, std::io::Error> {
        let final_path = PathBuf::from(final_path);
        let temp_path = AtomicFile::temp_path_for(&final_path);

        // Ensure parent directory exists
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let (file, offset) = if temp_path.exists() {
            // Resume: open existing .aerotmp in append mode
            let meta = fs::metadata(&temp_path).await?;
            let offset = meta.len();
            let file = fs::OpenOptions::new()
                .append(true)
                .open(&temp_path)
                .await?;
            (file, offset)
        } else {
            // Fresh: create new .aerotmp
            let file = fs::File::create(&temp_path).await?;
            (file, 0)
        };

        Ok(Self {
            temp_path,
            final_path,
            file,
            committed: false,
            offset,
        })
    }

    /// Create a fresh resumable file, discarding any existing `.aerotmp`.
    /// Use this when the remote file has changed and partial data is stale.
    pub async fn open_fresh(final_path: &str) -> Result<Self, std::io::Error> {
        let final_path_buf = PathBuf::from(final_path);
        let temp_path = AtomicFile::temp_path_for(&final_path_buf);

        if let Some(parent) = final_path_buf.parent() {
            fs::create_dir_all(parent).await?;
        }

        let file = fs::File::create(&temp_path).await?;

        Ok(Self {
            temp_path,
            final_path: final_path_buf,
            file,
            committed: false,
            offset: 0,
        })
    }

    /// Byte offset of existing partial data (0 = fresh download).
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Write data to the temp file (appending after any existing partial data).
    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), std::io::Error> {
        self.file.write_all(buf).await
    }

    /// Get a mutable reference to the underlying file for writing.
    pub fn file_mut(&mut self) -> &mut tokio::fs::File {
        &mut self.file
    }

    /// Flush and commit: rename temp file to final path.
    pub async fn commit(mut self) -> Result<(), std::io::Error> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        self.file.shutdown().await?;

        fs::rename(&self.temp_path, &self.final_path).await?;
        self.committed = true;
        Ok(())
    }

    /// Discard partial data and remove the `.aerotmp` file.
    pub async fn discard(mut self) -> Result<(), std::io::Error> {
        self.committed = true; // prevent Drop from running
        let _ = self.file.shutdown().await;
        fs::remove_file(&self.temp_path).await
    }
}

impl Drop for ResumableFile {
    fn drop(&mut self) {
        if !self.committed {
            // INTENTIONALLY keep .aerotmp on failure — this is the whole point
            // of ResumableFile: partial data is preserved for later resume.
            tracing::debug!(
                "ResumableFile: keeping partial download at {}",
                self.temp_path.display()
            );
        }
    }
}
