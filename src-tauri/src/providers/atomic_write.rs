//! Atomic file write helper for safe downloads.
//!
//! Prevents 0-byte files by writing to a `.aerotmp` temporary file first,
//! then atomically renaming to the final path only on success.
//! If the download fails mid-stream, the temp file is cleaned up.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// Global flag: when true, skip .aerotmp and write directly to the final path.
/// Set via `set_inplace_mode(true)` from the CLI when --inplace is passed.
static INPLACE_MODE: AtomicBool = AtomicBool::new(false);

/// Enable or disable inplace mode (skip .aerotmp temp files).
pub fn set_inplace_mode(enabled: bool) {
    INPLACE_MODE.store(enabled, Ordering::Relaxed);
}

fn inplace_active() -> bool {
    INPLACE_MODE.load(Ordering::Relaxed)
}

/// A guard that writes to a temp file and renames on commit.
/// If dropped without calling `commit()`, the temp file is deleted.
/// In inplace mode, writes directly to the final path (no temp, no rename).
pub struct AtomicFile {
    temp_path: PathBuf,
    final_path: PathBuf,
    file: tokio::fs::File,
    committed: bool,
    inplace: bool,
}

impl AtomicFile {
    /// Create a new atomic file writer. The temp file is created immediately.
    /// In inplace mode, writes directly to the final path.
    pub async fn new(final_path: &str) -> Result<Self, std::io::Error> {
        let final_path = PathBuf::from(final_path);
        let inplace = inplace_active();
        let temp_path = if inplace {
            final_path.clone()
        } else {
            Self::temp_path_for(&final_path)
        };

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
            inplace,
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
    /// In inplace mode, no rename is needed (already writing to final path).
    pub async fn commit(mut self) -> Result<(), std::io::Error> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        self.file.shutdown().await?;

        if !self.inplace {
            fs::rename(&self.temp_path, &self.final_path).await?;
        }
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
        if !self.committed && !self.inplace {
            // Best-effort cleanup of temp file (skip in inplace mode — file is the final path)
            let temp = self.temp_path.clone();
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
    inplace: bool,
}

impl ResumableFile {
    /// Open a resumable file writer.
    /// In inplace mode, writes directly to the final path (no .aerotmp).
    pub async fn open(final_path: &str) -> Result<Self, std::io::Error> {
        let final_path = PathBuf::from(final_path);
        let inplace = inplace_active();
        let temp_path = if inplace {
            final_path.clone()
        } else {
            AtomicFile::temp_path_for(&final_path)
        };

        // Ensure parent directory exists
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let (file, offset) = if temp_path.exists() {
            // Resume: open existing file in append mode
            let meta = fs::metadata(&temp_path).await?;
            let offset = meta.len();
            let file = fs::OpenOptions::new().append(true).open(&temp_path).await?;
            (file, offset)
        } else {
            // Fresh: create new file
            let file = fs::File::create(&temp_path).await?;
            (file, 0)
        };

        Ok(Self {
            temp_path,
            final_path,
            file,
            committed: false,
            offset,
            inplace,
        })
    }

    /// Create a fresh resumable file, discarding any existing partial data.
    pub async fn open_fresh(final_path: &str) -> Result<Self, std::io::Error> {
        let final_path_buf = PathBuf::from(final_path);
        let inplace = inplace_active();
        let temp_path = if inplace {
            final_path_buf.clone()
        } else {
            AtomicFile::temp_path_for(&final_path_buf)
        };

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
            inplace,
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
    /// In inplace mode, no rename is needed.
    pub async fn commit(mut self) -> Result<(), std::io::Error> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        self.file.shutdown().await?;

        if !self.inplace {
            fs::rename(&self.temp_path, &self.final_path).await?;
        }
        self.committed = true;
        Ok(())
    }

    /// Discard partial data and remove the temp file.
    pub async fn discard(mut self) -> Result<(), std::io::Error> {
        self.committed = true; // prevent Drop from running
        let _ = self.file.shutdown().await;
        if !self.inplace {
            fs::remove_file(&self.temp_path).await
        } else {
            // In inplace mode, the temp file IS the final file — remove it
            fs::remove_file(&self.final_path).await
        }
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
