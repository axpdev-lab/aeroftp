//! SFTP Provider Implementation
//!
//! This module provides SFTP (SSH File Transfer Protocol) support.
//! Supports both password and key-based authentication.
//!
//! Status: In Development (v1.3.0)

use super::{ProviderError, ProviderType, RemoteEntry, SftpConfig, StorageProvider};
use async_trait::async_trait;
use std::path::Path;

/// SFTP Provider
///
/// Note: Full implementation will use russh crate. Currently a stub for compilation.
pub struct SftpProvider {
    config: SftpConfig,
    connected: bool,
    current_dir: String,
}

impl SftpProvider {
    pub fn new(config: SftpConfig) -> Self {
        Self {
            config,
            connected: false,
            current_dir: "/".to_string(),
        }
    }

    /// Normalize path (ensure absolute)
    fn normalize_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            path.to_string()
        } else if path.is_empty() || path == "." {
            self.current_dir.clone()
        } else if path == ".." {
            let parent = Path::new(&self.current_dir)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "/".to_string());
            if parent.is_empty() { "/".to_string() } else { parent }
        } else {
            format!("{}/{}", self.current_dir.trim_end_matches('/'), path)
        }
    }
}

#[async_trait]
impl StorageProvider for SftpProvider {
    fn provider_type(&self) -> ProviderType {
        ProviderType::Sftp
    }

    fn display_name(&self) -> String {
        format!("{}@{}", self.config.username, self.config.host)
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // TODO: Implement full SFTP connection using russh
        // For now, return NotSupported to indicate work in progress
        tracing::info!("SFTP connect requested for {}:{}", self.config.host, self.config.port);

        Err(ProviderError::NotSupported(
            "SFTP support is coming in v1.3.0. Stay tuned!".to_string()
        ))
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, _path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        Ok(vec![])
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_dir.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        self.current_dir = self.normalize_path(path);
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn download(
        &mut self,
        _remote_path: &str,
        _local_path: &str,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }

    async fn download_to_bytes(&mut self, _remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        Err(ProviderError::NotConnected)
    }

    async fn upload(
        &mut self,
        _local_path: &str,
        _remote_path: &str,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }

    async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }

    async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }

    async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }

    async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }

    async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let full_path = self.normalize_path(path);
        let name = Path::new(&full_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| full_path.clone());

        Ok(RemoteEntry {
            name,
            path: full_path,
            is_dir: false,
            size: 0,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        })
    }

    async fn size(&mut self, _path: &str) -> Result<u64, ProviderError> {
        Err(ProviderError::NotConnected)
    }

    async fn exists(&mut self, _path: &str) -> Result<bool, ProviderError> {
        Err(ProviderError::NotConnected)
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "SFTP Server: {}:{} (user: {})",
            self.config.host, self.config.port, self.config.username
        ))
    }

    fn supports_chmod(&self) -> bool {
        true // SFTP supports chmod
    }

    async fn chmod(&mut self, _path: &str, _mode: u32) -> Result<(), ProviderError> {
        Err(ProviderError::NotConnected)
    }

    fn supports_symlinks(&self) -> bool {
        true // SFTP supports symlinks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sftp_provider_creation() {
        let config = SftpConfig {
            host: "example.com".to_string(),
            port: 22,
            username: "testuser".to_string(),
            password: Some("testpass".to_string()),
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
        };

        let provider = SftpProvider::new(config);
        assert_eq!(provider.provider_type(), ProviderType::Sftp);
        assert!(!provider.is_connected());
    }

    #[test]
    fn test_normalize_path() {
        let config = SftpConfig {
            host: "example.com".to_string(),
            port: 22,
            username: "testuser".to_string(),
            password: None,
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
        };

        let mut provider = SftpProvider::new(config);
        provider.current_dir = "/home/user".to_string();

        assert_eq!(provider.normalize_path("/absolute"), "/absolute");
        assert_eq!(provider.normalize_path("relative"), "/home/user/relative");
        assert_eq!(provider.normalize_path(".."), "/home");
        assert_eq!(provider.normalize_path("."), "/home/user");
    }
}
