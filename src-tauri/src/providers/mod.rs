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
pub mod box_provider;
pub mod pcloud;
pub mod azure;
pub mod filen;
pub mod oauth1;
pub mod fourshared;
pub mod zoho_workdrive;
pub mod http_retry;

pub use types::*;
// GAP-A01: retry infrastructure ready — integration into providers deferred to v2.5.0
#[allow(unused_imports)]
pub use http_retry::{HttpRetryConfig, send_with_retry};
pub use ftp::FtpProvider;
pub use sftp::SftpProvider;
pub use webdav::WebDavProvider;
pub use s3::S3Provider;
pub use google_drive::GoogleDriveProvider;
pub use dropbox::DropboxProvider;
pub use onedrive::OneDriveProvider;
pub use mega::MegaProvider;
pub use box_provider::BoxProvider;
pub use pcloud::PCloudProvider;
pub use azure::AzureProvider;
pub use filen::FilenProvider;
pub use fourshared::FourSharedProvider;
pub use zoho_workdrive::ZohoWorkdriveProvider;
pub use oauth2::{OAuth2Manager, OAuthConfig, OAuthProvider};

use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;

/// GAP-A10: Sanitize API error response bodies to prevent leaking sensitive data.
/// Truncates to first line (max 200 chars), strips potential tokens/keys.
pub fn sanitize_api_error(body: &str) -> String {
    let first_line = body.lines().next().unwrap_or("unknown error");
    let truncated = if first_line.len() > 200 {
        let boundary = first_line.char_indices()
            .take_while(|&(i, _)| i <= 200)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(200);
        format!("{}...", &first_line[..boundary])
    } else {
        first_line.to_string()
    };
    // Strip potential Bearer tokens or API keys from error messages
    if truncated.contains("Bearer ") || truncated.contains("eyJ") {
        "API error (response contained credentials — redacted)".to_string()
    } else {
        truncated
    }
}

/// Transfer optimization hints — per-provider capability advertisement
#[derive(Debug, Clone, Serialize)]
pub struct TransferOptimizationHints {
    pub supports_multipart: bool,
    pub multipart_threshold: u64,
    pub multipart_part_size: u64,
    pub multipart_max_parallel: u8,
    pub supports_resume_download: bool,
    pub supports_resume_upload: bool,
    pub supports_server_checksum: bool,
    pub preferred_checksum_algo: Option<String>,
    pub supports_compression: bool,
    pub supports_delta_sync: bool,
}

impl Default for TransferOptimizationHints {
    fn default() -> Self {
        Self {
            supports_multipart: false,
            multipart_threshold: 0,
            multipart_part_size: 0,
            multipart_max_parallel: 1,
            supports_resume_download: false,
            supports_resume_upload: false,
            supports_server_checksum: false,
            preferred_checksum_algo: None,
            supports_compression: false,
            supports_delta_sync: false,
        }
    }
}

/// Unified storage provider trait
///
/// All storage backends must implement this trait to be used with AeroFTP.
/// This enables protocol-agnostic file operations and makes it easy to add
/// new storage providers in the future.
///
/// Note: Some trait methods are not yet used but are part of the planned API
/// for future features (Properties dialog, chmod support, etc.)
#[async_trait]
#[allow(dead_code)]
pub trait StorageProvider: Send + Sync {
    /// Downcast to concrete provider type for provider-specific operations
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;

    /// Get the provider type identifier
    fn provider_type(&self) -> ProviderType;
    
    /// Get display name for this provider instance
    fn display_name(&self) -> String;

    /// Get the authenticated account email/username (if available after connect)
    fn account_email(&self) -> Option<String> { None }

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

    /// Check if provider supports importing from public links
    fn supports_import_link(&self) -> bool {
        false
    }

    /// Import a file/folder from a public link into the account
    async fn import_link(&mut self, _link: &str, _dest: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("import_link".to_string()))
    }

    /// Remove a previously created share/export link
    async fn remove_share_link(&mut self, _path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("remove_share_link".to_string()))
    }

    /// Get storage quota information (used/total/free)
    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        Err(ProviderError::NotSupported("storage_info".to_string()))
    }

    /// Get disk usage for a specific path in bytes
    async fn disk_usage(&mut self, _path: &str) -> Result<u64, ProviderError> {
        Err(ProviderError::NotSupported("disk_usage".to_string()))
    }

    /// Check if provider supports remote search
    fn supports_find(&self) -> bool {
        false
    }

    /// Search for files matching a pattern under the given path
    async fn find(&mut self, _path: &str, _pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        Err(ProviderError::NotSupported("find".to_string()))
    }

    /// Set transfer speed limits (in KB/s, 0 = unlimited)
    async fn set_speed_limit(&mut self, _upload_kb: u64, _download_kb: u64) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("set_speed_limit".to_string()))
    }

    /// Get current transfer speed limits (upload_kb, download_kb) in KB/s
    async fn get_speed_limit(&mut self) -> Result<(u64, u64), ProviderError> {
        Err(ProviderError::NotSupported("get_speed_limit".to_string()))
    }

    /// Check if provider supports resume (REST command)
    fn supports_resume(&self) -> bool {
        false
    }

    /// Resume a download from a given byte offset
    async fn resume_download(
        &mut self,
        _remote_path: &str,
        _local_path: &str,
        _offset: u64,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("resume_download".to_string()))
    }

    /// Resume an upload from a given byte offset
    async fn resume_upload(
        &mut self,
        _local_path: &str,
        _remote_path: &str,
        _offset: u64,
        _on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("resume_upload".to_string()))
    }

    /// Check if provider supports file versions
    fn supports_versions(&self) -> bool {
        false
    }

    /// List versions of a file
    async fn list_versions(&mut self, _path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        Err(ProviderError::NotSupported("list_versions".to_string()))
    }

    /// Download a specific version of a file
    async fn download_version(
        &mut self,
        _path: &str,
        _version_id: &str,
        _local_path: &str,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("download_version".to_string()))
    }

    /// Restore a file to a specific version
    async fn restore_version(&mut self, _path: &str, _version_id: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("restore_version".to_string()))
    }

    /// Check if provider supports file locking
    fn supports_locking(&self) -> bool {
        false
    }

    /// Lock a file
    async fn lock_file(&mut self, _path: &str, _timeout: u64) -> Result<LockInfo, ProviderError> {
        Err(ProviderError::NotSupported("lock_file".to_string()))
    }

    /// Unlock a file
    async fn unlock_file(&mut self, _path: &str, _lock_token: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("unlock_file".to_string()))
    }

    /// Check if provider supports thumbnails
    fn supports_thumbnails(&self) -> bool {
        false
    }

    /// Get a thumbnail URL or base64-encoded data for a file
    async fn get_thumbnail(&mut self, _path: &str) -> Result<String, ProviderError> {
        Err(ProviderError::NotSupported("get_thumbnail".to_string()))
    }

    /// Check if provider supports advanced sharing (per-user permissions)
    fn supports_permissions(&self) -> bool {
        false
    }

    /// List current permissions/shares on a file
    async fn list_permissions(&mut self, _path: &str) -> Result<Vec<SharePermission>, ProviderError> {
        Err(ProviderError::NotSupported("list_permissions".to_string()))
    }

    /// Add a permission/share to a file
    async fn add_permission(
        &mut self,
        _path: &str,
        _permission: &SharePermission,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("add_permission".to_string()))
    }

    /// Remove a permission/share from a file
    async fn remove_permission(
        &mut self,
        _path: &str,
        _target: &str,
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("remove_permission".to_string()))
    }

    /// Whether this provider supports file checksums
    fn supports_checksum(&self) -> bool {
        false
    }

    /// Get checksum(s) for a file. Returns HashMap with algorithm → hex digest.
    async fn checksum(&mut self, _path: &str) -> Result<HashMap<String, String>, ProviderError> {
        Err(ProviderError::NotSupported("checksum".to_string()))
    }

    /// Whether this provider supports remote/URL upload (server fetches a URL)
    fn supports_remote_upload(&self) -> bool {
        false
    }

    /// Tell the server to download a file from a URL into the given path
    async fn remote_upload(&mut self, _url: &str, _dest_path: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotSupported("remote_upload".to_string()))
    }

    /// Whether this provider supports change tracking (delta sync)
    fn supports_change_tracking(&self) -> bool {
        false
    }

    /// Get a start page token for change tracking
    async fn get_change_token(&mut self) -> Result<String, ProviderError> {
        Err(ProviderError::NotSupported("get_change_token".to_string()))
    }

    /// List changes since the given page token, returns (changes, new_token)
    async fn list_changes(&mut self, _page_token: &str) -> Result<(Vec<ChangeEntry>, String), ProviderError> {
        Err(ProviderError::NotSupported("list_changes".to_string()))
    }

    /// Get transfer optimization hints for this provider
    fn transfer_optimization_hints(&self) -> TransferOptimizationHints {
        TransferOptimizationHints::default()
    }

    /// Whether this provider supports delta sync (rsync-style block transfer)
    fn supports_delta_sync(&self) -> bool {
        false
    }

    /// Read a byte range from a remote file (needed for delta sync)
    async fn read_range(&mut self, _path: &str, _offset: u64, _len: u64) -> Result<Vec<u8>, ProviderError> {
        Err(ProviderError::NotSupported("read_range".to_string()))
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
                Ok(Box::new(WebDavProvider::new(webdav_config)?))
            }
            ProviderType::S3 => {
                let s3_config = S3Config::from_provider_config(config)?;
                Ok(Box::new(S3Provider::new(s3_config)?))
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
            ProviderType::GoogleDrive | ProviderType::Dropbox | ProviderType::OneDrive
            | ProviderType::Box | ProviderType::PCloud | ProviderType::ZohoWorkdrive => {
                // OAuth2 providers require a different initialization flow
                // Use oauth2_connect command instead
                Err(ProviderError::NotSupported(
                    "OAuth2 providers must be connected using oauth2_start_auth and oauth2_connect commands".to_string()
                ))
            }
            ProviderType::FourShared => {
                // OAuth1 provider — use fourshared_connect command
                Err(ProviderError::NotSupported(
                    "4shared must be connected using fourshared_start_auth and fourshared_connect commands".to_string()
                ))
            }
            ProviderType::Mega => {
                let mega_config = MegaConfig::from_provider_config(config)?;
                Ok(Box::new(MegaProvider::new(mega_config)))
            }
            ProviderType::Azure => {
                let azure_config = AzureConfig::from_provider_config(config)?;
                Ok(Box::new(AzureProvider::new(azure_config)))
            }
            ProviderType::Filen => {
                let filen_config = FilenConfig::from_provider_config(config)?;
                Ok(Box::new(FilenProvider::new(filen_config)))
            }
        }
    }
    
    /// Get list of all supported provider types
    #[allow(dead_code)]
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
            ProviderType::Box,
            ProviderType::PCloud,
            ProviderType::Azure,
            ProviderType::Filen,
            ProviderType::FourShared,
            ProviderType::ZohoWorkdrive,
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
