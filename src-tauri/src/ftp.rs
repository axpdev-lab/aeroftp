//! FTP Manager - Handles FTP connections and file operations
//! 
//! This module provides an async wrapper around the suppaftp crate.

use anyhow::{Context, Result};
use suppaftp::tokio::AsyncFtpStream;
use suppaftp::types::FileType;
use thiserror::Error;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{debug, info, warn};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use secrecy::{ExposeSecret, SecretString};

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum FtpManagerError {
    #[error("FTP connection error: {0}")]
    ConnectionError(String),
    
    #[error("Not connected to server")]
    NotConnected,
    
    #[error("Invalid path: {0}")]
    InvalidPath(String),
    
    #[error("Operation failed: {0}")]
    OperationFailed(String),
    
    #[error("Timeout occurred")]
    Timeout,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteFile {
    pub name: String,
    pub path: String,
    pub size: Option<u64>,
    pub is_dir: bool,
    pub modified: Option<String>,
    pub permissions: Option<String>,
}

pub struct FtpManager {
    stream: Option<AsyncFtpStream>,
    current_path: String,
    server: Option<String>,
    username: Option<String>,
    password: Option<SecretString>,  // Stored for auto-reconnect, zeroized on drop
}

#[allow(dead_code)]
impl FtpManager {
    pub fn new() -> Self {
        Self {
            stream: None,
            current_path: "/".to_string(),
            server: None,
            username: None,
            password: None,
        }
    }

    /// Connect to FTP server
    pub async fn connect(&mut self, server: &str) -> Result<()> {
        info!("Connecting to FTP server: {}", server);
        
        // Parse server address
        let server_addr = if server.contains(':') {
            server.to_string()
        } else {
            format!("{}:21", server)
        };

        // Connect with timeout
        let stream = tokio::time::timeout(
            Duration::from_secs(10),
            AsyncFtpStream::connect(&server_addr)
        )
        .await
        .context("Connection timeout")?
        .map_err(|e| FtpManagerError::ConnectionError(e.to_string()))?;
        
        self.stream = Some(stream);
        self.server = Some(server_addr.clone());
        info!("Successfully connected to {}", server_addr);
        
        Ok(())
    }

    /// Login to FTP server
    pub async fn login(&mut self, username: &str, password: &str) -> Result<()> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;
        
        info!("Logging in as {}", username);
        
        stream.login(username, password)
            .await
            .map_err(|e| FtpManagerError::OperationFailed(format!("Login failed: {}", e)))?;
        
        self.username = Some(username.to_string());
        self.password = Some(SecretString::from(password.to_string()));  // Store for auto-reconnect
        info!("Successfully logged in as {}", username);
        
        // Get current working directory after login
        self.current_path = self.pwd().await.unwrap_or_else(|_| "/".to_string());
        
        Ok(())
    }

    /// Disconnect from FTP server
    pub async fn disconnect(&mut self) -> Result<()> {
        if let Some(mut stream) = self.stream.take() {
            info!("Disconnecting from FTP server");
            
            // Send QUIT command with timeout
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                stream.quit()
            )
            .await;
            
            match result {
                Ok(Ok(_)) => info!("Successfully disconnected"),
                Ok(Err(e)) => warn!("Error during disconnect: {:?}", e),
                Err(_) => warn!("Disconnect timeout"),
            }
            
            self.stream = None;
            self.server = None;
            self.username = None;
        }
        Ok(())
    }

    /// List files in current directory
    pub async fn list_files(&mut self) -> Result<Vec<RemoteFile>> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;

        debug!("Listing files in: {}", self.current_path);

        // Send NOOP before LIST to drain any pending control-connection responses
        // from a previous large transfer/listing. This prevents stale data from
        // leaking into the next LIST response (phantom files bug).
        if let Err(e) = stream.noop().await {
            debug!("Pre-LIST NOOP failed (non-fatal): {}", e);
        }

        // Use LIST without explicit path argument — relies on CWD already being set.
        // Passing the path explicitly (LIST /path/with spaces/#chars) causes FTP servers
        // to misinterpret paths containing #, spaces, or other special characters.
        let files = tokio::time::timeout(
            Duration::from_secs(30),
            stream.list(None)
        )
        .await
        .context("List operation timeout")?
        .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;

        let mut remote_files = Vec::new();

        for file_str in &files {
            let trimmed = file_str.trim();
            // Skip non-listing lines that FTP servers may include
            if trimmed.is_empty()
                || trimmed.starts_with("total ")
                || trimmed.starts_with("Total ")
            {
                continue;
            }
            // Skip directory header lines (e.g. "/path/to/dir:" from recursive LIST)
            if trimmed.ends_with(':') && !trimmed.contains(' ') {
                debug!("Skipping directory header line: {}", trimmed);
                continue;
            }
            if let Ok(file) = self.parse_ftp_listing(trimmed) {
                // Skip . and .. for cleaner UX - use "Up" button for navigation
                if file.name == "." || file.name == ".." {
                    continue;
                }
                remote_files.push(file);
            }
        }
        
        // Sort: directories first, then files, both alphabetically
        remote_files.sort_by(|a, b| {
            match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
        });
        
        Ok(remote_files)
    }

    /// Change working directory
    pub async fn change_dir(&mut self, path: &str) -> Result<()> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;
        
        info!("Changing directory to: {}", path);
        
        // If path is absolute, use it directly
        // If path is relative (like ".."), let the server handle it
        tokio::time::timeout(
            Duration::from_secs(10),
            stream.cwd(path)
        )
        .await
        .context("Change directory timeout")?
        .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        // Get the actual current directory from the server using PWD
        let pwd = tokio::time::timeout(
            Duration::from_secs(5),
            stream.pwd()
        )
        .await
        .context("PWD timeout")?
        .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        self.current_path = pwd;
        info!("Changed directory to: {}", self.current_path);
        
        Ok(())
    }

    /// Go to parent directory
    pub async fn go_up(&mut self) -> Result<()> {
        if self.current_path != "/" {
            self.change_dir("..").await?;
        }
        Ok(())
    }

    /// Get current working directory
    pub async fn pwd(&mut self) -> Result<String> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;
        
        let path = tokio::time::timeout(
            Duration::from_secs(5),
            stream.pwd()
        )
        .await
        .context("PWD timeout")?
        .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        Ok(path)
    }

    /// Get file size
    pub async fn get_file_size(&mut self, path: &str) -> Result<u64> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;
        
        match stream.size(path).await {
            Ok(size) => Ok(size as u64),
            Err(_) => Ok(0), // Return 0 if size cannot be determined
        }
    }

    /// Download a file (legacy, without progress)
    pub async fn download_file(&mut self, remote_path: &str, local_path: &str) -> Result<()> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;
        
        info!("Downloading: {} -> {}", remote_path, local_path);
        
        // Set binary transfer mode
        stream.transfer_type(FileType::Binary)
            .await
            .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        // Create local directory if needed
        if let Some(parent) = PathBuf::from(local_path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        
        // Download using retr_as_stream
        let mut data_stream = stream.retr_as_stream(remote_path)
            .await
            .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        // Read all data
        let mut buf = Vec::new();
        data_stream.read_to_end(&mut buf).await?;
        
        // Finalize the stream
        stream.finalize_retr_stream(data_stream)
            .await
            .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        // Write to local file
        tokio::fs::write(local_path, buf).await?;
        
        info!("Download completed: {}", remote_path);
        Ok(())
    }

    /// Download a file to memory (returns bytes directly).
    /// H2: Capped at 500 MB to prevent OOM — use download() for larger files.
    pub async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>> {
        const LIMIT: u64 = 500 * 1024 * 1024; // 500 MB

        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;

        info!("Downloading to memory: {}", remote_path);

        // Check file size first if server supports SIZE command
        if let Ok(size) = stream.size(remote_path).await {
            if size as u64 > LIMIT {
                return Err(FtpManagerError::OperationFailed(format!(
                    "File too large for in-memory download ({:.1} MB). Max: {:.0} MB.",
                    size as f64 / 1_048_576.0,
                    LIMIT as f64 / 1_048_576.0,
                )).into());
            }
        }

        // Set binary transfer mode
        stream.transfer_type(FileType::Binary)
            .await
            .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;

        // Download using retr_as_stream
        let mut data_stream = stream.retr_as_stream(remote_path)
            .await
            .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;

        // H2: Read with size cap to prevent OOM
        let mut buf = Vec::new();
        let limit_usize = (LIMIT + 1) as usize;
        loop {
            let mut chunk = [0u8; 8192];
            use tokio::io::AsyncReadExt;
            let n = data_stream.read(&mut chunk).await
                .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
            if n == 0 { break; }
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > limit_usize { break; }
        }
        let bytes_read = buf.len();

        // Finalize the stream
        stream.finalize_retr_stream(data_stream)
            .await
            .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;

        if bytes_read as u64 > LIMIT {
            return Err(FtpManagerError::OperationFailed(format!(
                "Download exceeded {:.0} MB size limit.",
                LIMIT as f64 / 1_048_576.0,
            )).into());
        }

        info!("Downloaded {} bytes from: {}", buf.len(), remote_path);
        Ok(buf)
    }

    /// Download a file with progress callback
    /// The callback returns `true` to continue or `false` to cancel the transfer.
    pub async fn download_file_with_progress<F>(
        &mut self,
        remote_path: &str,
        local_path: &str,
        mut on_progress: F
    ) -> Result<()>
    where
        F: FnMut(u64) -> bool
    {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;

        info!("Downloading with progress: {} -> {}", remote_path, local_path);

        // Set binary transfer mode
        stream.transfer_type(FileType::Binary)
            .await
            .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;

        // Create local directory if needed
        if let Some(parent) = PathBuf::from(local_path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Download using retr_as_stream
        let mut data_stream = stream.retr_as_stream(remote_path)
            .await
            .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;

        // Stream directly to disk in 8KB chunks (no full-file RAM buffering)
        let mut local_file = tokio::fs::File::create(local_path).await?;
        let mut chunk = [0u8; 8192];
        let mut total_read: u64 = 0;
        let mut cancelled = false;

        loop {
            let n = data_stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            local_file.write_all(&chunk[..n]).await?;
            total_read += n as u64;
            if !on_progress(total_read) {
                cancelled = true;
                info!("Download cancelled by user at {} bytes: {}", total_read, remote_path);
                break;
            }
        }

        local_file.flush().await?;

        // Finalize the stream (must always finalize to keep FTP connection clean)
        // On cancel, FTP server sends 426 — ignore that error
        let finalize_result = stream.finalize_retr_stream(data_stream).await;
        if cancelled {
            let _ = finalize_result; // Ignore 426 error on cancel
            let _ = tokio::fs::remove_file(local_path).await;
            return Err(FtpManagerError::OperationFailed("Transfer cancelled by user".to_string()).into());
        }
        finalize_result.map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;

        info!("Download completed: {} ({} bytes)", remote_path, total_read);
        Ok(())
    }

    /// Upload a file (legacy, without progress)
    pub async fn upload_file(&mut self, local_path: &str, remote_path: &str) -> Result<()> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;
        
        info!("Uploading: {} -> {}", local_path, remote_path);
        
        // Set binary transfer mode
        stream.transfer_type(FileType::Binary)
            .await
            .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        // Read local file
        let data = tokio::fs::read(local_path).await?;
        let mut cursor = std::io::Cursor::new(data);
        
        // Upload with timeout
        tokio::time::timeout(
            Duration::from_secs(300),
            stream.put_file(remote_path, &mut cursor)
        )
        .await
        .context("Upload timeout")?
        .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        info!("Upload completed: {}", remote_path);
        Ok(())
    }

    /// Upload a file with progress callback (streaming, chunked)
    /// The callback returns `true` to continue or `false` to cancel the transfer.
    pub async fn upload_file_with_progress<F>(
        &mut self,
        local_path: &str,
        remote_path: &str,
        _file_size: u64,
        mut on_progress: F
    ) -> Result<()>
    where
        F: FnMut(u64) -> bool
    {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;

        info!("Uploading with progress: {} -> {}", local_path, remote_path);

        // Set binary transfer mode
        stream.transfer_type(FileType::Binary)
            .await
            .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;

        // Open local file for streaming read
        let mut file = tokio::fs::File::open(local_path).await?;
        let file_size = file.metadata().await?.len();

        // Report initial progress
        on_progress(0);

        // Open streaming upload channel
        let mut data_stream = stream.put_with_stream(remote_path)
            .await
            .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;

        // Write in 8KB chunks with progress tracking
        let mut chunk = [0u8; 8192];
        let mut total_written: u64 = 0;
        let mut cancelled = false;

        loop {
            let n = file.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            data_stream.write_all(&chunk[..n])
                .await
                .map_err(|e| FtpManagerError::OperationFailed(format!("Write error: {}", e)))?;
            total_written += n as u64;
            if !on_progress(total_written) {
                cancelled = true;
                info!("Upload cancelled by user at {} of {} bytes: {}", total_written, file_size, remote_path);
                break;
            }
        }

        // Finalize the stream (must always finalize to keep FTP connection clean)
        let finalize_result = stream.finalize_put_stream(data_stream).await;
        if cancelled {
            let _ = finalize_result; // Ignore error on cancel
            // Try to remove the partial remote file
            let _ = stream.rm(remote_path).await;
            return Err(FtpManagerError::OperationFailed("Transfer cancelled by user".to_string()).into());
        }
        finalize_result.map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;

        info!("Upload completed: {} ({} bytes)", remote_path, total_written);
        Ok(())
    }

    /// Create a directory
    pub async fn mkdir(&mut self, path: &str) -> Result<()> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;
        
        info!("Creating directory: {}", path);
        
        tokio::time::timeout(
            Duration::from_secs(10),
            stream.mkdir(path)
        )
        .await
        .context("MKDIR timeout")?
        .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        Ok(())
    }

    /// Remove a file
    pub async fn remove(&mut self, path: &str) -> Result<()> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;
        
        info!("Removing: {}", path);
        
        tokio::time::timeout(
            Duration::from_secs(10),
            stream.rm(path)
        )
        .await
        .context("Remove timeout")?
        .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        Ok(())
    }

    /// Remove a directory
    pub async fn remove_dir(&mut self, path: &str) -> Result<()> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;
        
        info!("Removing directory: {}", path);
        
        tokio::time::timeout(
            Duration::from_secs(10),
            stream.rmdir(path)
        )
        .await
        .context("Remove directory timeout")?
        .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        Ok(())
    }

    /// Delete a folder recursively (with all contents) - iterative approach
    pub async fn delete_folder_recursive(&mut self, path: &str) -> Result<()> {
        let _ = self.stream.as_ref()
            .ok_or(FtpManagerError::NotConnected)?;

        info!("Deleting folder recursively: {}", path);

        let original_path = self.current_path.clone();

        let target_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{}", original_path, path)
        };

        if let Err(e) = self.change_dir(&target_path).await {
            warn!("Cannot access directory {}: {}", target_path, e);
            self.current_path = original_path;
            return Err(e);
        }

        let mut dirs_to_process: Vec<String> = vec![target_path.clone()];

        while let Some(current_dir) = dirs_to_process.pop() {
            if let Err(e) = self.change_dir(&current_dir).await {
                warn!("Cannot navigate to directory {}: {}", current_dir, e);
                continue;
            }

            let files = match self.list_files().await {
                Ok(f) => f,
                Err(e) => {
                    warn!("Cannot list files in {}: {}", current_dir, e);
                    continue;
                }
            };

            let mut sub_dirs: Vec<String> = Vec::new();

            for file in files {
                let file_path = format!("{}/{}", current_dir, file.name);

                if file.is_dir {
                    sub_dirs.push(file_path.clone());
                } else if let Err(e) = self.remove(&file_path).await {
                    warn!("Failed to delete file {}: {}", file_path, e);
                }
            }

            for sub_dir in sub_dirs {
                dirs_to_process.push(sub_dir);
            }

            if let Err(e) = self.change_dir("..").await {
                warn!("Cannot navigate to parent directory: {}", e);
            }

            if let Err(e) = self.remove_dir(&current_dir).await {
                debug!("Could not delete directory {} (may have subdirs): {}", current_dir, e);
            }
        }

        let _ = self.change_dir(&original_path).await;

        let _ = self.remove_dir(&target_path).await;

        info!("Folder deleted recursively: {}", target_path);
        Ok(())
    }

    /// Rename a file or directory
    pub async fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;
        
        info!("Renaming: {} -> {}", from, to);
        
        tokio::time::timeout(
            Duration::from_secs(10),
            stream.rename(from, to)
        )
        .await
        .context("Rename timeout")?
        .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        Ok(())
    }

    /// Change permissions (CHMOD)
    pub async fn chmod(&mut self, path: &str, mode: &str) -> Result<()> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;
        
        info!("Changing permissions: {} -> {}", path, mode);
        
        // Use SITE CHMOD command
        let args = format!("CHMOD {} {}", mode, path);
        
        tokio::time::timeout(
            Duration::from_secs(10),
            stream.site(&args)
        )
        .await
        .context("CHMOD timeout")?
        .map_err(|e| FtpManagerError::OperationFailed(e.to_string()))?;
        
        Ok(())
    }

    /// Get server information
    pub async fn server_info(&mut self) -> Result<String> {
        Ok("FTP Server".to_string())
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// Send NOOP command to keep connection alive
    pub async fn noop(&mut self) -> Result<()> {
        let stream = self.stream.as_mut()
            .ok_or(FtpManagerError::NotConnected)?;
        
        stream.noop()
            .await
            .map_err(|e| FtpManagerError::OperationFailed(format!("NOOP failed: {}", e)))?;
        
        Ok(())
    }

    /// Attempt to reconnect using stored credentials
    pub async fn reconnect(&mut self) -> Result<()> {
        let server = self.server.clone()
            .ok_or(FtpManagerError::NotConnected)?;
        let username = self.username.clone()
            .ok_or(FtpManagerError::NotConnected)?;
        let password = self.password.as_ref()
            .ok_or(FtpManagerError::NotConnected)?;
        let password_exposed = password.expose_secret().to_string();

        info!("Attempting to reconnect to {}", server);

        // Disconnect existing stream if any
        if self.stream.is_some() {
            let _ = self.disconnect().await;
        }

        // Reconnect
        self.connect(&server).await?;
        self.login(&username, &password_exposed).await?;
        
        info!("Successfully reconnected to {}", server);
        Ok(())
    }

    /// Get current path
    pub fn current_path(&self) -> String {
        self.current_path.clone()
    }

    /// Get server info string
    pub fn server_info_string(&self) -> String {
        match (&self.server, &self.username) {
            (Some(server), Some(username)) => format!("{}@{}", username, server),
            (Some(server), None) => server.clone(),
            _ => "Not connected".to_string(),
        }
    }

    /// Parse FTP listing string into RemoteFile
    fn parse_ftp_listing(&self, listing: &str) -> Result<RemoteFile> {
        if listing.trim().is_empty() {
            return Err(FtpManagerError::InvalidPath("Empty listing".to_string()).into());
        }

        debug!("Parsing FTP listing: {}", listing);

        // Try to parse Unix-style listing first
        if let Some(file) = self.parse_unix_listing(listing) {
            debug!("Parsed as Unix: {} (is_dir: {})", file.name, file.is_dir);
            return Ok(file);
        }

        // Try to parse DOS-style listing
        if let Some(file) = self.parse_dos_listing(listing) {
            debug!("Parsed as DOS: {} (is_dir: {})", file.name, file.is_dir);
            return Ok(file);
        }

        // Fallback: treat as simple filename
        // Check if it might be a directory (no extension, common dir indicators)
        let name = listing.trim().to_string();
        let is_likely_dir = !name.contains('.') || name == "." || name == "..";
        
        debug!("Fallback parsing: {} (guessed is_dir: {})", name, is_likely_dir);
        
        let path = if self.current_path.ends_with('/') {
            format!("{}{}", self.current_path, name)
        } else {
            format!("{}/{}", self.current_path, name)
        };

        Ok(RemoteFile {
            name,
            path,
            size: None,
            is_dir: is_likely_dir,
            modified: None,
            permissions: None,
        })
    }

    fn parse_unix_listing(&self, listing: &str) -> Option<RemoteFile> {
        let parts: Vec<&str> = listing.split_whitespace().collect();
        if parts.len() < 9 {
            return None;
        }

        let permissions = parts[0];
        let is_dir = permissions.starts_with('d');
        let is_symlink = permissions.starts_with('l');
        
        // Join all parts from index 8 onwards to handle filenames with spaces
        // Unix listing format: permissions links owner group size month day time/year name...
        let name = parts[8..].join(" ");
        let size = parts.get(4).and_then(|s| s.parse().ok());
        
        let modified = if parts.len() >= 8 {
            Some(format!("{} {} {}", parts[5], parts[6], parts[7]))
        } else {
            None
        };

        let path = if self.current_path.ends_with('/') {
            format!("{}{}", self.current_path, name)
        } else {
            format!("{}/{}", self.current_path, name)
        };

        let actual_name = if is_symlink && name.contains(" -> ") {
            name.split(" -> ").next()?.to_string()
        } else {
            name
        };

        Some(RemoteFile {
            name: actual_name,
            path,
            size,
            is_dir,
            modified,
            permissions: Some(permissions.to_string()),
        })
    }

    fn parse_dos_listing(&self, listing: &str) -> Option<RemoteFile> {
        let parts: Vec<&str> = listing.split_whitespace().collect();
        if parts.len() < 4 {
            return None;
        }

        let is_dir = parts.contains(&"<DIR>");
        let size = if is_dir {
            None
        } else {
            parts.get(2).and_then(|s| s.parse().ok())
        };

        // DOS format: date time <DIR>/size name...
        // Find the filename start (after date, time, and either <DIR> or size)
        let name_start_idx = 3;
        let name = if parts.len() > name_start_idx {
            parts[name_start_idx..].join(" ")
        } else {
            parts.last()?.to_string()
        };

        let path = if self.current_path.ends_with('/') {
            format!("{}{}", self.current_path, name)
        } else {
            format!("{}/{}", self.current_path, name)
        };

        Some(RemoteFile {
            name,
            path,
            size,
            is_dir,
            modified: Some(format!("{} {}", parts[0], parts[1])),
            permissions: None,
        })
    }
}

impl Default for FtpManager {
    fn default() -> Self {
        Self::new()
    }
}