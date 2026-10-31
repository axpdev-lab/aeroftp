//! The local file a chunked upload reads, checked before the upload commits.
//!
//! A chunked upload reads the file part by part and publishes the remote object
//! only at its commit (Azure `Put Block List`, the Box and Dropbox session
//! commit, the Drime multipart completion). A file rewritten while it is being
//! read is uploaded as a mixture: the first parts come from one version, the
//! last from the other, every length matches, and the commit publishes a file
//! that never existed on disk. Reading the parts in parallel is not what causes
//! it; a single sequential read of a file being rewritten mixes the same way.
//!
//! [`UploadSource`] takes the file's size and modification time before the first
//! byte is read and compares them once the last byte has been read, before the
//! commit. When they differ the upload is refused and nothing is committed, so
//! the remote keeps its previous content. The transfer engine's single-file
//! multipart path has its own check of the same facts before its commit
//! (`DurableMultipartCheckpoint::verify_against_source`); its batch multipart
//! path, which reads each file's parts at their offsets concurrently, and the
//! uploads a provider runs by itself use this one.
//!
//! Declared limit: a rewrite that keeps both the size and the modification time
//! (a file system with coarse timestamps, or a writer that restores the time)
//! is not detected.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::time::SystemTime;

use super::ProviderError;

/// The phrase every refusal of this kind carries, so a reader of a log or an
/// error can tell "the local file moved under us" from a transport failure.
pub const UPLOAD_SOURCE_CHANGED_MARKER: &str = "changed while it was being uploaded";

/// The size and modification time of a local file, taken before an upload
/// reads it.
#[derive(Debug)]
pub struct UploadSource {
    path: String,
    size: u64,
    modified: Option<SystemTime>,
}

impl UploadSource {
    /// Take the facts of `local_path` before its first byte is read.
    pub async fn observe(local_path: &str) -> Result<Self, ProviderError> {
        let meta = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        Ok(Self {
            path: local_path.to_string(),
            size: meta.len(),
            modified: meta.modified().ok(),
        })
    }

    /// The size the file had when it was observed. An upload either plans
    /// from it or refuses a plan made from a size read earlier, which may
    /// describe another version of the file.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Refuse an upload planned for another size: a file that grew before this
    /// observation would otherwise go up cut short, and pass the later check.
    pub fn ensure_size(&self, planned: u64, scope: &str) -> Result<(), ProviderError> {
        if self.size == planned {
            return Ok(());
        }
        Err(self.changed(scope, &format!("size {planned} became {}", self.size)))
    }

    /// Refuse the commit when the file no longer has the facts it had before
    /// the upload read it. `scope` names the upload in the message.
    pub async fn ensure_unchanged(&self, scope: &str) -> Result<(), ProviderError> {
        let now = match tokio::fs::metadata(&self.path).await {
            Ok(meta) => meta,
            Err(e) => return Err(self.changed(scope, &format!("it can no longer be read: {e}"))),
        };
        if now.len() != self.size {
            return Err(self.changed(scope, &format!("size {} became {}", self.size, now.len())));
        }
        if now.modified().ok() != self.modified {
            return Err(self.changed(scope, "its modification time moved"));
        }
        Ok(())
    }

    /// Read the whole file into memory for an upload that sends it from there,
    /// and refuse it when it changed while it was being read: the bytes are
    /// coherent only if the file kept its facts from before the read to after.
    pub async fn read_whole(local_path: &str, scope: &str) -> Result<Vec<u8>, ProviderError> {
        let source = Self::observe(local_path).await?;
        let data = tokio::fs::read(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        #[cfg(test)]
        tests::run_after_read_hook();
        source.ensure_unchanged(scope).await?;
        Ok(data)
    }

    fn changed(&self, scope: &str, what: &str) -> ProviderError {
        ProviderError::TransferFailed(format!(
            "{scope}: {} {UPLOAD_SOURCE_CHANGED_MARKER} ({what}); nothing was committed",
            self.path
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    thread_local! {
        /// Run by [`UploadSource::read_whole`] between its read and its check,
        /// on the test's own thread (`#[tokio::test]` is single-threaded).
        static AFTER_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn run_after_read_hook() {
        if let Some(hook) = AFTER_READ.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
    }

    fn rewrite_keeping_size(path: &std::path::Path, data: &[u8], modified: SystemTime) {
        std::fs::write(path, data).unwrap();
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(modified).unwrap();
    }

    #[tokio::test]
    async fn an_upload_planned_for_another_size_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"12345").unwrap();
        let source = UploadSource::observe(path.to_str().unwrap()).await.unwrap();
        assert!(source.ensure_size(5, "test").is_ok());
        assert!(source.ensure_size(4, "test").is_err());
    }

    #[tokio::test]
    async fn an_untouched_file_passes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let source = UploadSource::observe(path.to_str().unwrap()).await.unwrap();
        assert_eq!(source.size(), 10);
        source.ensure_unchanged("test upload").await.unwrap();
    }

    #[tokio::test]
    async fn a_size_change_is_refused_and_named() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let source = UploadSource::observe(path.to_str().unwrap()).await.unwrap();
        std::fs::write(&path, b"0123456789AB").unwrap();
        let err = source.ensure_unchanged("test upload").await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(UPLOAD_SOURCE_CHANGED_MARKER), "{msg}");
        assert!(msg.contains("size 10 became 12"), "{msg}");
        assert!(msg.contains("nothing was committed"), "{msg}");
    }

    #[tokio::test]
    async fn a_same_size_rewrite_is_refused_by_its_modification_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        let before = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        rewrite_keeping_size(&path, b"AAAAAAAAAA", before);
        let source = UploadSource::observe(path.to_str().unwrap()).await.unwrap();
        rewrite_keeping_size(&path, b"BBBBBBBBBB", before + Duration::from_secs(1));
        let err = source.ensure_unchanged("test upload").await.unwrap_err();
        assert!(err.to_string().contains("modification time moved"), "{err}");
    }

    #[tokio::test]
    async fn a_whole_read_returns_the_file_when_it_did_not_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let data = UploadSource::read_whole(path.to_str().unwrap(), "test upload")
            .await
            .unwrap();
        assert_eq!(data, b"0123456789");
    }

    #[tokio::test]
    async fn a_whole_read_of_a_file_rewritten_during_the_read_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        let before = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        rewrite_keeping_size(&path, b"AAAAAAAAAA", before);
        let rewrite = path.clone();
        AFTER_READ.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                rewrite_keeping_size(&rewrite, b"BBBBBBBBBB", before + Duration::from_secs(1));
            }));
        });
        let err = UploadSource::read_whole(path.to_str().unwrap(), "test upload")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains(UPLOAD_SOURCE_CHANGED_MARKER),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_removed_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let source = UploadSource::observe(path.to_str().unwrap()).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        let err = source.ensure_unchanged("test upload").await.unwrap_err();
        assert!(err.to_string().contains("can no longer be read"), "{err}");
    }
}
