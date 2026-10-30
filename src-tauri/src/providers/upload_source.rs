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
//! the remote keeps its previous content. The native multipart path of the
//! transfer engine has its own check of the same facts before its commit
//! (`DurableMultipartCheckpoint::verify_against_source`); this one covers the
//! uploads a provider runs by itself.
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

    /// The size the file had when it was observed.
    pub fn size(&self) -> u64 {
        self.size
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

    fn rewrite_keeping_size(path: &std::path::Path, data: &[u8], modified: SystemTime) {
        std::fs::write(path, data).unwrap();
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(modified).unwrap();
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
