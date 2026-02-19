//! FTP Storage Provider
//!
//! Implementation of the StorageProvider trait for FTP and FTPS protocols.
//! Uses the suppaftp crate for FTP operations.

use async_trait::async_trait;
use suppaftp::tokio::{AsyncNativeTlsConnector, AsyncNativeTlsFtpStream};
use suppaftp::types::FileType;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    StorageProvider, ProviderError, ProviderType, RemoteEntry, FtpConfig,
    FtpTlsMode,
};

/// FTP/FTPS Storage Provider
pub struct FtpProvider {
    config: FtpConfig,
    stream: Option<AsyncNativeTlsFtpStream>,
    current_path: String,
    /// Whether server supports MLSD/MLST (RFC 3659)
    mlsd_supported: bool,
    /// Set to true if ExplicitIfAvailable mode fell back to plaintext
    pub tls_downgraded: bool,
}

impl FtpProvider {
    /// Create a new FTP provider with the given configuration
    pub fn new(config: FtpConfig) -> Self {
        Self {
            config,
            stream: None,
            current_path: "/".to_string(),
            mlsd_supported: false,
            tls_downgraded: false,
        }
    }
    
    /// Get mutable reference to the FTP stream, returning error if not connected
    fn stream_mut(&mut self) -> Result<&mut AsyncNativeTlsFtpStream, ProviderError> {
        self.stream.as_mut().ok_or(ProviderError::NotConnected)
    }

    /// Create a TLS connector with the configured certificate verification settings
    fn make_tls_connector(&self) -> Result<AsyncNativeTlsConnector, ProviderError> {
        let mut builder = native_tls::TlsConnector::builder();
        if !self.config.verify_cert {
            builder.danger_accept_invalid_certs(true);
            builder.danger_accept_invalid_hostnames(true);
        }
        let connector = suppaftp::async_native_tls::TlsConnector::from(builder);
        Ok(AsyncNativeTlsConnector::from(connector))
    }
    
    /// Parse FTP listing into RemoteEntry
    fn parse_listing(&self, line: &str) -> Option<RemoteEntry> {
        // Try Unix format first, then DOS format
        self.parse_unix_listing(line)
            .or_else(|| self.parse_dos_listing(line))
    }
    
    /// Parse Unix-style listing (ls -l format)
    fn parse_unix_listing(&self, line: &str) -> Option<RemoteEntry> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 9 {
            return None;
        }
        
        let permissions = parts[0];
        let is_dir = permissions.starts_with('d');
        let is_symlink = permissions.starts_with('l');
        
        // Get size (might be in different position depending on format)
        let size: u64 = parts[4].parse().unwrap_or(0);
        
        // Name is everything after the 8th part (to handle spaces in names)
        let name = parts[8..].join(" ");
        
        // Handle symlinks (name -> target)
        let (actual_name, link_target) = if is_symlink && name.contains(" -> ") {
            let parts: Vec<&str> = name.splitn(2, " -> ").collect();
            (parts[0].to_string(), Some(parts.get(1).unwrap_or(&"").to_string()))
        } else {
            (name, None)
        };
        
        // Skip . and .. entries
        if actual_name == "." || actual_name == ".." {
            return None;
        }
        
        let path = if self.current_path.ends_with('/') {
            format!("{}{}", self.current_path, actual_name)
        } else {
            format!("{}/{}", self.current_path, actual_name)
        };
        
        // Parse date (parts[5..8] typically contain month day time/year)
        let modified = if parts.len() >= 8 {
            Some(format!("{} {} {}", parts[5], parts[6], parts[7]))
        } else {
            None
        };
        
        Some(RemoteEntry {
            name: actual_name,
            path,
            is_dir,
            size,
            modified,
            permissions: Some(permissions.to_string()),
            owner: Some(parts[2].to_string()),
            group: Some(parts[3].to_string()),
            is_symlink,
            link_target,
            mime_type: None,
            metadata: Default::default(),
        })
    }
    
    /// Parse DOS-style listing (Windows FTP servers)
    fn parse_dos_listing(&self, line: &str) -> Option<RemoteEntry> {
        // DOS format: 01-23-24  10:30AM       <DIR>          folder_name
        // Or:         01-23-24  10:30AM           12345      file.txt
        
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            return None;
        }
        
        let is_dir = parts[2] == "<DIR>";
        let size: u64 = if is_dir { 0 } else { parts[2].parse().unwrap_or(0) };
        let name = parts[3..].join(" ");
        
        // Skip . and .. entries
        if name == "." || name == ".." {
            return None;
        }
        
        let path = if self.current_path.ends_with('/') {
            format!("{}{}", self.current_path, name)
        } else {
            format!("{}/{}", self.current_path, name)
        };
        
        let modified = Some(format!("{} {}", parts[0], parts[1]));
        
        Some(RemoteEntry {
            name,
            path,
            is_dir,
            size,
            modified,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        })
    }

    /// Parse MLSD/MLST line (RFC 3659 machine-readable format)
    /// Format: "fact1=val1;fact2=val2; filename"
    fn parse_mlsd_entry(&self, line: &str, base_path: &str) -> Option<RemoteEntry> {
        // Split on first space after semicolons to get facts and filename
        let (facts_str, name) = line.split_once(' ')?;
        let name = name.to_string();

        if name == "." || name == ".." {
            return None;
        }

        let mut is_dir = false;
        let mut is_symlink = false;
        let mut size: u64 = 0;
        let mut modified: Option<String> = None;
        let mut permissions: Option<String> = None;
        let mut owner: Option<String> = None;
        let mut group: Option<String> = None;

        for fact in facts_str.split(';') {
            let fact = fact.trim();
            if fact.is_empty() {
                continue;
            }
            let (key, value) = match fact.split_once('=') {
                Some((k, v)) => (k.to_lowercase(), v),
                None => continue,
            };

            match key.as_str() {
                "type" => {
                    let v_lower = value.to_lowercase();
                    is_dir = v_lower == "dir" || v_lower == "cdir" || v_lower == "pdir";
                    is_symlink = v_lower == "os.unix=symlink" || v_lower == "os.unix=slink";
                }
                "size" | "sizd" => {
                    size = value.parse().unwrap_or(0);
                }
                "modify" => {
                    // YYYYMMDDHHMMSS[.sss] → format nicely
                    modified = Some(Self::format_mlsd_time(value));
                }
                "unix.mode" => {
                    permissions = Some(value.to_string());
                }
                "unix.owner" | "unix.uid" => {
                    owner = Some(value.to_string());
                }
                "unix.group" | "unix.gid" => {
                    group = Some(value.to_string());
                }
                "perm" => {
                    // MLSD perm facts (e.g. "rwcedf") - store as metadata
                    if permissions.is_none() {
                        permissions = Some(value.to_string());
                    }
                }
                _ => {}
            }
        }

        // Skip cdir/pdir (current/parent directory entries)
        if facts_str.to_lowercase().contains("type=cdir") || facts_str.to_lowercase().contains("type=pdir") {
            return None;
        }

        let path = if base_path.ends_with('/') {
            format!("{}{}", base_path, name)
        } else {
            format!("{}/{}", base_path, name)
        };

        Some(RemoteEntry {
            name,
            path,
            is_dir,
            size,
            modified,
            permissions,
            owner,
            group,
            is_symlink,
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        })
    }

    /// Format MLSD timestamp (YYYYMMDDHHMMSS) to readable form
    fn format_mlsd_time(ts: &str) -> String {
        if ts.len() >= 14 {
            format!(
                "{}-{}-{} {}:{}:{}",
                &ts[0..4], &ts[4..6], &ts[6..8],
                &ts[8..10], &ts[10..12], &ts[12..14]
            )
        } else if ts.len() >= 8 {
            format!("{}-{}-{}", &ts[0..4], &ts[4..6], &ts[6..8])
        } else {
            ts.to_string()
        }
    }
}

#[async_trait]
impl StorageProvider for FtpProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType {
        if self.config.tls_mode != FtpTlsMode::None {
            ProviderType::Ftps
        } else {
            ProviderType::Ftp
        }
    }
    
    fn display_name(&self) -> String {
        format!("{}@{}", self.config.username, self.config.host)
    }
    
    async fn connect(&mut self) -> Result<(), ProviderError> {
        let addr = format!("{}:{}", self.config.host, self.config.port);
        let domain = self.config.host.clone();

        // Connect and optionally upgrade to TLS based on tls_mode
        let mut stream = match self.config.tls_mode {
            FtpTlsMode::None => {
                // Plain FTP - no TLS
                AsyncNativeTlsFtpStream::connect(&addr)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?
            }
            FtpTlsMode::Explicit => {
                // Explicit TLS (AUTH TLS) - connect plain, then upgrade
                let stream = AsyncNativeTlsFtpStream::connect(&addr)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;
                let connector = self.make_tls_connector()?;
                stream.into_secure(connector, &domain)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(format!("TLS upgrade failed: {}", e)))?
            }
            FtpTlsMode::Implicit => {
                // Implicit TLS - connect then immediately upgrade (port 990)
                let stream = AsyncNativeTlsFtpStream::connect(&addr)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;
                let connector = self.make_tls_connector()?;
                stream.into_secure(connector, &domain)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(format!("Implicit TLS failed: {}", e)))?
            }
            FtpTlsMode::ExplicitIfAvailable => {
                // Try explicit TLS, fall back to plain
                let stream = AsyncNativeTlsFtpStream::connect(&addr)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;
                let connector = self.make_tls_connector()?;
                match stream.into_secure(connector, &domain).await {
                    Ok(secure) => {
                        self.tls_downgraded = false;
                        secure
                    }
                    Err(e) => {
                        // TLS not supported — fall back to plain with security warning
                        tracing::warn!(
                            "SECURITY: TLS upgrade failed for {}:{} ({}), falling back to PLAINTEXT FTP. \
                             Credentials will be sent unencrypted.",
                            self.config.host, self.config.port, e
                        );
                        self.tls_downgraded = true;
                        AsyncNativeTlsFtpStream::connect(&addr)
                            .await
                            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?
                    }
                }
            }
        };

        // Login
        use secrecy::ExposeSecret;
        let pwd = self.config.password.expose_secret();
        stream
            .login(self.config.username.as_str(), pwd)
            .await
            .map_err(|e| ProviderError::AuthenticationFailed(e.to_string()))?;

        // Set binary transfer mode
        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        // Navigate to initial path if specified
        if let Some(ref initial_path) = self.config.initial_path {
            if !initial_path.is_empty() {
                stream
                    .cwd(initial_path)
                    .await
                    .map_err(|e| ProviderError::InvalidPath(e.to_string()))?;
            }
        }

        // Check FEAT for MLSD support
        self.mlsd_supported = match stream.feat().await {
            Ok(features) => features.contains_key("MLST") || features.contains_key("MLSD"),
            Err(_) => false,
        };

        // Get current directory (normalize Windows backslashes from FTP servers)
        self.current_path = stream
            .pwd()
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?
            .replace('\\', "/");

        self.stream = Some(stream);
        Ok(())
    }
    
    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        if let Some(mut stream) = self.stream.take() {
            let _ = stream.quit().await;
        }
        Ok(())
    }
    
    fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
    
    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let list_path = if path.is_empty() || path == "." {
            None
        } else {
            Some(path.to_string())
        };

        let base_path = list_path.as_deref().unwrap_or(&self.current_path).to_string();

        // Prefer MLSD when supported
        if self.mlsd_supported {
            let stream = self.stream_mut()?;
            match stream.mlsd(list_path.as_deref()).await {
                Ok(lines) => {
                    let entries: Vec<RemoteEntry> = lines
                        .iter()
                        .filter_map(|line| self.parse_mlsd_entry(line, &base_path))
                        .collect();
                    return Ok(entries);
                }
                Err(_) => {
                    // Fall through to LIST
                }
            }
        }

        // Fallback to LIST
        let stream = self.stream_mut()?;
        let lines = stream
            .list(list_path.as_deref())
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        let entries: Vec<RemoteEntry> = lines
            .iter()
            .filter_map(|line| self.parse_listing(line))
            .collect();

        Ok(entries)
    }
    
    async fn pwd(&mut self) -> Result<String, ProviderError> {
        let stream = self.stream_mut()?;
        let path = stream
            .pwd()
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?
            .replace('\\', "/");
        self.current_path = path.clone();
        Ok(path)
    }
    
    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .cwd(path)
            .await
            .map_err(|e| ProviderError::InvalidPath(e.to_string()))?;
        
        self.current_path = stream
            .pwd()
            .await
            .unwrap_or_else(|_| path.to_string())
            .replace('\\', "/");

        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .cdup()
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        
        self.current_path = stream
            .pwd()
            .await
            .unwrap_or_else(|_| "/".to_string())
            .replace('\\', "/");

        Ok(())
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        
        // Get file size for progress
        let total_size = stream
            .size(remote_path)
            .await
            .unwrap_or(0) as u64;
        
        // Set binary mode
        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        
        // Download using retr_as_stream — stream directly to disk (no full-file RAM buffer)
        let mut data_stream = stream
            .retr_as_stream(remote_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let mut local_file = tokio::fs::File::create(local_path)
            .await
            .map_err(|e| ProviderError::IoError(e))?;

        let mut chunk = [0u8; 8192];
        let mut transferred: u64 = 0;

        loop {
            let n = data_stream
                .read(&mut chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            if n == 0 {
                break;
            }
            local_file
                .write_all(&chunk[..n])
                .await
                .map_err(|e| ProviderError::IoError(e))?;
            transferred += n as u64;

            if let Some(ref progress) = on_progress {
                progress(transferred, total_size);
            }
        }

        local_file.flush().await.map_err(|e| ProviderError::IoError(e))?;

        // Finalize the stream - need to get stream again after the borrow
        let stream = self.stream.as_mut().ok_or(ProviderError::NotConnected)?;
        stream
            .finalize_retr_stream(data_stream)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        
        Ok(())
    }
    
    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let stream = self.stream_mut()?;
        
        // Set binary mode
        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        
        // Download using retr_as_stream
        let mut data_stream = stream
            .retr_as_stream(remote_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        
        let mut data = Vec::new();
        data_stream
            .read_to_end(&mut data)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        
        // Finalize the stream
        let stream = self.stream.as_mut().ok_or(ProviderError::NotConnected)?;
        stream
            .finalize_retr_stream(data_stream)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        
        Ok(data)
    }
    
    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;

        // Stream from file instead of reading entire file into memory
        let total_size = tokio::fs::metadata(local_path).await
            .map_err(|e| ProviderError::IoError(e))?.len();

        let mut file = tokio::fs::File::open(local_path).await
            .map_err(|e| ProviderError::IoError(e))?;

        // Upload using AsyncRead
        stream
            .put_file(remote_path, &mut file)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        
        if let Some(progress) = on_progress {
            progress(total_size, total_size);
        }
        
        Ok(())
    }
    
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .mkdir(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        Ok(())
    }
    
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .rm(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        Ok(())
    }
    
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .rmdir(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        Ok(())
    }
    
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Get list of contents
        let entries = self.list(path).await?;
        
        // Delete contents first
        for entry in entries {
            if entry.is_dir {
                // Use Box::pin for recursive async call
                Box::pin(self.rmdir_recursive(&entry.path)).await?;
            } else {
                self.delete(&entry.path).await?;
            }
        }
        
        // Now delete the empty directory
        self.rmdir(path).await
    }
    
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .rename(from, to)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        Ok(())
    }
    
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        // Use MLST when available for direct single-file info
        if self.mlsd_supported {
            let stream = self.stream_mut()?;
            if let Ok(mlst_line) = stream.mlst(Some(path)).await {
                let parent = std::path::Path::new(path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "/".to_string());
                if let Some(entry) = self.parse_mlsd_entry(&mlst_line.trim(), &parent) {
                    return Ok(entry);
                }
            }
        }

        // Fallback: list parent and find the entry
        let parent = std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string());

        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| ProviderError::InvalidPath(path.to_string()))?;

        let entries = self.list(&parent).await?;

        entries
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| ProviderError::NotFound(path.to_string()))
    }
    
    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let stream = self.stream_mut()?;
        let size = stream
            .size(path)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        Ok(size as u64)
    }
    
    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
    
    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        stream
            .noop()
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        Ok(())
    }
    
    async fn server_info(&mut self) -> Result<String, ProviderError> {
        // FTP doesn't have a standard server info command
        // Return basic connection info
        Ok(format!(
            "FTP Server: {}:{}",
            self.config.host, self.config.port
        ))
    }
    
    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let pattern_lower = pattern.to_lowercase();
        let mut results = Vec::new();
        let search_path = if path.is_empty() || path == "." {
            self.current_path.clone()
        } else {
            path.to_string()
        };
        let mut dirs_to_scan = vec![search_path];

        while let Some(dir) = dirs_to_scan.pop() {
            // Save current_path, list, restore
            let saved = self.current_path.clone();
            self.current_path = dir.clone();
            let entries = match self.list(&dir).await {
                Ok(e) => e,
                Err(_) => {
                    self.current_path = saved;
                    continue;
                }
            };
            self.current_path = saved;

            for entry in entries {
                if entry.is_dir {
                    dirs_to_scan.push(entry.path.clone());
                }

                if entry.name.to_lowercase().contains(&pattern_lower) {
                    results.push(entry);
                    if results.len() >= 500 {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    fn supports_resume(&self) -> bool {
        true
    }

    async fn resume_download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;

        // Get total file size
        let total_size = stream
            .size(remote_path)
            .await
            .unwrap_or(0) as u64;

        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        // Send REST command to set offset
        stream
            .resume_transfer(offset as usize)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("REST failed: {}", e)))?;

        // Now retrieve from offset
        let mut data_stream = stream
            .retr_as_stream(remote_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let mut data = Vec::new();
        data_stream
            .read_to_end(&mut data)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let stream = self.stream.as_mut().ok_or(ProviderError::NotConnected)?;
        stream
            .finalize_retr_stream(data_stream)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if let Some(progress) = on_progress {
            progress(offset + data.len() as u64, total_size);
        }

        // Append to existing local file or create new one
        use tokio::io::AsyncWriteExt as _;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(false)
            .open(local_path)
            .await
            .map_err(|e| ProviderError::IoError(e))?;

        // Seek to offset and write
        file.set_len(offset)
            .await
            .map_err(|e| ProviderError::IoError(e))?;
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| ProviderError::IoError(e))?;
        file.write_all(&data)
            .await
            .map_err(|e| ProviderError::IoError(e))?;

        Ok(())
    }

    async fn resume_upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::AsyncSeekExt;

        let total_size = tokio::fs::metadata(local_path).await
            .map_err(|e| ProviderError::IoError(e))?.len();

        if offset >= total_size {
            return Ok(()); // Nothing to upload
        }

        // Open file and seek to offset for streaming append
        let mut file = tokio::fs::File::open(local_path).await
            .map_err(|e| ProviderError::IoError(e))?;
        file.seek(std::io::SeekFrom::Start(offset)).await
            .map_err(|e| ProviderError::IoError(e))?;

        let stream = self.stream_mut()?;
        stream
            .transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        stream
            .append_file(remote_path, &mut file)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if let Some(progress) = on_progress {
            progress(total_size, total_size);
        }

        Ok(())
    }

    fn supports_chmod(&self) -> bool {
        true
    }
    
    async fn chmod(&mut self, path: &str, mode: u32) -> Result<(), ProviderError> {
        let stream = self.stream_mut()?;
        
        // SITE CHMOD command
        let chmod_cmd = format!("CHMOD {:o} {}", mode, path);
        stream
            .site(&chmod_cmd)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;
        
        Ok(())
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_resume_download: true,
            supports_resume_upload: true,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_unix_listing() {
        let provider = FtpProvider::new(FtpConfig {
            host: "test".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });
        
        let line = "drwxr-xr-x    2 user     group        4096 Jan 20 10:00 projects";
        let entry = provider.parse_unix_listing(line).unwrap();
        
        assert_eq!(entry.name, "projects");
        assert!(entry.is_dir);
        assert_eq!(entry.size, 4096);
    }
    
    #[test]
    fn test_parse_mlsd_entry() {
        let provider = FtpProvider::new(FtpConfig {
            host: "test".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });

        let line = "type=file;size=12345;modify=20260131120000;unix.mode=0644; readme.txt";
        let entry = provider.parse_mlsd_entry(line, "/home").unwrap();

        assert_eq!(entry.name, "readme.txt");
        assert!(!entry.is_dir);
        assert_eq!(entry.size, 12345);
        assert_eq!(entry.modified.as_deref(), Some("2026-01-31 12:00:00"));
        assert_eq!(entry.permissions.as_deref(), Some("0644"));
        assert_eq!(entry.path, "/home/readme.txt");
    }

    #[test]
    fn test_parse_mlsd_directory() {
        let provider = FtpProvider::new(FtpConfig {
            host: "test".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });

        let line = "type=dir;modify=20260115080000; projects";
        let entry = provider.parse_mlsd_entry(line, "/").unwrap();

        assert_eq!(entry.name, "projects");
        assert!(entry.is_dir);
        assert_eq!(entry.path, "/projects");
    }

    #[test]
    fn test_parse_mlsd_skips_cdir_pdir() {
        let provider = FtpProvider::new(FtpConfig {
            host: "test".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });

        assert!(provider.parse_mlsd_entry("type=cdir;modify=20260101000000; .", "/").is_none());
        assert!(provider.parse_mlsd_entry("type=pdir;modify=20260101000000; ..", "/").is_none());
    }

    #[test]
    fn test_parse_dos_listing() {
        let provider = FtpProvider::new(FtpConfig {
            host: "test".to_string(),
            port: 21,
            username: "user".to_string(),
            password: "pass".to_string().into(),
            tls_mode: FtpTlsMode::None,
            verify_cert: true,
            initial_path: None,
        });
        
        let line = "01-20-26  10:00AM       <DIR>          Projects";
        let entry = provider.parse_dos_listing(line).unwrap();
        
        assert_eq!(entry.name, "Projects");
        assert!(entry.is_dir);
    }
}
