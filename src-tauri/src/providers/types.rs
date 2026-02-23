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
    /// MEGA.nz Cloud Storage
    Mega,
    /// Box Cloud Storage (OAuth2)
    Box,
    /// pCloud (OAuth2)
    PCloud,
    /// Azure Blob Storage
    Azure,
    /// Filen.io (E2E Encrypted)
    Filen,
    /// 4shared (OAuth 1.0)
    FourShared,
    /// Zoho WorkDrive (OAuth2)
    ZohoWorkdrive,
    /// Internxt Drive (E2E Encrypted)
    Internxt,
    /// Infomaniak kDrive (Swiss Cloud)
    KDrive,
    /// Jottacloud (Norwegian Secure Cloud)
    Jottacloud,
    /// Drime Cloud (20GB Secure Cloud)
    DrimeCloud,
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
            ProviderType::Mega => write!(f, "MEGA"),
            ProviderType::Box => write!(f, "Box"),
            ProviderType::PCloud => write!(f, "pCloud"),
            ProviderType::Azure => write!(f, "Azure Blob"),
            ProviderType::Filen => write!(f, "Filen"),
            ProviderType::FourShared => write!(f, "4shared"),
            ProviderType::ZohoWorkdrive => write!(f, "Zoho WorkDrive"),
            ProviderType::Internxt => write!(f, "Internxt Drive"),
            ProviderType::KDrive => write!(f, "kDrive"),
            ProviderType::Jottacloud => write!(f, "Jottacloud"),
            ProviderType::DrimeCloud => write!(f, "Drime Cloud"),
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
            ProviderType::Mega => 443,
            ProviderType::Box => 443,
            ProviderType::PCloud => 443,
            ProviderType::Azure => 443,
            ProviderType::Filen => 443,
            ProviderType::FourShared => 443,
            ProviderType::ZohoWorkdrive => 443,
            ProviderType::Internxt => 443,
            ProviderType::KDrive => 443,
            ProviderType::Jottacloud => 443,
            ProviderType::DrimeCloud => 443,
        }
    }
    
    /// Check if this provider uses encryption by default
    #[allow(dead_code)]
    pub fn uses_encryption(&self) -> bool {
        matches!(self,
            ProviderType::Ftps |
            ProviderType::Sftp |
            ProviderType::WebDav |
            ProviderType::S3 |
            ProviderType::AeroCloud |  // AeroCloud recommends FTPS
            ProviderType::GoogleDrive |
            ProviderType::Dropbox |
            ProviderType::OneDrive |
            ProviderType::Mega |
            ProviderType::Box |
            ProviderType::PCloud |
            ProviderType::Azure |
            ProviderType::Filen |
            ProviderType::FourShared |
            ProviderType::ZohoWorkdrive |
            ProviderType::Internxt |
            ProviderType::KDrive |
            ProviderType::Jottacloud |
            ProviderType::DrimeCloud
        )
    }

    /// Check if this provider requires OAuth2 authentication
    #[allow(dead_code)]
    pub fn requires_oauth2(&self) -> bool {
        matches!(self,
            ProviderType::GoogleDrive |
            ProviderType::Dropbox |
            ProviderType::OneDrive |
            ProviderType::Box |
            ProviderType::PCloud |
            ProviderType::ZohoWorkdrive
        )
    }

    /// Check if this is an AeroCloud provider (uses FTP backend with sync)
    #[allow(dead_code)]
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

/// TLS mode for FTP connections
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FtpTlsMode {
    /// Plain FTP (no encryption)
    #[default]
    None,
    /// Explicit TLS (AUTH TLS on port 21) - required
    Explicit,
    /// Implicit TLS (direct TLS on port 990)
    Implicit,
    /// Try explicit TLS, fall back to plain if unsupported
    ExplicitIfAvailable,
}

/// FTP-specific configuration
#[derive(Debug, Clone)]
pub struct FtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: secrecy::SecretString,
    pub tls_mode: FtpTlsMode,
    pub verify_cert: bool,
    pub initial_path: Option<String>,
}

impl FtpConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let tls_mode = config.extra.get("tls_mode")
            .map(|v| match v.as_str() {
                "explicit" => FtpTlsMode::Explicit,
                "implicit" => FtpTlsMode::Implicit,
                "explicit_if_available" => FtpTlsMode::ExplicitIfAvailable,
                _ => FtpTlsMode::None,
            })
            .unwrap_or_else(|| {
                if config.provider_type == ProviderType::Ftps {
                    FtpTlsMode::Implicit
                } else {
                    FtpTlsMode::None
                }
            });

        let verify_cert = config.extra.get("verify_cert")
            .map(|v| v != "false")
            .unwrap_or(true);

        Ok(Self {
            host: config.host.clone(),
            port: config.effective_port(),
            username: config.username.clone().unwrap_or_else(|| "anonymous".to_string()),
            password: secrecy::SecretString::from(config.password.clone().unwrap_or_default()),
            tls_mode,
            verify_cert,
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
    pub password: secrecy::SecretString,
    pub initial_path: Option<String>,
    /// Whether to verify TLS certificates (default: true). Set to false for self-signed certs.
    pub verify_cert: bool,
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
        
        let raw_url = if config.host.starts_with("http://") || config.host.starts_with("https://") {
            config.host.clone()
        } else {
            format!("{}://{}{}", scheme, config.host, port_suffix)
        };

        // Resolve {username} template in URL (used by CloudMe, Nextcloud presets)
        let username = config.username.clone().unwrap_or_default();
        let url = raw_url.replace("{username}", &username);

        let verify_cert = config.extra.get("verify_cert")
            .map(|v| v != "false")
            .unwrap_or(true);

        Ok(Self {
            url,
            username,
            password: secrecy::SecretString::from(config.password.clone().unwrap_or_default()),
            initial_path: config.initial_path.clone(),
            verify_cert,
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
    /// Secret access key (SecretString for memory zeroization)
    pub secret_access_key: secrecy::SecretString,
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
            .trim().to_string();

        let region = config.extra.get("region")
            .cloned()
            .unwrap_or_else(|| "us-east-1".to_string())
            .trim().to_string();

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
            secret_access_key: secrecy::SecretString::from(config.password.clone().unwrap_or_default()),
            bucket,
            prefix: config.initial_path.clone(),
            path_style,
        })
    }
}

/// SFTP-specific configuration
#[derive(Debug, Clone)]
pub struct SftpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Password authentication (optional if using key)
    pub password: Option<secrecy::SecretString>,
    /// Path to private key file (e.g., ~/.ssh/id_rsa)
    pub private_key_path: Option<String>,
    /// Passphrase for encrypted private key
    pub key_passphrase: Option<secrecy::SecretString>,
    /// Initial directory to navigate to
    pub initial_path: Option<String>,
    /// Connection timeout in seconds
    pub timeout_secs: u64,
}

impl SftpConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let username = config.username.clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Username required for SFTP".to_string()))?;

        let private_key_path = config.extra.get("private_key_path").cloned();
        let key_passphrase = config.extra.get("key_passphrase")
            .map(|v| secrecy::SecretString::from(v.clone()));

        let timeout_secs = config.extra.get("timeout")
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);

        Ok(Self {
            host: config.host.clone(),
            port: config.effective_port(),
            username,
            password: config.password.clone().map(secrecy::SecretString::from),
            private_key_path,
            key_passphrase,
            initial_path: config.initial_path.clone(),
            timeout_secs,
        })
    }
}

/// MEGA configuration
#[derive(Debug, Clone)]
pub struct MegaConfig {
    pub email: String,
    pub password: secrecy::SecretString,
    /// Whether to save session for reconnection (used in future session persistence)
    #[allow(dead_code)]
    pub save_session: bool,
    pub logout_on_disconnect: Option<bool>,
}

impl MegaConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let email = config.username.clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Email required for MEGA".to_string()))?;
            
        let password = config.password.clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Password required for MEGA".to_string()))?;
            
        let save_session = config.extra.get("save_session")
            .map(|v| v == "true")
            .unwrap_or(true);
            
        let logout_on_disconnect = config.extra.get("logout_on_disconnect")
            .map(|v| v == "true");

        Ok(Self {
            email,
            password: password.into(),
            save_session,
            logout_on_disconnect,
        })
    }
}

/// Box configuration
#[derive(Debug, Clone)]
pub struct BoxConfig {
    pub client_id: String,
    pub client_secret: String,
}

impl BoxConfig {
    pub fn new(client_id: &str, client_secret: &str) -> Self {
        Self {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        }
    }

    #[allow(dead_code)]
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let client_id = config.extra.get("client_id")
            .ok_or_else(|| ProviderError::InvalidConfig("Missing client_id for Box".to_string()))?;
        let client_secret = config.extra.get("client_secret")
            .ok_or_else(|| ProviderError::InvalidConfig("Missing client_secret for Box".to_string()))?;
        Ok(Self::new(client_id, client_secret))
    }
}

/// pCloud configuration
#[derive(Debug, Clone)]
pub struct PCloudConfig {
    pub client_id: String,
    pub client_secret: String,
    /// API region: "us" or "eu"
    pub region: String,
}

impl PCloudConfig {
    pub fn new(client_id: &str, client_secret: &str, region: &str) -> Self {
        Self {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            region: region.to_string(),
        }
    }

    #[allow(dead_code)]
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let client_id = config.extra.get("client_id")
            .ok_or_else(|| ProviderError::InvalidConfig("Missing client_id for pCloud".to_string()))?;
        let client_secret = config.extra.get("client_secret")
            .ok_or_else(|| ProviderError::InvalidConfig("Missing client_secret for pCloud".to_string()))?;
        let region = config.extra.get("region")
            .cloned()
            .unwrap_or_else(|| "us".to_string());
        Ok(Self::new(client_id, client_secret, &region))
    }

    /// Get the API base URL for this region
    pub fn api_base(&self) -> &str {
        if self.region == "eu" {
            "https://eapi.pcloud.com"
        } else {
            "https://api.pcloud.com"
        }
    }
}

/// Azure Blob Storage configuration
#[derive(Debug, Clone)]
pub struct AzureConfig {
    /// Storage account name
    pub account_name: String,
    /// Shared Key for HMAC signing (SecretString for memory zeroization)
    pub access_key: secrecy::SecretString,
    /// Container name
    pub container: String,
    /// Optional SAS token (alternative to access_key)
    pub sas_token: Option<secrecy::SecretString>,
    /// Custom endpoint (for Azure Stack, Azurite emulator, etc.)
    pub endpoint: Option<String>,
}

impl AzureConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let account_name = config.extra.get("account_name")
            .or(config.username.as_ref())
            .ok_or_else(|| ProviderError::InvalidConfig("Account name required for Azure".to_string()))?
            .clone();
        let access_key: secrecy::SecretString = config.extra.get("access_key")
            .or(config.password.as_ref())
            .ok_or_else(|| ProviderError::InvalidConfig("Access key required for Azure".to_string()))?
            .clone()
            .into();
        let container = config.extra.get("container")
            .ok_or_else(|| ProviderError::InvalidConfig("Container name required for Azure".to_string()))?
            .clone();
        let sas_token: Option<secrecy::SecretString> = config.extra.get("sas_token").map(|s| s.clone().into());
        // Host may arrive as ":443" when the endpoint field is empty but port is set
        let clean_host = config.host.split(':').next().unwrap_or("").trim();
        let endpoint = if clean_host.is_empty() || clean_host == "blob.core.windows.net" {
            None
        } else {
            Some(config.host.clone())
        };
        Ok(Self { account_name, access_key, container, sas_token, endpoint })
    }

    /// Get the blob service endpoint URL
    pub fn blob_endpoint(&self) -> String {
        if let Some(ref ep) = self.endpoint {
            // Ensure custom endpoint has a scheme
            if ep.starts_with("http://") || ep.starts_with("https://") {
                ep.clone()
            } else {
                format!("https://{}", ep)
            }
        } else {
            format!("https://{}.blob.core.windows.net", self.account_name)
        }
    }
}

/// Filen configuration
#[derive(Debug, Clone)]
pub struct FilenConfig {
    pub email: String,
    pub password: secrecy::SecretString,
    /// Optional TOTP code for accounts with 2FA enabled
    pub two_factor_code: Option<String>,
}

impl FilenConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let email = config.username.clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Email required for Filen".to_string()))?;
        let password = config.password.clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Password required for Filen".to_string()))?;
        let two_factor_code = config.extra.get("two_factor_code").cloned();
        Ok(Self { email, password: password.into(), two_factor_code })
    }
}

/// Internxt Drive configuration
#[derive(Debug, Clone)]
pub struct InternxtConfig {
    pub email: String,
    pub password: secrecy::SecretString,
    /// Optional TOTP code for accounts with 2FA enabled
    pub two_factor_code: Option<String>,
    /// Optional initial remote path
    pub initial_path: Option<String>,
}

impl InternxtConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let email = config.username.clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Email required for Internxt".to_string()))?;
        let password = config.password.clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Password required for Internxt".to_string()))?;
        let two_factor_code = config.extra.get("two_factor_code").cloned();
        Ok(Self {
            email,
            password: password.into(),
            two_factor_code,
            initial_path: config.initial_path.clone(),
        })
    }
}

/// Infomaniak kDrive configuration (API Token)
#[derive(Debug, Clone)]
pub struct KDriveConfig {
    /// Bearer API token from Infomaniak dashboard
    pub api_token: secrecy::SecretString,
    /// kDrive ID (numeric)
    pub drive_id: String,
    /// Optional initial remote path
    pub initial_path: Option<String>,
}

impl KDriveConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let token = config.password.clone()
            .ok_or_else(|| ProviderError::InvalidConfig("API token required for kDrive".to_string()))?;
        let drive_id = config.extra.get("drive_id").cloned()
            .ok_or_else(|| ProviderError::InvalidConfig("Drive ID required for kDrive".to_string()))?;
        // F6: Validate drive_id is numeric to prevent URL path traversal
        if !drive_id.chars().all(|c| c.is_ascii_digit()) {
            return Err(ProviderError::InvalidConfig("Drive ID must be numeric".to_string()));
        }
        Ok(Self {
            api_token: token.into(),
            drive_id,
            initial_path: config.initial_path.clone(),
        })
    }
}

/// Jottacloud configuration (Personal Login Token)
#[derive(Debug, Clone)]
pub struct JottacloudConfig {
    /// Base64-encoded Personal Login Token from Jottacloud settings
    pub login_token: secrecy::SecretString,
    /// Device name (default "Jotta")
    pub device: String,
    /// Mountpoint name (default "Archive")
    pub mountpoint: String,
    /// Optional initial remote path
    pub initial_path: Option<String>,
}

impl JottacloudConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let token = config.password.clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Login token required for Jottacloud".to_string()))?;
        let device = config.extra.get("device").cloned()
            .unwrap_or_else(|| "Jotta".to_string());
        let mountpoint = config.extra.get("mountpoint").cloned()
            .unwrap_or_else(|| "Archive".to_string());
        // Validate device/mountpoint don't contain path traversal
        if device.contains("..") || device.contains('/') {
            return Err(ProviderError::InvalidConfig("Invalid device name".to_string()));
        }
        if mountpoint.contains("..") || mountpoint.contains('/') {
            return Err(ProviderError::InvalidConfig("Invalid mountpoint name".to_string()));
        }
        Ok(Self {
            login_token: token.into(),
            device,
            mountpoint,
            initial_path: config.initial_path.clone(),
        })
    }
}

/// Drime Cloud configuration (API Token)
#[derive(Debug, Clone)]
pub struct DrimeCloudConfig {
    /// Bearer API token from Drime Cloud dashboard
    pub api_token: secrecy::SecretString,
    /// Optional initial remote path
    pub initial_path: Option<String>,
}

impl DrimeCloudConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let token = config.password.clone()
            .ok_or_else(|| ProviderError::InvalidConfig("API token required for Drime Cloud".to_string()))?;
        Ok(Self {
            api_token: token.into(),
            initial_path: config.initial_path.clone(),
        })
    }
}

/// 4shared configuration (OAuth 1.0)
#[derive(Debug, Clone)]
pub struct FourSharedConfig {
    pub consumer_key: String,
    pub consumer_secret: secrecy::SecretString,
    pub access_token: secrecy::SecretString,
    pub access_token_secret: secrecy::SecretString,
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

    /// Create a new file entry (used in tests and future provider implementations)
    #[allow(dead_code)]
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

    /// Get file extension (used in tests and MIME type detection)
    #[allow(dead_code)]
    pub fn extension(&self) -> Option<&str> {
        if self.is_dir {
            return None;
        }
        self.name.rsplit('.').next().filter(|ext| ext.len() < self.name.len())
    }
}

/// Provider error type
#[derive(Error, Debug)]
#[allow(dead_code)]
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
    #[allow(dead_code)]
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            ProviderError::Timeout
                | ProviderError::NetworkError(_)
                | ProviderError::NotConnected
        )
    }
}

/// Storage quota information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageInfo {
    /// Bytes used
    pub used: u64,
    /// Total bytes available
    pub total: u64,
    /// Bytes free
    pub free: u64,
}

/// File version metadata (for versioned providers like Google Drive, Dropbox, OneDrive)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileVersion {
    /// Version identifier
    pub id: String,
    /// Modification timestamp
    pub modified: Option<String>,
    /// Size in bytes
    pub size: u64,
    /// User who modified (if available)
    pub modified_by: Option<String>,
}

/// Lock information for WebDAV locking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockInfo {
    /// Lock token
    pub token: String,
    /// Lock owner
    pub owner: Option<String>,
    /// Lock timeout in seconds (0 = infinite)
    pub timeout: u64,
    /// Whether this is an exclusive lock
    pub exclusive: bool,
}

/// Share permission for advanced sharing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharePermission {
    /// Permission role: "reader", "writer", "commenter", "owner"
    pub role: String,
    /// Target type: "user", "group", "domain", "anyone"
    pub target_type: String,
    /// Target email or identifier (empty for "anyone")
    pub target: String,
}

/// Change tracking entry (for delta sync)
#[derive(Debug, Clone, Serialize)]
pub struct ChangeEntry {
    /// File/folder path or ID
    pub file_id: String,
    /// File name
    pub name: String,
    /// Change type: "created", "modified", "deleted", "renamed"
    pub change_type: String,
    /// MIME type of the changed file
    pub mime_type: Option<String>,
    /// Timestamp of the change
    pub timestamp: Option<String>,
    /// Whether the file was trashed/deleted
    pub removed: bool,
}

/// Transfer progress information (for future progress events)
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
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

#[allow(dead_code)]
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
