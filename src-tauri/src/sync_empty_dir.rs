// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

//! Remove a folder a sync emptied, only if it is empty.
//!
//! A Mirror deletes a folder the source no longer has after the files under
//! it. The folder can still hold what the plan never removed: a file whose
//! move into the versioned-backup folder failed, and a file the compare
//! excluded (`docs/.env`), which has no row of its own. A recursive delete
//! took both along. This removal never recurses: a folder that lists any
//! entry is kept, and a folder the removal is refused for and that is still
//! there afterwards is kept too.
//!
//! What "empty" rests on differs by backend, and the difference is the limit:
//!
//! - local disk: `remove_dir` refuses a folder that is not empty, always;
//! - FTP and SFTP: `RMD` / `rmdir` refuse it by protocol, so a file the
//!   listing did not show (an FTP `LIST` that hides dot files) still keeps it;
//! - other providers: several remove a folder with its content on `rmdir`,
//!   so the listing taken right before it is the only check, and a stored
//!   object that listing leaves out goes with the folder. `aeroftp-cli sync`
//!   removes no folder at all on those backends
//!   (`sync_rmdir_refuses_non_empty`).

use std::path::Path;

use async_trait::async_trait;

use crate::providers::{ProviderError, StorageProvider};

/// What a removal did to the folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyDirRemoval {
    /// The folder was empty and is gone.
    Removed,
    /// The folder holds something, or refused the removal and is still
    /// there: it stays, with whatever is in it.
    Kept,
}

impl EmptyDirRemoval {
    /// The answer the frontend runner reads.
    pub fn as_str(self) -> &'static str {
        match self {
            EmptyDirRemoval::Removed => "removed",
            EmptyDirRemoval::Kept => "kept",
        }
    }
}

/// Remove the local folder `path` if it is empty. `remove_dir` never
/// recurses; a refusal is `Kept` when the folder is still there and holds an
/// entry, and the error otherwise.
pub fn remove_local_dir_if_empty(path: &Path) -> std::io::Result<EmptyDirRemoval> {
    match std::fs::remove_dir(path) {
        Ok(()) => Ok(EmptyDirRemoval::Removed),
        Err(e) => {
            let holds_entry = path.is_dir()
                && std::fs::read_dir(path)
                    .map(|mut entries| entries.next().is_some())
                    .unwrap_or(false);
            if holds_entry {
                Ok(EmptyDirRemoval::Kept)
            } else {
                Err(e)
            }
        }
    }
}

/// The three operations the removal needs from a remote, so a provider and
/// the GUI's own FTP session run the same code.
#[async_trait]
pub trait EmptyDirRemote: Send {
    /// How many entries a listing of `path` shows.
    async fn entry_count(&mut self, path: &str) -> Result<usize, ProviderError>;
    /// Remove the folder `path`, which the last listing showed empty.
    async fn remove_listed_empty(&mut self, path: &str) -> Result<(), ProviderError>;
    /// Whether a file or folder is at `path`. An error means "could not
    /// tell", never "no".
    async fn path_exists(&mut self, path: &str) -> Result<bool, ProviderError>;
}

/// Remove the remote folder `path` if it is empty: list it, keep it when the
/// listing shows anything, and otherwise remove it. A removal that fails
/// while the folder is still there is `Kept` (the server refused it, most
/// often because it holds what the listing did not show); any other failure
/// is returned.
pub async fn remove_remote_dir_if_empty<R: EmptyDirRemote + ?Sized>(
    remote: &mut R,
    path: &str,
) -> Result<EmptyDirRemoval, ProviderError> {
    if remote.entry_count(path).await? > 0 {
        return Ok(EmptyDirRemoval::Kept);
    }
    match remote.remove_listed_empty(path).await {
        Ok(()) => Ok(EmptyDirRemoval::Removed),
        Err(e) => match remote.path_exists(path).await {
            Ok(true) => Ok(EmptyDirRemoval::Kept),
            _ => Err(e),
        },
    }
}

/// A [`StorageProvider`] seen as a removal target.
pub struct ProviderDirRemote<'a>(pub &'a mut dyn StorageProvider);

#[async_trait]
impl EmptyDirRemote for ProviderDirRemote<'_> {
    async fn entry_count(&mut self, path: &str) -> Result<usize, ProviderError> {
        Ok(self.0.list(path).await?.len())
    }
    async fn remove_listed_empty(&mut self, path: &str) -> Result<(), ProviderError> {
        self.0.rmdir(path).await
    }
    async fn path_exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        self.0.exists(path).await
    }
}

/// The GUI's FTP session: `LIST` needs the working directory, which is put
/// back afterwards; `RMD` refuses a folder that is not empty.
#[async_trait]
impl EmptyDirRemote for crate::ftp::FtpManager {
    async fn entry_count(&mut self, path: &str) -> Result<usize, ProviderError> {
        let original = self.current_path();
        self.change_dir(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        let listed = self.list_files().await;
        // A session left inside the listed folder would run every later GUI
        // operation from there: a failed return is the answer, not the count.
        self.change_dir(&original)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        listed
            .map(|entries| entries.len())
            .map_err(|e| ProviderError::ServerError(e.to_string()))
    }
    async fn remove_listed_empty(&mut self, path: &str) -> Result<(), ProviderError> {
        self.remove_dir(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))
    }
    async fn path_exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        self.exists(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn a_local_folder_with_an_excluded_file_is_kept_with_it() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = tmp.path().join("docs");
        std::fs::create_dir(&docs).unwrap();
        std::fs::write(docs.join(".env"), b"SECRET=1").unwrap();

        assert_eq!(
            remove_local_dir_if_empty(&docs).unwrap(),
            EmptyDirRemoval::Kept
        );
        assert_eq!(std::fs::read(docs.join(".env")).unwrap(), b"SECRET=1");
    }

    #[test]
    fn a_local_folder_holding_only_a_folder_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        std::fs::create_dir_all(a.join("b")).unwrap();

        assert_eq!(
            remove_local_dir_if_empty(&a).unwrap(),
            EmptyDirRemoval::Kept
        );
        assert!(a.join("b").is_dir());
    }

    #[test]
    fn an_empty_local_folder_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty");
        std::fs::create_dir(&empty).unwrap();

        assert_eq!(
            remove_local_dir_if_empty(&empty).unwrap(),
            EmptyDirRemoval::Removed
        );
        assert!(!empty.exists());
    }

    #[test]
    fn a_missing_local_folder_is_an_error_not_a_removal() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(remove_local_dir_if_empty(&tmp.path().join("nope")).is_err());
    }

    /// A remote of folders and files. `hidden` entries exist but are not
    /// listed (an FTP `LIST` without dot files); `recursive_rmdir` removes a
    /// folder with its content, as several cloud APIs do, while otherwise
    /// `rmdir` refuses a folder that holds anything.
    #[derive(Default)]
    struct FakeRemote {
        paths: BTreeSet<String>,
        hidden: BTreeSet<String>,
        recursive_rmdir: bool,
        list_fails: bool,
        rmdir_calls: Vec<String>,
    }

    impl FakeRemote {
        fn with(paths: &[&str]) -> Self {
            Self {
                paths: paths.iter().map(|p| p.to_string()).collect(),
                ..Self::default()
            }
        }
        fn children(&self, path: &str) -> Vec<&String> {
            let prefix = format!("{path}/");
            self.paths
                .iter()
                .filter(|p| p.starts_with(&prefix))
                .collect()
        }
    }

    #[async_trait]
    impl EmptyDirRemote for FakeRemote {
        async fn entry_count(&mut self, path: &str) -> Result<usize, ProviderError> {
            if self.list_fails {
                return Err(ProviderError::ServerError("503".into()));
            }
            Ok(self
                .children(path)
                .into_iter()
                .filter(|p| !self.hidden.contains(*p))
                .count())
        }
        async fn remove_listed_empty(&mut self, path: &str) -> Result<(), ProviderError> {
            self.rmdir_calls.push(path.to_string());
            let children: Vec<String> = self.children(path).into_iter().cloned().collect();
            if !children.is_empty() && !self.recursive_rmdir {
                return Err(ProviderError::ServerError("550 Directory not empty".into()));
            }
            for child in children {
                self.paths.remove(&child);
            }
            self.paths.remove(path);
            Ok(())
        }
        async fn path_exists(&mut self, path: &str) -> Result<bool, ProviderError> {
            Ok(self.paths.contains(path))
        }
    }

    #[tokio::test]
    async fn a_remote_folder_with_an_excluded_file_is_kept_and_never_removed() {
        let mut remote = FakeRemote::with(&["/r/docs", "/r/docs/.env"]);
        remote.recursive_rmdir = true;
        let got = remove_remote_dir_if_empty(&mut remote, "/r/docs")
            .await
            .unwrap();
        assert_eq!(got, EmptyDirRemoval::Kept);
        assert!(remote.rmdir_calls.is_empty());
        assert!(remote.paths.contains("/r/docs/.env"));
    }

    #[tokio::test]
    async fn a_folder_the_listing_shows_empty_but_the_server_refuses_is_kept() {
        let mut remote = FakeRemote::with(&["/r/docs", "/r/docs/.env"]);
        remote.hidden.insert("/r/docs/.env".into());
        let got = remove_remote_dir_if_empty(&mut remote, "/r/docs")
            .await
            .unwrap();
        assert_eq!(got, EmptyDirRemoval::Kept);
        assert_eq!(remote.rmdir_calls, vec!["/r/docs".to_string()]);
        assert!(remote.paths.contains("/r/docs/.env"));
    }

    #[tokio::test]
    async fn an_empty_remote_folder_is_removed() {
        let mut remote = FakeRemote::with(&["/r/empty"]);
        let got = remove_remote_dir_if_empty(&mut remote, "/r/empty")
            .await
            .unwrap();
        assert_eq!(got, EmptyDirRemoval::Removed);
        assert!(!remote.paths.contains("/r/empty"));
    }

    #[tokio::test]
    async fn a_listing_that_fails_removes_nothing() {
        let mut remote = FakeRemote::with(&["/r/docs", "/r/docs/a.txt"]);
        remote.recursive_rmdir = true;
        remote.list_fails = true;
        assert!(remove_remote_dir_if_empty(&mut remote, "/r/docs")
            .await
            .is_err());
        assert!(remote.rmdir_calls.is_empty());
        assert!(remote.paths.contains("/r/docs/a.txt"));
    }

    #[tokio::test]
    async fn a_refused_removal_of_a_folder_that_is_gone_is_the_error() {
        struct Gone;
        #[async_trait]
        impl EmptyDirRemote for Gone {
            async fn entry_count(&mut self, _p: &str) -> Result<usize, ProviderError> {
                Ok(0)
            }
            async fn remove_listed_empty(&mut self, _p: &str) -> Result<(), ProviderError> {
                Err(ProviderError::PermissionDenied("no".into()))
            }
            async fn path_exists(&mut self, _p: &str) -> Result<bool, ProviderError> {
                Ok(false)
            }
        }
        let got = remove_remote_dir_if_empty(&mut Gone, "/r/x").await;
        assert!(matches!(got, Err(ProviderError::PermissionDenied(_))));
    }

    #[test]
    fn the_answers_are_the_words_the_runner_reads() {
        assert_eq!(EmptyDirRemoval::Removed.as_str(), "removed");
        assert_eq!(EmptyDirRemoval::Kept.as_str(), "kept");
    }
}
