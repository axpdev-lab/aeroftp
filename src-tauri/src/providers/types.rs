//! Shared types for storage providers
//!
//! This module contains all shared types used across different storage providers,
//! including configuration structs, file entry representations, and error types.

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// Supported storage provider types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    /// Standard FTP (File Transfer Protocol)
    Ftp,
    /// FTP over TLS/SSL
    Ftps,
    /// SSH File Transfer Protocol
    Sftp,
    /// WebDAV (Web Distributed Authoring and Versioning)
    WebDav,
    /// Amazon S3 and S3-compatible storage
    S3,
    /// AeroCloud - Personal FTP-based cloud sync
    AeroCloud,
    /// Google Drive (OAuth2)
    GoogleDrive,
    /// Dropbox (OAuth2)
    Dropbox,
    /// Microsoft OneDrive (OAuth2)
    OneDrive,
}

impl fmt::Display for ProviderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderType::Ftp => write!(f, "FTP"),
            ProviderType::Ftps => write!(f, "FTPS"),
            ProviderType::Sftp => write!(f, "SFTP"),
            ProviderType::WebDav => write!(f, "WebDAV"),
            ProviderType::S3 => write!(f, "S3"),
            ProviderType::AeroCloud => write!(f, "AeroCloud"),
            ProviderType::GoogleDrive => write!(f, "Google Drive"),
            ProviderType::Dropbox => write!(f, "Dropbox"),
            ProviderType::OneDrive => write!(f, "OneDrive"),
        }
    }
}

impl ProviderType {
    /// Get default port for this provider type
    pub fn default_port(&self) -> u16 {
        match self {
            ProviderType::Ftp => 21,
            ProviderType::Ftps => 990,
            ProviderType::Sftp => 22,
            ProviderType::WebDav => 443,
            ProviderType::S3 => 443,
            ProviderType::AeroCloud => 21, // Uses FTP in background
            ProviderType::GoogleDrive => 443,
            ProviderType::Dropbox => 443,
            ProviderType::OneDrive => 443,
        }
    }
    
    /// Check if this provider uses encryption by default
    pub fn uses_encryption(&self) -> bool {
        matches!(self, 
            ProviderType::Ftps | 
            ProviderType::Sftp | 
            ProviderType::WebDav | 
            ProviderType::S3 |
            ProviderType::AeroCloud |  // AeroCloud recommends FTPS
            ProviderType::GoogleDrive |
            ProviderType::Dropbox |
            ProviderType::OneDrive
        )
    }
    
    /// Check if this provider requires OAuth2 authentication
    pub fn requires_oauth2(&self) -> bool {
        matches!(self, 
            ProviderType::GoogleDrive |
            ProviderType::Dropbox |
            ProviderType::OneDrive
        )
    }
    
    /// Check if this is an AeroCloud provider (uses FTP backend with sync)
    pub fn is_aerocloud(&self) -> bool {
        matches!(self, ProviderType::AeroCloud)
    }
}

/// Generic provider configuration
/// 
/// This struct can be used to configure any provider type.
/// Provider-specific fields are stored in the `extra` HashMap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Display name for this connection
    pub name: String,
    /// Provider type
    pub provider_type: ProviderType,
    /// Host/endpoint URL
    pub host: String,
    /// Port number (uses default if None)
    pub port: Option<u16>,
    /// Username for authentication
    pub username: Option<String>,
    /// Password for authentication
    pub password: Option<String>,
    /// Initial path to navigate to after connection
    pub initial_path: Option<String>,
    /// Extra provider-specific options
    #[serde(default)]
    pub extra: std::collections::HashMap<String, String>,
}

impl ProviderConfig {
    /// Get the effective port (default or specified)
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or_else(|| self.provider_type.default_port())
    }
}

/// FTP-specific configuration
#[derive(Debug, Clone)]
pub struct FtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub use_tls: bool,
    pub initial_path: Option<String>,
}

impl FtpConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        Ok(Self {
            host: config.host.clone(),
            port: config.effective_port(),
            username: config.username.clone().unwrap_or_else(|| "anonymous".to_string()),
            password: config.password.clone().unwrap_or_default(),
            use_tls: config.provider_type == ProviderType::Ftps,
            initial_path: config.initial_path.clone(),
        })
    }
}

/// WebDAV-specific configuration
#[derive(Debug, Clone)]
pub struct WebDavConfig {
    /// Full URL to WebDAV endpoint (e.g., https://cloud.example.com/remote.php/dav/files/user/)
    pub url: String,
    pub username: String,
    pub password: String,
    pub initial_path: Option<String>,
}

impl WebDavConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        // Build WebDAV URL from host
        let scheme = if config.effective_port() == 80 { "http" } else { "https" };
        let port_suffix = if config.effective_port() == 443 || config.effective_port() == 80 {
            String::new()
        } else {
            format!(":{}", config.effective_port())
        };
        
        let url = if config.host.starts_with("http://") || config.host.starts_with("https://") {
            config.host.clone()
        } else {
            format!("{}://{}{}", scheme, config.host, port_suffix)
        };
        
        Ok(Self {
            url,
            username: config.username.clone().unwrap_or_default(),
            password: config.password.clone().unwrap_or_default(),
            initial_path: config.initial_path.clone(),
        })
    }
}

/// S3-specific configuration
#[derive(Debug, Clone)]
pub struct S3Config {
    /// S3-compatible endpoint URL (empty for AWS S3)
    pub endpoint: Option<String>,
    /// AWS region (e.g., us-east-1)
    pub region: String,
    /// Access key ID
    pub access_key_id: String,
    /// Secret access key
    pub secret_access_key: String,
    /// Bucket name
    pub bucket: String,
    /// Path prefix within bucket
    pub prefix: Option<String>,
    /// Use path-style addressing (for MinIO, etc.)
    pub path_style: bool,
}

impl S3Config {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let bucket = config.extra.get("bucket")
            .ok_or_else(|| ProviderError::InvalidConfig("S3 bucket name is required".to_string()))?
            .clone();
        
        let region = config.extra.get("region")
            .cloned()
            .unwrap_or_else(|| "us-east-1".to_string());
        
        let endpoint = if config.host.is_empty() || config.host == "s3.amazonaws.com" {
            None
        } else {
            // Ensure endpoint has scheme
            let host = config.host.trim();
            if host.starts_with("http://") || host.starts_with("https://") {
                Some(host.to_string())
            } else {
                Some(format!("https://{}", host))
            }
        };
        
        let path_style = config.extra.get("path_style")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(endpoint.is_some()); // Default to path style for custom endpoints
        
        Ok(Self {
            endpoint,
            region,
            access_key_id: config.username.clone().unwrap_or_default(),
            secret_access_key: config.password.clone().unwrap_or_default(),
            bucket,
            prefix: config.initial_path.clone(),
            path_style,
        })
    }
}

/// Remote file/directory entry
/// 
/// Unified representation of a file or directory across all providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteEntry {
    /// File or directory name
    pub name: String,
    /// Full path from root
    pub path: String,
    /// Whether this is a directory
    pub is_dir: bool,
    /// File size in bytes (0 for directories)
    pub size: u64,
    /// Last modification time (ISO 8601 string)
    pub modified: Option<String>,
    /// Permission string (Unix-style, e.g., "rwxr-xr-x")
    pub permissions: Option<String>,
    /// Owner name
    pub owner: Option<String>,
    /// Group name
    pub group: Option<String>,
    /// Whether this is a symbolic link
    pub is_symlink: bool,
    /// Link target (if symlink)
    pub link_target: Option<String>,
    /// MIME type (if known)
    pub mime_type: Option<String>,
    /// Provider-specific metadata
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, String>,
}

impl RemoteEntry {
    /// Create a new directory entry
    pub fn directory(name: String, path: String) -> Self {
        Self {
            name,
            path,
            is_dir: true,
            size: 0,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        }
    }
    
    /// Create a new file entry
    pub fn file(name: String, path: String, size: u64) -> Self {
        Self {
            name,
            path,
            is_dir: false,
            size,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        }
    }
    
    /// Get file extension
    pub fn extension(&self) -> Option<&str> {
        if self.is_dir {
            return None;
        }
        self.name.rsplit('.').next().filter(|ext| ext.len() < self.name.len())
    }
}

/// Provider error type
#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("Not connected to server")]
    NotConnected,
    
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    
    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),
    
    #[error("Path not found: {0}")]
    NotFound(String),
    
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    
    #[error("Path already exists: {0}")]
    AlreadyExists(String),
    
    #[error("Directory not empty: {0}")]
    DirectoryNotEmpty(String),
    
    #[error("Invalid path: {0}")]
    InvalidPath(String),
    
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
    
    #[error("Operation not supported: {0}")]
    NotSupported(String),
    
    #[error("Transfer cancelled")]
    Cancelled,
    
    #[error("Transfer failed: {0}")]
    TransferFailed(String),
    
    #[error("Timeout")]
    Timeout,
    
    #[error("Network error: {0}")]
    NetworkError(String),
    
    #[error("Parse error: {0}")]
    ParseError(String),
    
    #[error("Server error: {0}")]
    ServerError(String),
    
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    
    #[error("Unknown error: {0}")]
    Unknown(String),
    
    #[error("{0}")]
    Other(String),
}

impl ProviderError {
    /// Check if this error is recoverable (can retry)
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            ProviderError::Timeout
                | ProviderError::NetworkError(_)
                | ProviderError::NotConnected
        )
    }
}

/// Transfer progress information
#[derive(Debug, Clone, Serialize)]
pub struct TransferProgressInfo {
    /// Bytes transferred so far
    pub bytes_transferred: u64,
    /// Total bytes to transfer
    pub total_bytes: u64,
    /// Progress percentage (0-100)
    pub percentage: f64,
    /// Current transfer speed in bytes/second
    pub speed_bps: u64,
    /// Estimated time remaining in seconds
    pub eta_seconds: Option<u64>,
}

impl TransferProgressInfo {
    pub fn new(bytes_transferred: u64, total_bytes: u64) -> Self {
        let percentage = if total_bytes > 0 {
            (bytes_transferred as f64 / total_bytes as f64) * 100.0
        } else {
            0.0
        };
        
        Self {
            bytes_transferred,
            total_bytes,
            percentage,
            speed_bps: 0,
            eta_seconds: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_provider_type_default_port() {
        assert_eq!(ProviderType::Ftp.default_port(), 21);
        assert_eq!(ProviderType::Ftps.default_port(), 990);
        assert_eq!(ProviderType::Sftp.default_port(), 22);
        assert_eq!(ProviderType::WebDav.default_port(), 443);
        assert_eq!(ProviderType::S3.default_port(), 443);
    }
    
    #[test]
    fn test_remote_entry_extension() {
        let file = RemoteEntry::file("document.pdf".to_string(), "/path/document.pdf".to_string(), 1000);
        assert_eq!(file.extension(), Some("pdf"));
        
        let dir = RemoteEntry::directory("folder".to_string(), "/path/folder".to_string());
        assert_eq!(dir.extension(), None);
        
        let no_ext = RemoteEntry::file("Makefile".to_string(), "/path/Makefile".to_string(), 500);
        assert_eq!(no_ext.extension(), None);
    }
}
