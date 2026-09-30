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
        let previous = self.0.current_path();
        self.0.change_dir(path).await.map_err(failed)?;
        let listed = self.0.list_files().await.map_err(failed);
        self.0.change_dir(&previous).await.map_err(failed)?;
        Ok(listed?
            .into_iter()
            .map(|file| RemoteEntry {
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
                metadata: Default::default(),
            })
            .collect())
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
        let (parent, name) = parent_and_name(path);
        self.list(parent)
            .await?
            .into_iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        self.0.get_file_size(path).await.map_err(failed)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        self.0.exists(path).await.map_err(failed)
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
