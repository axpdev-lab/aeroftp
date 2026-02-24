//! SFTP Provider Implementation
//!
//! This module provides SFTP (SSH File Transfer Protocol) support using the russh crate.
//! Supports both password and SSH key-based authentication.
//!
//! Status: v1.3.0

use super::{ProviderError, ProviderType, RemoteEntry, SftpConfig, StorageProvider};
use async_trait::async_trait;
use russh::client::{self, Config, Handle, Handler};
use russh::keys::{self, known_hosts, PrivateKeyWithHashAlg, PublicKey};
use russh::client::AuthResult;
use russh::{compression, Preferred};
use russh_sftp::client::SftpSession;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// SSH Client Handler for server key verification
struct SshHandler {
    /// The host being connected to (for known_hosts lookup)
    host: String,
    /// The port being connected to
    port: u16,
}

impl SshHandler {
    fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
        }
    }
}

impl Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        // Use russh's built-in known_hosts verification
        match known_hosts::check_known_hosts(&self.host, self.port, server_public_key) {
            Ok(true) => {
                tracing::info!("SFTP: Host key verified for {}", self.host);
                Ok(true)
            }
            Ok(false) => {
                // SEC-P1-06: Host not in known_hosts — reject here.
                // Frontend must call sftp_check_host_key + sftp_accept_host_key first.
                tracing::warn!(
                    "SFTP: Host key for {} not pre-approved via TOFU dialog — rejecting",
                    self.host
                );
                Ok(false)
            }
            Err(keys::Error::KeyChanged { line }) => {
                tracing::error!(
                    "SFTP: REJECTING connection to {} - host key changed at known_hosts line {} (possible MITM attack)",
                    self.host, line
                );
                Ok(false)
            }
            Err(e) => {
                // SEC: Reject on unknown errors — do not silently accept.
                // Only TOFU (Ok(false)) should auto-accept; other errors may indicate
                // corrupted known_hosts or key format issues.
                tracing::error!(
                    "SFTP: REJECTING connection to {} - known_hosts verification error: {}",
                    self.host, e
                );
                Ok(false)
            }
        }
    }
}

/// SFTP Provider
///
/// Provides secure file transfer over SSH using the SFTP protocol.
pub struct SftpProvider {
    config: SftpConfig,
    /// SSH connection handle
    ssh_handle: Option<Handle<SshHandler>>,
    /// SFTP session for file operations
    sftp: Option<SftpSession>,
    /// Current working directory
    current_dir: String,
    /// Home directory (resolved on connect)
    home_dir: String,
    /// Download speed limit in bytes/sec (0 = unlimited)
    download_limit_bps: u64,
    /// Upload speed limit in bytes/sec (0 = unlimited)
    upload_limit_bps: u64,
    /// SSH compression enabled (zlib@openssh.com)
    compression_enabled: bool,
}

impl SftpProvider {
    pub fn new(config: SftpConfig) -> Self {
        Self {
            config,
            ssh_handle: None,
            sftp: None,
            current_dir: "/".to_string(),
            home_dir: "/".to_string(),
            download_limit_bps: 0,
            upload_limit_bps: 0,
            compression_enabled: false,
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
        } else if path == "~" {
            self.home_dir.clone()
        } else if let Some(stripped) = path.strip_prefix("~/") {
            format!("{}/{}", self.home_dir.trim_end_matches('/'), stripped)
        } else {
            format!("{}/{}", self.current_dir.trim_end_matches('/'), path)
        }
    }

    /// Get SFTP session or error if not connected
    fn get_sftp(&self) -> Result<&SftpSession, ProviderError> {
        self.sftp.as_ref().ok_or(ProviderError::NotConnected)
    }

    /// Get mutable SFTP session or error if not connected
    #[allow(dead_code)]
    fn get_sftp_mut(&mut self) -> Result<&mut SftpSession, ProviderError> {
        self.sftp.as_mut().ok_or(ProviderError::NotConnected)
    }

    /// Convert russh-sftp metadata to RemoteEntry
    fn metadata_to_entry(&self, name: String, path: String, metadata: &russh_sftp::protocol::FileAttributes) -> RemoteEntry {
        let is_dir = metadata.permissions
            .map(|p| (p & 0o40000) != 0)
            .unwrap_or(false);

        let permissions = metadata.permissions.map(|p| {
            format_permissions(p, is_dir)
        });

        let modified = metadata.mtime.map(|t| {
            chrono::DateTime::from_timestamp(t as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default()
        });

        RemoteEntry {
            name,
            path,
            is_dir,
            size: metadata.size.unwrap_or(0),
            modified,
            permissions,
            owner: metadata.uid.map(|u| u.to_string()),
            group: metadata.gid.map(|g| g.to_string()),
            is_symlink: false, // Will be set separately for symlinks
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        }
    }

    /// Authenticate using SSH private key
    async fn authenticate_with_key(&self, handle: &mut Handle<SshHandler>) -> Result<bool, ProviderError> {
        let key_path = self.config.private_key_path.as_ref()
            .ok_or_else(|| ProviderError::AuthenticationFailed("No private key path specified".to_string()))?;

        // Expand ~ in path (cross-platform: uses PathBuf for correct separator)
        let expanded_path = if let Some(stripped) = key_path.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                home.join(stripped).to_string_lossy().to_string()
            } else {
                key_path.clone()
            }
        } else {
            key_path.clone()
        };

        tracing::info!("SFTP: Loading private key from {}", expanded_path);

        // Load and parse the key using russh's built-in key loading
        use secrecy::ExposeSecret;
        let passphrase_str = self.config.key_passphrase.as_ref().map(|s| s.expose_secret().to_string());
        let key_pair = keys::load_secret_key(&expanded_path, passphrase_str.as_deref())
            .map_err(|e| ProviderError::AuthenticationFailed(format!("Failed to load key: {}", e)))?;

        // Wrap in PrivateKeyWithHashAlg (required by russh 0.54+)
        let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(key_pair), None);

        // Authenticate
        let auth_result = handle.authenticate_publickey(&self.config.username, key_with_hash).await
            .map_err(|e| ProviderError::AuthenticationFailed(format!("Key authentication failed: {}", e)))?;

        match auth_result {
            AuthResult::Success => Ok(true),
            AuthResult::Failure { .. } => Ok(false),
        }
    }
}

/// Format Unix permissions as rwx string
fn format_permissions(mode: u32, is_dir: bool) -> String {
    let user = format!(
        "{}{}{}",
        if mode & 0o400 != 0 { 'r' } else { '-' },
        if mode & 0o200 != 0 { 'w' } else { '-' },
        if mode & 0o100 != 0 { 'x' } else { '-' }
    );
    let group = format!(
        "{}{}{}",
        if mode & 0o040 != 0 { 'r' } else { '-' },
        if mode & 0o020 != 0 { 'w' } else { '-' },
        if mode & 0o010 != 0 { 'x' } else { '-' }
    );
    let other = format!(
        "{}{}{}",
        if mode & 0o004 != 0 { 'r' } else { '-' },
        if mode & 0o002 != 0 { 'w' } else { '-' },
        if mode & 0o001 != 0 { 'x' } else { '-' }
    );
    format!("{}{}{}{}", if is_dir { 'd' } else { '-' }, user, group, other)
}

#[async_trait]
impl StorageProvider for SftpProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Sftp
    }

    fn display_name(&self) -> String {
        format!("{}@{}", self.config.username, self.config.host)
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        tracing::info!("SFTP: Connecting to {}:{}", self.config.host, self.config.port);

        // Create SSH config with keepalive to prevent server from closing connection
        let preferred = if self.compression_enabled {
            tracing::info!("SFTP: SSH compression enabled (zlib@openssh.com)");
            Preferred {
                compression: std::borrow::Cow::Borrowed(&[
                    compression::ZLIB_LEGACY,
                    compression::ZLIB,
                    compression::NONE,
                ]),
                ..Default::default()
            }
        } else {
            Preferred::default()
        };
        let config = Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(self.config.timeout_secs * 2)),
            keepalive_interval: Some(std::time::Duration::from_secs(15)), // Send keepalive every 15s
            keepalive_max: 3, // Allow 3 missed keepalives before disconnect
            preferred,
            ..Default::default()
        };

        // Connect to SSH server
        let addr = format!("{}:{}", self.config.host, self.config.port);
        let mut handle = client::connect(Arc::new(config), &addr, SshHandler::new(&self.config.host, self.config.port)).await
            .map_err(|e| ProviderError::ConnectionFailed(format!("SSH connection failed: {}", e)))?;

        tracing::info!("SFTP: SSH connection established, authenticating...");

        // Authenticate
        let authenticated = if self.config.private_key_path.is_some() {
            // Try key-based authentication
            self.authenticate_with_key(&mut handle).await?
        } else if let Some(password) = &self.config.password {
            // Password authentication
            use secrecy::ExposeSecret;
            let result = handle.authenticate_password(&self.config.username, password.expose_secret()).await
                .map_err(|e| ProviderError::AuthenticationFailed(format!("Password auth failed: {}", e)))?;
            matches!(result, AuthResult::Success)
        } else {
            return Err(ProviderError::AuthenticationFailed(
                "No authentication method provided (need password or private key)".to_string()
            ));
        };

        if !authenticated {
            return Err(ProviderError::AuthenticationFailed(
                "Authentication rejected by server".to_string()
            ));
        }

        tracing::info!("SFTP: Authenticated successfully, opening SFTP channel...");

        // Open SFTP subsystem channel
        let channel = handle.channel_open_session().await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Failed to open session channel: {}", e)))?;

        channel.request_subsystem(true, "sftp").await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Failed to request SFTP subsystem: {}", e)))?;

        // Create SFTP session from channel
        let sftp = SftpSession::new(channel.into_stream()).await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Failed to create SFTP session: {}", e)))?;

        // Get home directory (canonicalize ".")
        let home = sftp.canonicalize(".").await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Failed to get home directory: {}", e)))?;

        self.home_dir = home;

        // Set initial directory
        if let Some(initial) = &self.config.initial_path {
            self.current_dir = self.normalize_path(initial);
        } else {
            self.current_dir = self.home_dir.clone();
        }

        self.ssh_handle = Some(handle);
        self.sftp = Some(sftp);

        tracing::info!("SFTP: Connected successfully to {} (home: {})", self.config.host, self.home_dir);
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        tracing::info!("SFTP: Disconnecting from {}", self.config.host);

        // Close SFTP session
        if let Some(sftp) = self.sftp.take() {
            let _ = sftp.close().await;
        }

        // Close SSH handle
        if let Some(handle) = self.ssh_handle.take() {
            let _ = handle.disconnect(russh::Disconnect::ByApplication, "", "en").await;
        }

        self.current_dir = "/".to_string();
        self.home_dir = "/".to_string();

        tracing::info!("SFTP: Disconnected");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.sftp.is_some()
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::debug!("SFTP: Listing directory: {}", full_path);

        let entries = sftp.read_dir(&full_path).await
            .map_err(|e| ProviderError::NotFound(format!("Failed to list directory: {}", e)))?;

        let mut result = Vec::new();

        for entry in entries {
            let name = entry.file_name();

            // Skip . and ..
            if name == "." || name == ".." {
                continue;
            }

            let entry_path = if full_path == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", full_path.trim_end_matches('/'), name)
            };

            let mut remote_entry = self.metadata_to_entry(name.clone(), entry_path.clone(), &entry.metadata());

            // Check if it's a symlink
            if let Ok(link_meta) = sftp.symlink_metadata(&entry_path).await {
                if let Some(perms) = link_meta.permissions {
                    // S_IFLNK = 0o120000
                    if (perms & 0o170000) == 0o120000 {
                        remote_entry.is_symlink = true;
                        if let Ok(target) = sftp.read_link(&entry_path).await {
                            remote_entry.link_target = Some(target);
                        }
                        // Follow the symlink to determine the real type (file vs directory)
                        // metadata() follows symlinks, unlike symlink_metadata()
                        if let Ok(target_meta) = sftp.metadata(&entry_path).await {
                            if let Some(target_perms) = target_meta.permissions {
                                remote_entry.is_dir = (target_perms & 0o40000) != 0;
                            }
                            // Update size from target if available
                            if let Some(target_size) = target_meta.size {
                                remote_entry.size = target_size;
                            }
                        }
                    }
                }
            }

            result.push(remote_entry);
        }

        // Sort: directories first, then alphabetically
        result.sort_by(|a, b| {
            match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
        });

        tracing::debug!("SFTP: Listed {} entries", result.len());
        Ok(result)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_dir.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        // Verify the directory exists
        let metadata = sftp.metadata(&full_path).await
            .map_err(|e| ProviderError::NotFound(format!("Directory not found: {}", e)))?;

        if let Some(perms) = metadata.permissions {
            if (perms & 0o40000) == 0 {
                return Err(ProviderError::InvalidPath(format!("{} is not a directory", full_path)));
            }
        }

        self.current_dir = full_path;
        tracing::debug!("SFTP: Changed directory to {}", self.current_dir);
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(remote_path);

        tracing::info!("SFTP: Downloading {} to {}", full_path, local_path);

        // Get file size
        let metadata = sftp.metadata(&full_path).await
            .map_err(|e| ProviderError::NotFound(format!("File not found: {}", e)))?;
        let total_size = metadata.size.unwrap_or(0);

        // Open remote file
        let mut remote_file = sftp.open(&full_path).await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to open remote file: {}", e)))?;

        // Create local file
        let mut local_file = tokio::fs::File::create(local_path).await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to create local file: {}", e)))?;

        // Read and write in chunks with optional rate limiting
        let mut buffer = vec![0u8; 32768]; // 32KB chunks
        let mut transferred: u64 = 0;
        let start = std::time::Instant::now();

        loop {
            let bytes_read = remote_file.read(&mut buffer).await
                .map_err(|e| ProviderError::TransferFailed(format!("Read error: {}", e)))?;

            if bytes_read == 0 {
                break;
            }

            local_file.write_all(&buffer[..bytes_read]).await
                .map_err(|e| ProviderError::TransferFailed(format!("Write error: {}", e)))?;

            transferred += bytes_read as u64;

            if let Some(ref progress) = on_progress {
                progress(transferred, total_size);
            }

            // Apply bandwidth throttling
            if self.download_limit_bps > 0 {
                let expected = std::time::Duration::from_secs_f64(transferred as f64 / self.download_limit_bps as f64);
                let elapsed = start.elapsed();
                if expected > elapsed {
                    tokio::time::sleep(expected - elapsed).await;
                }
            }
        }

        local_file.flush().await
            .map_err(|e| ProviderError::TransferFailed(format!("Flush error: {}", e)))?;

        tracing::info!("SFTP: Download complete: {} bytes", transferred);
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(remote_path);
        let limit = super::MAX_DOWNLOAD_TO_BYTES;

        tracing::debug!("SFTP: Reading file to bytes: {}", full_path);

        // H2: Check file size before reading to prevent OOM
        if let Ok(metadata) = sftp.metadata(&full_path).await {
            if metadata.size.unwrap_or(0) > limit {
                return Err(ProviderError::TransferFailed(format!(
                    "File too large for in-memory download ({:.1} MB). Use streaming download for files over {:.0} MB.",
                    metadata.size.unwrap_or(0) as f64 / 1_048_576.0,
                    limit as f64 / 1_048_576.0,
                )));
            }
        }

        let data = sftp.read(&full_path).await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to read file: {}", e)))?;

        if data.len() as u64 > limit {
            return Err(ProviderError::TransferFailed(format!(
                "Download exceeded {:.0} MB size limit. Use streaming download for large files.",
                limit as f64 / 1_048_576.0,
            )));
        }

        Ok(data)
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(remote_path);

        tracing::info!("SFTP: Uploading {} to {}", local_path, full_path);

        // Open local file
        let mut local_file = tokio::fs::File::open(local_path).await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to open local file: {}", e)))?;

        let total_size = local_file.metadata().await
            .map(|m| m.len())
            .unwrap_or(0);

        // Create remote file
        let mut remote_file = sftp.create(&full_path).await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to create remote file: {}", e)))?;

        // Read and write in chunks with optional rate limiting
        let mut buffer = vec![0u8; 32768]; // 32KB chunks
        let mut transferred: u64 = 0;
        let start = std::time::Instant::now();

        loop {
            let bytes_read = local_file.read(&mut buffer).await
                .map_err(|e| ProviderError::TransferFailed(format!("Read error: {}", e)))?;

            if bytes_read == 0 {
                break;
            }

            remote_file.write_all(&buffer[..bytes_read]).await
                .map_err(|e| ProviderError::TransferFailed(format!("Write error: {}", e)))?;

            transferred += bytes_read as u64;

            if let Some(ref progress) = on_progress {
                progress(transferred, total_size);
            }

            // Apply bandwidth throttling
            if self.upload_limit_bps > 0 {
                let expected = std::time::Duration::from_secs_f64(transferred as f64 / self.upload_limit_bps as f64);
                let elapsed = start.elapsed();
                if expected > elapsed {
                    tokio::time::sleep(expected - elapsed).await;
                }
            }
        }

        remote_file.shutdown().await
            .map_err(|e| ProviderError::TransferFailed(format!("Shutdown error: {}", e)))?;

        tracing::info!("SFTP: Upload complete: {} bytes", transferred);
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Creating directory: {}", full_path);

        sftp.create_dir(&full_path).await
            .map_err(|e| ProviderError::ServerError(format!("Failed to create directory: {}", e)))?;

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Deleting file: {}", full_path);

        sftp.remove_file(&full_path).await
            .map_err(|e| ProviderError::ServerError(format!("Failed to delete file: {}", e)))?;

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Removing directory: {}", full_path);

        sftp.remove_dir(&full_path).await
            .map_err(|e| ProviderError::ServerError(format!("Failed to remove directory: {}", e)))?;

        Ok(())
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Recursively removing directory: {}", full_path);

        // List all entries
        let entries = self.list(&full_path).await?;

        // Delete all entries recursively (GAP-A02: skip symlinks to prevent following into target dirs)
        for entry in entries {
            if entry.is_symlink {
                self.delete(&entry.path).await?;
            } else if entry.is_dir {
                // Use Box::pin to avoid infinite recursion type issues
                Box::pin(self.rmdir_recursive(&entry.path)).await?;
            } else {
                self.delete(&entry.path).await?;
            }
        }

        // Now remove the empty directory
        self.rmdir(&full_path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let from_path = self.normalize_path(from);
        let to_path = self.normalize_path(to);

        tracing::info!("SFTP: Renaming {} to {}", from_path, to_path);

        sftp.rename(&from_path, &to_path).await
            .map_err(|e| ProviderError::ServerError(format!("Failed to rename: {}", e)))?;

        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        let metadata = sftp.metadata(&full_path).await
            .map_err(|e| ProviderError::NotFound(format!("File not found: {}", e)))?;

        let name = Path::new(&full_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| full_path.clone());

        let mut entry = self.metadata_to_entry(name, full_path.clone(), &metadata);

        // Check for symlink
        if let Ok(link_meta) = sftp.symlink_metadata(&full_path).await {
            if let Some(perms) = link_meta.permissions {
                if (perms & 0o170000) == 0o120000 {
                    entry.is_symlink = true;
                    if let Ok(target) = sftp.read_link(&full_path).await {
                        entry.link_target = Some(target);
                    }
                }
            }
        }

        Ok(entry)
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        let metadata = sftp.metadata(&full_path).await
            .map_err(|e| ProviderError::NotFound(format!("File not found: {}", e)))?;

        Ok(metadata.size.unwrap_or(0))
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        match sftp.try_exists(&full_path).await {
            Ok(exists) => Ok(exists),
            Err(_) => Ok(false),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // SFTP over SSH is a persistent connection
        // Just check if we're still connected
        if self.sftp.is_none() {
            return Err(ProviderError::NotConnected);
        }

        // Optionally do a simple operation to verify connection
        // canonicalize(".") is lightweight
        if let Some(sftp) = &self.sftp {
            sftp.canonicalize(".").await
                .map_err(|_| ProviderError::NotConnected)?;
        }

        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "SFTP Server: {}:{} (user: {}, home: {})",
            self.config.host, self.config.port, self.config.username, self.home_dir
        ))
    }

    fn supports_chmod(&self) -> bool {
        true // SFTP supports chmod
    }

    async fn chmod(&mut self, path: &str, mode: u32) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: chmod {} to {:o}", full_path, mode);

        let attrs = russh_sftp::protocol::FileAttributes { permissions: Some(mode), ..Default::default() };

        sftp.set_metadata(&full_path, attrs).await
            .map_err(|e| ProviderError::ServerError(format!("Failed to chmod: {}", e)))?;

        Ok(())
    }

    fn supports_symlinks(&self) -> bool {
        true // SFTP supports symlinks
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let sftp = self.get_sftp()?;
        let root = self.normalize_path(path);
        let pattern_lower = pattern.to_lowercase();
        let mut results = Vec::new();
        let mut dirs_to_scan = vec![root];

        while let Some(dir) = dirs_to_scan.pop() {
            let entries = match sftp.read_dir(&dir).await {
                Ok(e) => e,
                Err(_) => continue, // Skip inaccessible directories
            };

            for entry in entries {
                let name = entry.file_name();
                if name == "." || name == ".." {
                    continue;
                }

                let entry_path = if dir == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", dir.trim_end_matches('/'), name)
                };

                let remote_entry = self.metadata_to_entry(name.clone(), entry_path.clone(), &entry.metadata());

                if remote_entry.is_dir {
                    dirs_to_scan.push(entry_path.clone());
                }

                if name.to_lowercase().contains(&pattern_lower) {
                    results.push(remote_entry);
                    if results.len() >= 500 {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    async fn storage_info(&mut self) -> Result<super::StorageInfo, ProviderError> {
        let sftp = self.get_sftp()?;
        let path = self.normalize_path(".");

        let stat = sftp.fs_info(path).await
            .map_err(|e| ProviderError::ServerError(format!("statvfs failed: {}", e)))?
            .ok_or_else(|| ProviderError::NotSupported("Server does not support statvfs".to_string()))?;

        let total = stat.blocks * stat.fragment_size;
        let free = stat.blocks_avail * stat.fragment_size;
        let used = total.saturating_sub(free);

        Ok(super::StorageInfo { used, total, free })
    }

    async fn set_speed_limit(&mut self, upload_kb: u64, download_kb: u64) -> Result<(), ProviderError> {
        self.upload_limit_bps = upload_kb * 1024;
        self.download_limit_bps = download_kb * 1024;
        tracing::info!("SFTP: Speed limits set: download={}KB/s upload={}KB/s", download_kb, upload_kb);
        Ok(())
    }

    async fn get_speed_limit(&mut self) -> Result<(u64, u64), ProviderError> {
        Ok((self.upload_limit_bps / 1024, self.download_limit_bps / 1024))
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_resume_download: true,
            supports_resume_upload: true,
            supports_compression: true,
            supports_delta_sync: true,
            ..Default::default()
        }
    }

    fn supports_delta_sync(&self) -> bool {
        true
    }

    async fn read_range(&mut self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>, ProviderError> {
        let sftp = self.sftp.as_ref()
            .ok_or_else(|| ProviderError::NotConnected)?;
        let full_path = self.normalize_path(path);

        let mut file = sftp.open(&full_path).await
            .map_err(|e| ProviderError::ServerError(format!("Failed to open file for range read: {}", e)))?;

        // Seek to offset
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        file.seek(std::io::SeekFrom::Start(offset)).await
            .map_err(|e| ProviderError::ServerError(format!("Failed to seek: {}", e)))?;

        // GAP-A03: Cap read_range allocation to prevent attacker-controlled OOM
        const MAX_READ_RANGE: u64 = 100 * 1024 * 1024; // 100 MB
        if len > MAX_READ_RANGE {
            return Err(ProviderError::Other(
                format!("Read range size {} exceeds maximum {} bytes", len, MAX_READ_RANGE)
            ));
        }

        // Read exact len bytes
        let mut buf = vec![0u8; len as usize];
        let mut total_read = 0usize;
        while total_read < len as usize {
            let n = file.read(&mut buf[total_read..]).await
                .map_err(|e| ProviderError::ServerError(format!("Failed to read range: {}", e)))?;
            if n == 0 { break; }
            total_read += n;
        }
        buf.truncate(total_read);
        Ok(buf)
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
            password: Some(secrecy::SecretString::from("testpass".to_string())),
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
        provider.home_dir = "/home/user".to_string();

        assert_eq!(provider.normalize_path("/absolute"), "/absolute");
        assert_eq!(provider.normalize_path("relative"), "/home/user/relative");
        assert_eq!(provider.normalize_path(".."), "/home");
        assert_eq!(provider.normalize_path("."), "/home/user");
        assert_eq!(provider.normalize_path("~"), "/home/user");
        assert_eq!(provider.normalize_path("~/documents"), "/home/user/documents");
    }

    #[test]
    fn test_format_permissions() {
        assert_eq!(format_permissions(0o755, true), "drwxr-xr-x");
        assert_eq!(format_permissions(0o644, false), "-rw-r--r--");
        assert_eq!(format_permissions(0o777, true), "drwxrwxrwx");
        assert_eq!(format_permissions(0o600, false), "-rw-------");
    }
}
