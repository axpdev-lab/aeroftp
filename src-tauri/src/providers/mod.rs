//! Storage Providers Module
//! 
//! This module provides a unified abstraction layer for different storage backends.
//! All providers implement the `StorageProvider` trait, allowing the application
//! to work with FTP, WebDAV, S3, and other storage systems through a common interface.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │              StorageProvider Trait                       │
//! │    connect, list, upload, download, mkdir, etc.          │
//! └─────────────────────────────────────────────────────────┘
//!                           │
//!    ┌──────┬───────┬───────┼───────┬────────┬────────┐
//!    ▼      ▼       ▼       ▼       ▼        ▼        ▼
//! ┌─────┐┌──────┐┌──────┐┌─────┐┌────────┐┌────────┐┌──────┐
//! │ FTP ││ SFTP ││WebDAV││ S3  ││ GDrive ││Dropbox ││ MEGA │
//! └─────┘└──────┘└──────┘└─────┘└────────┘└────────┘└──────┘
//! ```

pub mod types;
pub mod ftp;
pub mod sftp;
pub mod webdav;
pub mod s3;
pub mod oauth2;
pub mod google_drive;
pub mod dropbox;
pub mod onedrive;
pub mod mega;

pub use types::*;
pub use ftp::FtpProvider;
pub use sftp::SftpProvider;
pub use webdav::WebDavProvider;
pub use s3::S3Provider;
pub use google_drive::GoogleDriveProvider;
pub use dropbox::DropboxProvider;
pub use onedrive::OneDriveProvider;
pub use mega::MegaProvider;
pub use oauth2::{OAuth2Manager, OAuthConfig, OAuthProvider};

use async_trait::async_trait;

/// Unified storage provider trait
/// 
/// All storage backends must implement this trait to be used with AeroFTP.
/// This enables protocol-agnostic file operations and makes it easy to add
/// new storage providers in the future.
#[async_trait]
pub trait StorageProvider: Send + Sync {
    /// Get the provider type identifier
    fn provider_type(&self) -> ProviderType;
    
    /// Get display name for this provider instance
    fn display_name(&self) -> String;
    
    /// Connect to the storage backend
    async fn connect(&mut self) -> Result<(), ProviderError>;
    
    /// Disconnect from the storage backend
    async fn disconnect(&mut self) -> Result<(), ProviderError>;
    
    /// Check if currently connected
    fn is_connected(&self) -> bool;
    
    /// List files and directories in the given path
    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError>;
    
    /// Get current working directory
    async fn pwd(&mut self) -> Result<String, ProviderError>;
    
    /// Change current directory
    async fn cd(&mut self, path: &str) -> Result<(), ProviderError>;
    
    /// Go to parent directory
    async fn cd_up(&mut self) -> Result<(), ProviderError>;
    
    /// Download a file to local path
    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError>;
    
    /// Download a file to memory (returns bytes)
    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError>;
    
    /// Upload a file from local path
    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError>;
    
    /// Create a directory
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError>;
    
    /// Delete a file
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError>;
    
    /// Delete a directory (must be empty for most providers)
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError>;
    
    /// Delete a directory recursively (with all contents)
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError>;
    
    /// Rename/move a file or directory
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError>;
    
    /// Get file/directory info
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError>;
    
    /// Get file size
    async fn size(&mut self, path: &str) -> Result<u64, ProviderError>;
    
    /// Check if path exists
    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError>;
    
    /// Keep connection alive (send heartbeat/noop)
    async fn keep_alive(&mut self) -> Result<(), ProviderError>;
    
    /// Get server/service info
    async fn server_info(&mut self) -> Result<String, ProviderError>;
    
    // Optional capabilities - providers can override these
    
    /// Check if provider supports chmod
    fn supports_chmod(&self) -> bool {
        false
    }
    
    /// Change file permissions (Unix-style)
    async fn chmod(&mut self, _path: &str, _mode: u32) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("chmod".to_string()))
    }
    
    /// Check if provider supports symlinks
    fn supports_symlinks(&self) -> bool {
        false
    }
    
    /// Check if provider supports server-side copy
    fn supports_server_copy(&self) -> bool {
        false
    }
    
    /// Copy file on server side (without download/upload)
    async fn server_copy(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("server_copy".to_string()))
    }
    
    /// Check if provider supports share links
    fn supports_share_links(&self) -> bool {
        false
    }
    
    /// Generate a share link for a file
    async fn create_share_link(
        &mut self,
        _path: &str,
        _expires_in_secs: Option<u64>,
    ) -> Result<String, ProviderError> {
        Err(ProviderError::NotSupported("share_link".to_string()))
    }
}

/// Provider factory for creating provider instances
pub struct ProviderFactory;

impl ProviderFactory {
    /// Create a new provider instance based on configuration
    pub fn create(config: &ProviderConfig) -> Result<Box<dyn StorageProvider>, ProviderError> {
        match config.provider_type {
            ProviderType::Ftp | ProviderType::Ftps => {
                let ftp_config = FtpConfig::from_provider_config(config)?;
                Ok(Box::new(FtpProvider::new(ftp_config)))
            }
            ProviderType::WebDav => {
                let webdav_config = WebDavConfig::from_provider_config(config)?;
                Ok(Box::new(WebDavProvider::new(webdav_config)))
            }
            ProviderType::S3 => {
                let s3_config = S3Config::from_provider_config(config)?;
                Ok(Box::new(S3Provider::new(s3_config)))
            }
            ProviderType::Sftp => {
                let sftp_config = SftpConfig::from_provider_config(config)?;
                Ok(Box::new(SftpProvider::new(sftp_config)))
            }
            ProviderType::AeroCloud => {
                // AeroCloud uses FTP internally but is configured via CloudPanel
                Err(ProviderError::NotSupported(
                    "AeroCloud must be configured via the AeroCloud panel (click AeroCloud in status bar)".to_string()
                ))
            }
            ProviderType::GoogleDrive | ProviderType::Dropbox | ProviderType::OneDrive => {
                // OAuth2 providers require a different initialization flow
                // Use oauth2_connect command instead
                Err(ProviderError::NotSupported(
                    "OAuth2 providers must be connected using oauth2_start_auth and oauth2_connect commands".to_string()
                ))
            }
            ProviderType::Mega => {
                let mega_config = MegaConfig::from_provider_config(config)?;
                Ok(Box::new(MegaProvider::new(mega_config)))
            }
        }
    }
    
    /// Get list of all supported provider types
    pub fn supported_types() -> Vec<ProviderType> {
        vec![
            ProviderType::Ftp,
            ProviderType::Ftps,
            ProviderType::Sftp,
            ProviderType::WebDav,
            ProviderType::S3,
            ProviderType::AeroCloud,
            ProviderType::GoogleDrive,
            ProviderType::Dropbox,
            ProviderType::OneDrive,
            ProviderType::Mega,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_provider_factory_supported_types() {
        let types = ProviderFactory::supported_types();
        assert!(types.contains(&ProviderType::Ftp));
        assert!(types.contains(&ProviderType::WebDav));
        assert!(types.contains(&ProviderType::S3));
    }
}
