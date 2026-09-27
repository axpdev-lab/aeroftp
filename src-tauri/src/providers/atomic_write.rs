//! Atomic file write helper for safe downloads.
//!
//! Prevents 0-byte files by writing to a `.aerotmp` temporary file first,
//! then atomically renaming to the final path only on success.
//! If the download fails mid-stream, the temp file is cleaned up.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

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

pub(crate) fn inplace_active() -> bool {
    INPLACE_MODE.load(Ordering::Relaxed)
}

/// Generate the dedicated temp path used by out-of-order read-ahead downloads.
/// It must stay distinct from `.aerotmp`: a pre-sized, out-of-order partial is
/// not safe for the length-based resumable writer to append to or commit.
pub fn readahead_temp_path_for(final_path: &Path) -> PathBuf {
    let mut temp = final_path.as_os_str().to_owned();
    temp.push(".aeroardtmp");
    PathBuf::from(temp)
}

/// True for local download sidecars that filesystem watchers must hide.
pub fn is_download_temp_path(path: &str) -> bool {
    path.ends_with(".aerotmp") || path.ends_with(".aeroardtmp")
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

        let file = if inplace {
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)
                .await?
        } else {
            fs::File::from_std(Self::create_locked(&temp_path)?)
        };

        Ok(Self {
            temp_path,
            final_path,
            file,
            committed: false,
            inplace,
        })
    }

    /// Create the temporary and hold an exclusive lock on it for the
    /// writer's lifetime (it goes with the file handle). One already at its
    /// name is a live writer's or a stale one, and its lock tells which. A
    /// live writer's (two downloads to one local path: the same file queued
    /// twice, a sync and a manual `get`, two CLI runs) is left alone, and this
    /// download fails on it as before: taking it let this writer's half-written
    /// file be committed by the other as complete. A stale one (a download
    /// killed, or dropped while its create was on its way) is removed and
    /// created anew, as `ResumableFile::open_fresh` does (RESUME-01): nothing
    /// resumes an atomic download's temporary, and keeping it failed every
    /// later download of the file. A symlink there is removed as a link, never
    /// followed.
    fn create_locked(temp_path: &Path) -> std::io::Result<std::fs::File> {
        let create = || -> std::io::Result<std::fs::File> {
            let file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(temp_path)?;
            file.try_lock().map_err(std::io::Error::from)?;
            Ok(file)
        };
        match create() {
            Err(taken) if taken.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::symlink_metadata(temp_path)?.file_type().is_file() {
                    let existing = std::fs::OpenOptions::new().write(true).open(temp_path)?;
                    match existing.try_lock() {
                        Ok(()) => {}
                        Err(std::fs::TryLockError::WouldBlock) => return Err(taken),
                        Err(std::fs::TryLockError::Error(e)) => return Err(e),
                    }
                }
                std::fs::remove_file(temp_path)?;
                create()
            }
            created => created,
        }
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
            // Best-effort cleanup of temp file (skip in inplace mode: file is the final path)
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
        Self::open_in(final_path, inplace_active()).await
    }

    /// [`Self::open`] with the in-place mode given rather than read from the
    /// process-wide flag, which a test cannot set without racing the others.
    async fn open_in(final_path: &str, inplace: bool) -> Result<Self, std::io::Error> {
        let final_path = PathBuf::from(final_path);
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
            let symlink_meta = fs::symlink_metadata(&temp_path).await?;
            if symlink_meta.file_type().is_symlink() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "refusing to resume through symlinked .aerotmp file",
                ));
            }
            // Resume: open existing file in append mode
            let meta = fs::metadata(&temp_path).await?;
            if !meta.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "resume target is not a regular file",
                ));
            }
            let offset = meta.len();
            let file = fs::OpenOptions::new().append(true).open(&temp_path).await?;
            (file, offset)
        } else {
            // Fresh: create new file
            let file = if inplace {
                fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&temp_path)
                    .await?
            } else {
                fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&temp_path)
                    .await?
            };
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

        let file = if inplace {
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)
                .await?
        } else {
            // RESUME-01: open_fresh's contract is to discard any existing partial
            // data. A stale `.aerotmp` from a previous interrupted download would
            // otherwise make create_new fail with AlreadyExists, wedging every
            // retry until the user removes it by hand. Remove it first (best
            // effort), then create the fresh temp so the create_new race guard is
            // preserved for genuinely concurrent writers.
            let _ = fs::remove_file(&temp_path).await;
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp_path)
                .await?
        };

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
            // In inplace mode, the temp file IS the final file: remove it
            fs::remove_file(&self.final_path).await
        }
    }
}

impl Drop for ResumableFile {
    fn drop(&mut self) {
        if !self.committed {
            // INTENTIONALLY keep .aerotmp on failure: this is the whole point
            // of ResumableFile: partial data is preserved for later resume.
            tracing::debug!(
                "ResumableFile: keeping partial download at {}",
                self.temp_path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_classifier_recognizes_serial_and_readahead_sidecars() {
        assert!(is_download_temp_path("file.bin.aerotmp"));
        assert!(is_download_temp_path("file.bin.aeroardtmp"));
        assert!(!is_download_temp_path("file.bin"));
        assert!(!is_download_temp_path("file.bin.aerardtmp"));
    }

    /// Review of round 2 of #951: a `.aerotmp` left by a download that was
    /// killed, or dropped while its create was on its way, made every later
    /// atomic download of the file fail on `create_new` until it was removed
    /// by hand. Nothing holds its lock: it is stale, and replaced.
    #[tokio::test]
    async fn an_atomic_download_replaces_a_stale_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let temp = dir.path().join("f.bin.aerotmp");
        std::fs::write(&temp, b"stale").unwrap();
        let mut file = AtomicFile::new(path.to_str().unwrap())
            .await
            .expect("a stale temporary must not block the download");
        file.write_all(b"new").await.unwrap();
        file.commit().await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!temp.exists());
    }

    /// Verification of round 3 of #951: replacing any temporary found at the
    /// name took a live writer's too. Two downloads to one local path: the
    /// second removed the first's `.aerotmp` and created its own, and the
    /// first's commit renamed the second's half-written file onto the final
    /// path, reported as complete. The second fails while the first lives,
    /// and goes ahead once the first is gone.
    #[tokio::test]
    async fn a_live_writers_temporary_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        let path = path.to_str().unwrap();
        let first = AtomicFile::new(path).await.expect("the first writer");
        let second = AtomicFile::new(path).await;
        assert_eq!(
            second.err().map(|e| e.kind()),
            Some(std::io::ErrorKind::AlreadyExists),
            "a live writer's temporary was taken"
        );
        drop(first);
        let mut third = AtomicFile::new(path)
            .await
            .expect("once the first writer is gone");
        third.write_all(b"new").await.unwrap();
        third.commit().await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }

    /// An in-place download over a file the user already has: that file is
    /// the destination, not a part of this download. It was taken for one: a
    /// file as long as the remote one was reported complete with its old
    /// content, and a shorter one had the remote's tail appended to it.
    #[tokio::test]
    async fn an_inplace_download_starts_over_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"old content").unwrap();
        let path = path.to_str().unwrap();
        let mut file = ResumableFile::open_in(path, true).await.unwrap();
        assert_eq!(file.offset(), 0, "the user's file was taken for a part");
        file.write_all(b"new").await.unwrap();
        file.commit().await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }
}
