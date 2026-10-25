// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! A [`StorageProvider`] view of the legacy FTP session ([`FtpManager`]),
//! the one the GUI opens with `connect_ftp` when no provider is active.
//!
//! It exists so that AeroAgent's `remote_edit` publishes on that session
//! through the same [`publish_remote_edit`](super::gui_tools) as on a
//! provider: a temporary beside the file, the mode copied, a symbolic link
//! refused, then `RNFR`/`RNTO`. Until 4.2.1 that path stored the new text
//! over the file in place. Only what the manager can do is delegated; the
//! rest answers `NotSupported`.

use crate::ftp::FtpManager;
use crate::providers::{ProviderError, ProviderType, RemoteEntry, StorageProvider};
use async_trait::async_trait;

pub(crate) struct FtpManagerProvider(pub(crate) FtpManager);

/// Restore the GUI's session even when a borrowed operation is cancelled.
pub(crate) struct FtpManagerLease<'a> {
    slot: &'a mut FtpManager,
    pub(crate) provider: FtpManagerProvider,
}

impl Drop for FtpManagerLease<'_> {
    fn drop(&mut self) {
        std::mem::swap(self.slot, &mut self.provider.0);
    }
}

fn failed(error: anyhow::Error) -> ProviderError {
    ProviderError::ServerError(error.to_string())
}

/// The folder of `path` and its last component (`/a/b.txt` -> `/a`, `b.txt`).
fn parent_and_name(path: &str) -> (&str, &str) {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some((parent, name)) => (parent, name),
        None => (".", trimmed),
    }
}

impl FtpManagerProvider {
    pub(crate) fn borrow(manager: &mut FtpManager) -> FtpManagerLease<'_> {
        let session = std::mem::take(manager);
        FtpManagerLease {
            slot: manager,
            provider: Self(session),
        }
    }

    async fn list_probe(
        &mut self,
        path: &str,
        include_hidden: bool,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        let previous = self.0.current_path();
        self.0.change_dir(path).await.map_err(failed)?;
        let listed = self
            .0
            .list_files_for_probe(include_hidden)
            .await
            .map_err(failed);
        self.0.change_dir(&previous).await.map_err(failed)?;
        Ok(listed?
            .into_iter()
            .map(|file| {
                let mut metadata = std::collections::HashMap::new();
                if file.size.is_none() {
                    metadata.insert(
                        crate::providers::ftp_listing::SIZE_UNREADABLE.to_string(),
                        "1".to_string(),
                    );
                }
                RemoteEntry {
                    name: file.name,
                    path: file.path,
                    is_dir: file.is_dir,
                    size: file.size.unwrap_or(0),
                    modified: file.modified,
                    permissions: file.permissions,
                    owner: None,
                    group: None,
                    is_symlink: file.is_symlink,
                    link_target: file.link_target,
                    mime_type: None,
                    metadata,
                }
            })
            .collect())
    }
}

#[async_trait]
impl StorageProvider for FtpManagerProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Ftp
    }

    fn display_name(&self) -> String {
        "FTP".to_string()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        if self.0.is_connected() {
            Ok(())
        } else {
            Err(ProviderError::NotConnected)
        }
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.0.disconnect().await.map_err(failed)
    }

    fn is_connected(&self) -> bool {
        self.0.is_connected()
    }

    /// The manager lists only its current folder: it goes there, lists, and
    /// comes back to the folder it was in, whatever the listing answered.
    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        match self.list_probe(path, false).await {
            Ok(entries) => Ok(entries),
            Err(error) => {
                // stat uses list_probe directly, so this does not recurse
                // into list. Only an independent exact parent lookup can
                // turn an ambiguous refused CWD into a missing directory.
                match self.stat(path).await {
                    Err(ProviderError::NotFound(_)) => Err(ProviderError::NotFound(path.into())),
                    _ => Err(error),
                }
            }
        }
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.0.current_path())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        self.0.change_dir(path).await.map_err(failed)
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.0.go_up().await.map_err(failed)
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.0
            .download_file(remote_path, local_path)
            .await
            .map_err(failed)
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        self.0.download_to_bytes(remote_path).await.map_err(failed)
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.0
            .upload_file(local_path, remote_path)
            .await
            .map_err(failed)
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.0.mkdir(path).await.map_err(failed)
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        self.0.remove(path).await.map_err(failed)
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.0.remove_dir(path).await.map_err(failed)
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported(format!(
            "recursive delete of {path} on the legacy FTP session"
        )))
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.0.rename(from, to).await.map_err(failed)
    }

    /// RNFR/RNTO, whose answer to an occupied destination is the server's
    /// (see [`FtpManager::replace`]): the Unix-like servers replace in one
    /// `rename(2)`, the Windows ones refuse. That is why the inherited
    /// `supports_atomic_replace` of `true` stands — "no known obstacle", not
    /// "verified": no FTP answer (FEAT, SYST, the banner) names the semantics
    /// ahead of time, and on a refusing server the publish fails after the
    /// temporary is staged, with the temporary deleted and the original
    /// untouched. Answering `false` would refuse every edit on this session
    /// and send the agent back to the in-place write this path replaced.
    async fn replace(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        self.0.replace(from, to).await.map_err(failed)
    }

    /// The entry of `path` in the listing of its folder: the manager has no
    /// MLST, and the listing carries the mode and the link the edit needs.
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if path == "/" || path == "." {
            let previous = self.0.current_path();
            self.0.change_dir(path).await.map_err(failed)?;
            self.0.change_dir(&previous).await.map_err(failed)?;
            return Ok(RemoteEntry::directory(path.to_string(), path.to_string()));
        }
        let (parent, name) = parent_and_name(path);
        let entries = match self.list_probe(parent, false).await {
            Ok(entries) => entries,
            Err(error) if parent != "/" && parent != "." => {
                return match Box::pin(self.stat(parent)).await {
                    Err(ProviderError::NotFound(_)) => {
                        Err(ProviderError::NotFound(path.to_string()))
                    }
                    _ => Err(error),
                };
            }
            Err(error) => return Err(error),
        };
        if let Some(entry) = entries.into_iter().find(|entry| entry.name == name) {
            return Ok(entry);
        }
        if name.starts_with('.') {
            if let Some(entry) = self
                .list_probe(parent, true)
                .await?
                .into_iter()
                .find(|entry| entry.name == name)
            {
                return Ok(entry);
            }
        }
        Err(ProviderError::NotFound(path.to_string()))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        if crate::providers::ftp_listing::size_is_unreadable(&entry) {
            self.0.get_file_size(path).await.map_err(failed)
        } else {
            Ok(entry.size)
        }
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        self.0.noop().await.map_err(failed)
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        self.0.server_info().await.map_err(failed)
    }

    fn supports_chmod(&self) -> bool {
        true
    }

    async fn chmod(&mut self, path: &str, mode: u32) -> Result<(), ProviderError> {
        self.0
            .chmod(path, &format!("{mode:o}"))
            .await
            .map_err(failed)
    }
}

#[cfg(test)]
mod tests {
    use super::parent_and_name;

    #[test]
    fn a_path_splits_into_its_folder_and_its_name() {
        assert_eq!(parent_and_name("/t.txt"), ("/", "t.txt"));
        assert_eq!(parent_and_name("/a/b.txt"), ("/a", "b.txt"));
        assert_eq!(parent_and_name("b.txt"), (".", "b.txt"));
    }
}

#[cfg(test)]
mod probe_contract_tests {
    use super::*;

    #[tokio::test]
    async fn audit425_legacy_ftp_preserves_unknown_size_and_uses_size_command() {
        let addr = crate::ftp::exists_tests::vsftpd_like_with_unknown_size(true).await;
        let mut manager = FtpManager::new();
        manager.connect(&addr).await.unwrap();
        manager.login("u", "p").await.unwrap();
        let mut provider = FtpManagerProvider(manager);
        let entry = provider.stat("/a.txt").await.unwrap();
        assert!(
            crate::providers::ftp_listing::size_is_unreadable(&entry),
            "unknown LIST size must not become an exact zero"
        );
        assert_eq!(provider.size("/a.txt").await.unwrap(), 5);
    }

    #[tokio::test]
    async fn audit425_legacy_ftp_unknown_size_and_refused_size_is_an_error() {
        let addr = crate::ftp::exists_tests::vsftpd_like_script(true, true, false).await;
        let mut manager = FtpManager::new();
        manager.connect(&addr).await.unwrap();
        manager.login("u", "p").await.unwrap();
        assert!(FtpManagerProvider(manager).size("/a.txt").await.is_err());
    }

    #[tokio::test]
    async fn probe_contract_ftp_manager_adapter_finds_hidden_entries() {
        let addr = crate::ftp::exists_tests::vsftpd_like().await;
        let mut manager = FtpManager::new();
        manager.connect(&addr).await.unwrap();
        manager.login("u", "p").await.unwrap();
        let mut provider = FtpManagerProvider(manager);
        let outcome = provider.stat("/.hidden").await;
        assert!(
            matches!(outcome, Ok(ref entry) if entry.name == ".hidden"),
            "{outcome:?}"
        );
    }
}
