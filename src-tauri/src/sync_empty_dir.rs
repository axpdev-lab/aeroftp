// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

//! Remove a folder a sync emptied, only if it is empty.
//!
//! A Mirror deletes a folder the source no longer has after the files under
//! it. The folder can still hold what the plan never removed: a file whose
//! move into the versioned-backup folder failed, and a file the compare
//! excluded (`docs/.env`), which has no row of its own. A recursive delete
//! took both along. This removal never recurses: it is `StorageProvider::rmdir`,
//! which since the 4.2.1 review refuses a folder that holds anything on every
//! backend (`ProviderError::DirectoryNotEmpty`), and the local `remove_dir`,
//! which the operating system refuses the same way.
//!
//! What the answer says, and why:
//!
//! - `removed`: the folder was empty and is gone;
//! - `kept:entries`: the folder holds entries the plan did not have (an
//!   excluded file, a file written since the scan): the listing shows them;
//! - `kept:server`: the server refused the folder as not empty while its
//!   listing shows nothing (an FTP `LIST` that hides dot files); the folder
//!   stays with what it hides;
//! - an error: the removal was refused for any other reason (a permission,
//!   a lost connection) on a folder that lists empty. That is not a skip: the
//!   folder should have gone and did not, and the run must say so.
//!
//! How each backend refuses a folder that is not empty is in the table at
//! [`crate::providers::StorageProvider::rmdir`].

use std::path::Path;

use async_trait::async_trait;

use crate::providers::{ProviderError, StorageProvider};

/// Why a folder stayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeptReason {
    /// The listing shows entries the plan did not have.
    HoldsEntries,
    /// The server refused the folder as not empty; the listing shows nothing.
    RefusedByServer,
}

/// What a removal did to the folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyDirRemoval {
    /// The folder was empty and is gone.
    Removed,
    /// The folder holds something: it stays, with whatever is in it.
    Kept(KeptReason),
}

impl EmptyDirRemoval {
    /// The answer the frontend runner reads.
    pub fn as_str(self) -> &'static str {
        match self {
            EmptyDirRemoval::Removed => "removed",
            EmptyDirRemoval::Kept(KeptReason::HoldsEntries) => "kept:entries",
            EmptyDirRemoval::Kept(KeptReason::RefusedByServer) => "kept:server",
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
                Ok(EmptyDirRemoval::Kept(KeptReason::HoldsEntries))
            } else {
                Err(e)
            }
        }
    }
}

/// The two operations the removal needs from a remote, so a provider and the
/// GUI's own FTP session run the same code.
#[async_trait]
pub trait EmptyDirRemote: Send {
    /// Remove the folder `path` if it is empty, refusing one that is not with
    /// `DirectoryNotEmpty` where the backend tells the two apart (a bare
    /// refusal, as an FTP 550, is any other error).
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError>;
    /// How many entries a listing of `path` shows.
    async fn entry_count(&mut self, path: &str) -> Result<usize, ProviderError>;
}

/// Remove the remote folder `path` if it is empty, through the backend's own
/// non-recursive `rmdir`. A refusal as not empty keeps the folder, with the
/// listing saying whether it shows the entries (`HoldsEntries`) or not
/// (`RefusedByServer`). Any other refusal keeps the folder too when the
/// listing shows entries, since a backend whose refusal is a bare status (FTP
/// `RMD`) says no more than that; on a folder that lists empty it is the
/// error it is.
pub async fn remove_remote_dir_if_empty<R: EmptyDirRemote + ?Sized>(
    remote: &mut R,
    path: &str,
) -> Result<EmptyDirRemoval, ProviderError> {
    match remote.rmdir(path).await {
        Ok(()) => Ok(EmptyDirRemoval::Removed),
        Err(ProviderError::DirectoryNotEmpty(_)) => {
            let reason = if remote.entry_count(path).await? > 0 {
                KeptReason::HoldsEntries
            } else {
                KeptReason::RefusedByServer
            };
            Ok(EmptyDirRemoval::Kept(reason))
        }
        Err(e) => match remote.entry_count(path).await {
            Ok(n) if n > 0 => Ok(EmptyDirRemoval::Kept(KeptReason::HoldsEntries)),
            _ => Err(e),
        },
    }
}

/// A [`StorageProvider`] seen as a removal target.
pub struct ProviderDirRemote<'a>(pub &'a mut dyn StorageProvider);

#[async_trait]
impl EmptyDirRemote for ProviderDirRemote<'_> {
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.0.rmdir(path).await
    }
    async fn entry_count(&mut self, path: &str) -> Result<usize, ProviderError> {
        Ok(self.0.list(path).await?.len())
    }
}

/// Whether an FTP server's refusal names a folder that is not empty. Servers
/// that say so (ProFTPD, Pure-FTPd, IIS) do it in these words; vsftpd answers
/// a bare "Remove directory operation failed.", which says no more than that.
fn ftp_refusal_names_not_empty(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("not empty") || lower.contains("notempty")
}

/// The GUI's FTP session: `LIST` needs the working directory, which is put
/// back afterwards; `RMD` refuses a folder that is not empty.
#[async_trait]
impl EmptyDirRemote for crate::ftp::FtpManager {
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.remove_dir(path).await.map_err(|e| {
            let message = e.to_string();
            if ftp_refusal_names_not_empty(&message) {
                ProviderError::DirectoryNotEmpty(message)
            } else {
                ProviderError::ServerError(message)
            }
        })
    }
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
            EmptyDirRemoval::Kept(KeptReason::HoldsEntries)
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
            EmptyDirRemoval::Kept(KeptReason::HoldsEntries)
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

    /// How a fake server refuses a folder that is not empty.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    enum Refusal {
        /// The typed answer of a provider that lists first, or of an API
        /// with a code for it (Box, pCloud, SFTP).
        #[default]
        Typed,
        /// A bare status, as vsftpd's `550 Remove directory operation failed.`
        Bare,
    }

    /// A remote of folders and files. `hidden` entries exist but are not
    /// listed (an FTP `LIST` without dot files); `rmdir` refuses a folder
    /// that holds anything, listed or hidden, the way every backend does now,
    /// and `refuses_all` a folder the server will not remove at all (a
    /// permission).
    #[derive(Default)]
    struct FakeRemote {
        paths: BTreeSet<String>,
        hidden: BTreeSet<String>,
        refusal: Refusal,
        refuses_all: bool,
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
        async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
            self.rmdir_calls.push(path.to_string());
            if !self.paths.contains(path) {
                return Err(ProviderError::NotFound(path.to_string()));
            }
            if self.refuses_all {
                return Err(ProviderError::PermissionDenied(
                    "550 Permission denied".into(),
                ));
            }
            if !self.children(path).is_empty() {
                return Err(match self.refusal {
                    Refusal::Typed => {
                        ProviderError::DirectoryNotEmpty(format!("{path} holds entries"))
                    }
                    Refusal::Bare => {
                        ProviderError::ServerError("550 Remove directory operation failed.".into())
                    }
                });
            }
            self.paths.remove(path);
            Ok(())
        }
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
    }

    #[tokio::test]
    async fn a_remote_folder_with_an_excluded_file_is_kept_with_it() {
        let mut remote = FakeRemote::with(&["/r/docs", "/r/docs/.env"]);
        let got = remove_remote_dir_if_empty(&mut remote, "/r/docs")
            .await
            .unwrap();
        assert_eq!(got, EmptyDirRemoval::Kept(KeptReason::HoldsEntries));
        assert!(remote.paths.contains("/r/docs/.env"));
    }

    /// The same folder on a server whose refusal is a bare status: the
    /// listing says why the folder stayed.
    #[tokio::test]
    async fn a_bare_refusal_of_a_folder_the_listing_shows_full_is_kept() {
        let mut remote = FakeRemote::with(&["/r/docs", "/r/docs/a.txt"]);
        remote.refusal = Refusal::Bare;
        let got = remove_remote_dir_if_empty(&mut remote, "/r/docs")
            .await
            .unwrap();
        assert_eq!(got, EmptyDirRemoval::Kept(KeptReason::HoldsEntries));
        assert!(remote.paths.contains("/r/docs/a.txt"));
    }

    #[tokio::test]
    async fn a_folder_the_listing_shows_empty_but_the_server_refuses_as_not_empty_is_kept() {
        let mut remote = FakeRemote::with(&["/r/docs", "/r/docs/.env"]);
        remote.hidden.insert("/r/docs/.env".into());
        let got = remove_remote_dir_if_empty(&mut remote, "/r/docs")
            .await
            .unwrap();
        assert_eq!(got, EmptyDirRemoval::Kept(KeptReason::RefusedByServer));
        assert_eq!(remote.rmdir_calls, vec!["/r/docs".to_string()]);
        assert!(remote.paths.contains("/r/docs/.env"));
    }

    /// Round 2 of the 4.2.1 review: a refusal that is not "not empty", on a
    /// folder that lists empty, read as kept, so a permission error on an
    /// empty remote folder showed as skipped. It is the error.
    #[tokio::test]
    async fn a_refusal_on_a_folder_that_lists_empty_is_an_error_not_a_skip() {
        let mut remote = FakeRemote::with(&["/r/empty"]);
        remote.refuses_all = true;
        let got = remove_remote_dir_if_empty(&mut remote, "/r/empty").await;
        assert!(
            matches!(got, Err(ProviderError::PermissionDenied(_))),
            "{got:?}"
        );
        assert!(
            remote.paths.contains("/r/empty"),
            "the folder is still there"
        );

        // A bare refusal of a folder that hides what it holds reads the same
        // way: the run says the folder did not go, instead of a skip nobody
        // reads.
        let mut hidden = FakeRemote::with(&["/r/docs", "/r/docs/.env"]);
        hidden.hidden.insert("/r/docs/.env".into());
        hidden.refusal = Refusal::Bare;
        let got = remove_remote_dir_if_empty(&mut hidden, "/r/docs").await;
        assert!(matches!(got, Err(ProviderError::ServerError(_))), "{got:?}");
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
    async fn a_listing_that_fails_after_a_refusal_is_the_error() {
        let mut remote = FakeRemote::with(&["/r/docs", "/r/docs/a.txt"]);
        remote.list_fails = true;
        assert!(remove_remote_dir_if_empty(&mut remote, "/r/docs")
            .await
            .is_err());
        assert!(remote.paths.contains("/r/docs/a.txt"));
    }

    #[tokio::test]
    async fn a_folder_that_is_gone_is_the_error() {
        let mut remote = FakeRemote::with(&[]);
        let got = remove_remote_dir_if_empty(&mut remote, "/r/x").await;
        assert!(matches!(got, Err(ProviderError::NotFound(_))), "{got:?}");
    }

    #[test]
    fn the_answers_are_the_words_the_runner_reads() {
        assert_eq!(EmptyDirRemoval::Removed.as_str(), "removed");
        assert_eq!(
            EmptyDirRemoval::Kept(KeptReason::HoldsEntries).as_str(),
            "kept:entries"
        );
        assert_eq!(
            EmptyDirRemoval::Kept(KeptReason::RefusedByServer).as_str(),
            "kept:server"
        );
    }

    #[test]
    fn an_ftp_refusal_is_read_for_the_words_not_empty() {
        assert!(ftp_refusal_names_not_empty(
            "550 /docs: Directory not empty"
        ));
        assert!(ftp_refusal_names_not_empty(
            "Operation failed: 550 Directory not empty."
        ));
        assert!(!ftp_refusal_names_not_empty(
            "550 Remove directory operation failed."
        ));
        assert!(!ftp_refusal_names_not_empty("550 Permission denied"));
    }
}
