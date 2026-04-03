//! FTP Storage Provider
//!
//! Implementation of the StorageProvider trait for FTP and FTPS protocols.
//! Uses the suppaftp crate for FTP operations.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use globset::GlobBuilder;
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
    /// Whether server supports MFMT (RFC 3659) for setting remote file mtime
    mfmt_supported: bool,
    /// Whether server supports HASH, XMD5, XCRC, or XSHA1 for remote checksums
    hash_supported: Option<String>,
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
            mfmt_supported: false,
            hash_supported: None,
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
            // M6: Log a warning when TLS certificate verification is disabled.
            // This exposes the connection to MITM attacks — acceptable only for self-signed certs.
            tracing::warn!(
                "[FTP] TLS certificate verification DISABLED for {}:{} — connection is vulnerable to MITM attacks",
                self.config.host, self.config.port
            );
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

    fn join_remote_path(base_path: &str, name: &str) -> String {
        if name.starts_with('/') {
            return name.to_string();
        }

        let trimmed_base = base_path.trim_end_matches('/');
        if trimmed_base.is_empty() {
            format!("/{}", name.trim_start_matches('/'))
        } else {
            format!("{}/{}", trimmed_base, name.trim_start_matches('/'))
        }
    }

    fn normalize_mlsd_name(name: &str) -> String {
        let trimmed = name.trim_end_matches('/');
        std::path::Path::new(trimmed)
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| name.to_string())
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
        let raw_name = name.trim();
        let name = Self::normalize_mlsd_name(raw_name);

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

        let path = Self::join_remote_path(base_path, raw_name);

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

    /// Format MLSD timestamp (YYYYMMDDHHMMSS) to readable form.
    /// Appends 'Z' suffix because MLSD timestamps are always UTC per RFC 3659.
    fn format_mlsd_time(ts: &str) -> String {
        if ts.len() >= 14 {
            format!(
                "{}-{}-{} {}:{}:{}Z",
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
                // A3-02: Try explicit TLS, but NEVER fall back to plaintext silently.
                // Sending credentials over an unencrypted connection without user consent
                // is a security risk. If TLS fails, return an error instead.
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
                        tracing::warn!(
                            "SECURITY: TLS upgrade failed for {}:{} ({}). \
                             Refusing to send credentials over plaintext.",
                            self.config.host, self.config.port, e
                        );
                        return Err(ProviderError::ConnectionFailed(format!(
                            "TLS upgrade failed: {}. Connection would be unencrypted. \
                             Use 'None' encryption mode explicitly to connect without TLS.",
                            e
                        )));
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

        // Check FEAT for MLSD and MFMT support (RFC 3659)
        match stream.feat().await {
            Ok(features) => {
                self.mlsd_supported = features.contains_key("MLST") || features.contains_key("MLSD");
                self.mfmt_supported = features.contains_key("MFMT");
                // B3: Detect hash/checksum commands (prefer HASH > XMD5 > XCRC > XSHA1)
                self.hash_supported = if features.contains_key("HASH") {
                    Some("HASH".to_string())
                } else if features.contains_key("XMD5") {
                    Some("XMD5".to_string())
                } else if features.contains_key("XCRC") {
                    Some("XCRC".to_string())
                } else if features.contains_key("XSHA1") {
                    Some("XSHA1".to_string())
                } else {
                    None
                };
                tracing::debug!("FTP FEAT: MLSD={}, MFMT={}, HASH={:?}",
                    self.mlsd_supported, self.mfmt_supported, self.hash_supported);
            }
            Err(_) => {
                self.mlsd_supported = false;
                self.mfmt_supported = false;
                self.hash_supported = None;
            }
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

        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;

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
            atomic
                .write_all(&chunk[..n])
                .await
                .map_err(ProviderError::IoError)?;
            transferred += n as u64;

            if let Some(ref progress) = on_progress {
                progress(transferred, total_size);
            }
        }

        atomic.commit().await.map_err(ProviderError::IoError)?;

        // Finalize the stream - need to get stream again after the borrow
        let stream = self.stream.as_mut().ok_or(ProviderError::NotConnected)?;
        stream
            .finalize_retr_stream(data_stream)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
        
        Ok(())
    }
    
    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        use tokio::io::AsyncReadExt;
        let limit = super::MAX_DOWNLOAD_TO_BYTES;

        let stream = self.stream_mut()?;

        // Check file size first if server supports SIZE command
        if let Ok(size) = stream.size(remote_path).await {
            if size as u64 > limit {
                return Err(ProviderError::TransferFailed(format!(
                    "File too large for in-memory download ({:.1} MB). Use streaming download for files over {:.0} MB.",
                    size as f64 / 1_048_576.0,
                    limit as f64 / 1_048_576.0,
                )));
            }
        }

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

        // H2: Read with size cap to prevent OOM
        let mut data = Vec::new();
        let limit_usize = (limit + 1) as usize;
        loop {
            let mut buf = [0u8; 8192];
            let n = data_stream.read(&mut buf).await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            if n == 0 { break; }
            data.extend_from_slice(&buf[..n]);
            if data.len() > limit_usize { break; }
        }
        let bytes_read = data.len();

        // Finalize the stream
        let stream = self.stream.as_mut().ok_or(ProviderError::NotConnected)?;
        stream
            .finalize_retr_stream(data_stream)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if bytes_read as u64 > limit {
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
        use tokio::io::AsyncReadExt;
        use suppaftp::types::FileType;

        let stream = self.stream_mut()?;

        // Set binary transfer mode explicitly
        stream.transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let mut file = tokio::fs::File::open(local_path).await
            .map_err(ProviderError::IoError)?;
        let total_size = file.metadata().await
            .map_err(ProviderError::IoError)?.len();

        // Open streaming upload channel (PASV + STOR)
        let mut data_stream = stream.put_with_stream(remote_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        // Write in 64KB chunks for optimal throughput
        let mut chunk = [0u8; 65536];
        let mut total_written: u64 = 0;

        loop {
            let n = file.read(&mut chunk).await.map_err(ProviderError::IoError)?;
            if n == 0 {
                break;
            }
            data_stream.write_all(&chunk[..n])
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Data write error: {}", e)))?;
            total_written += n as u64;
            if let Some(ref progress) = on_progress {
                progress(total_written, total_size);
            }
        }

        // Flush all TLS buffers to the wire
        data_stream.flush()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Flush error: {}", e)))?;

        // Wait for TCP to drain all TLS records before close_notify
        // native-tls shutdown races with TCP send buffer; scale delay with file size
        let drain_ms = (total_written / 4096).clamp(100, 2000);
        tokio::time::sleep(std::time::Duration::from_millis(drain_ms)).await;

        // Finalize: sends TLS close_notify, reads 226 from control channel
        stream.finalize_put_stream(data_stream)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        // Preserve local file's mtime on the remote file via MFMT (draft-somers-ftp-mfxx).
        // MFMT is a standalone FTP command, NOT a SITE sub-command.
        // Best practice: FileZilla, WinSCP, lftp all do this after upload.
        if self.mfmt_supported {
            if let Ok(local_meta) = std::fs::metadata(local_path) {
                if let Ok(mtime) = local_meta.modified() {
                    if let Ok(duration) = mtime.duration_since(std::time::UNIX_EPOCH) {
                        let dt = chrono::DateTime::from_timestamp(duration.as_secs() as i64, 0);
                        if let Some(dt) = dt {
                            let mfmt_time = dt.format("%Y%m%d%H%M%S").to_string();
                            if let Some(stream) = self.stream.as_mut() {
                                // MFMT <time-val> <pathname> — expects 213 response
                                let cmd = format!("MFMT {} {}", mfmt_time, remote_path);
                                if let Err(e) = stream.custom_command(
                                    &cmd,
                                    &[suppaftp::Status::File],
                                ).await {
                                    tracing::debug!("FTP MFMT failed (non-fatal): {}", e);
                                }
                            }
                        }
                    }
                }
            }
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
                if let Some(entry) = self.parse_mlsd_entry(mlst_line.trim(), &parent) {
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
        let matcher = GlobBuilder::new(pattern)
            .case_insensitive(true)
            .literal_separator(true)
            .build()
            .map_err(|e| ProviderError::InvalidConfig(format!("Invalid find pattern '{}': {}", pattern, e)))?
            .compile_matcher();
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

                if matcher.is_match(&entry.name) {
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
        use tokio::io::{AsyncReadExt, AsyncWriteExt as _, AsyncSeekExt};

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

        // Retrieve from offset
        let mut data_stream = stream
            .retr_as_stream(remote_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        // H3: Stream directly to file instead of buffering entire file in memory
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        // Seek to the resume offset (no set_len — preserve existing bytes before offset)
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(ProviderError::IoError)?;

        // Stream chunks from FTP data stream directly to disk
        let mut transferred = offset;
        let mut buf = vec![0u8; 64 * 1024]; // 64 KB chunks
        loop {
            let n = data_stream
                .read(&mut buf)
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])
                .await
                .map_err(ProviderError::IoError)?;
            transferred += n as u64;

            if let Some(ref progress) = on_progress {
                progress(transferred, total_size);
            }
        }

        file.flush().await.map_err(ProviderError::IoError)?;

        let stream = self.stream.as_mut().ok_or(ProviderError::NotConnected)?;
        stream
            .finalize_retr_stream(data_stream)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

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
            .map_err(ProviderError::IoError)?.len();

        if offset >= total_size {
            return Ok(()); // Nothing to upload
        }

        // Open file and seek to offset for streaming append
        let mut file = tokio::fs::File::open(local_path).await
            .map_err(ProviderError::IoError)?;
        file.seek(std::io::SeekFrom::Start(offset)).await
            .map_err(ProviderError::IoError)?;

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

    fn supports_checksum(&self) -> bool {
        self.hash_supported.is_some()
    }

    async fn checksum(&mut self, path: &str) -> Result<std::collections::HashMap<String, String>, ProviderError> {
        self.remote_checksum(path).await
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_resume_download: true,
            supports_resume_upload: true,
            supports_range_download: true,
            ..Default::default()
        }
    }

    async fn read_range(&mut self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>, ProviderError> {
        use tokio::io::AsyncReadExt;

        const MAX_READ_RANGE: u64 = 100 * 1024 * 1024; // 100 MB
        if len > MAX_READ_RANGE {
            return Err(ProviderError::Other(
                format!("Read range size {} exceeds maximum {} bytes", len, MAX_READ_RANGE)
            ));
        }

        let stream = self.stream_mut()?;

        stream.transfer_type(FileType::Binary)
            .await
            .map_err(|e| ProviderError::ServerError(e.to_string()))?;

        // REST sets the byte offset for the next RETR
        stream.resume_transfer(offset as usize)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("REST failed: {}", e)))?;

        let mut data_stream = stream.retr_as_stream(path)
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        // Read exactly `len` bytes (or until EOF if file is shorter)
        let mut buf = vec![0u8; len as usize];
        let mut total_read = 0usize;
        while total_read < len as usize {
            let n = data_stream.read(&mut buf[total_read..])
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Range read failed: {}", e)))?;
            if n == 0 { break; }
            total_read += n;
        }
        buf.truncate(total_read);

        // Bounded FTP reads intentionally stop before EOF. Some servers will report an
        // error while finalizing that partial RETR; when that happens we proactively
        // disconnect so the disposable chunk connection cannot be reused in a bad state.
        let finalize_result = {
            let stream = self.stream.as_mut().ok_or(ProviderError::NotConnected)?;
            stream.finalize_retr_stream(data_stream).await
        };
        if finalize_result.is_err() {
            let _ = self.disconnect().await;
        }

        Ok(buf)
    }
}

// =============================================================================
// FTP Hash/Checksum Commands (B3)
// =============================================================================

impl FtpProvider {
    /// Compute a remote file checksum using the best available command.
    /// Returns a map like {"MD5": "abc123..."} or {"CRC32": "..."} etc.
    pub async fn remote_checksum(&mut self, path: &str) -> Result<std::collections::HashMap<String, String>, ProviderError> {
        let hash_cmd = self.hash_supported.clone()
            .ok_or_else(|| ProviderError::Other("Server does not support hash commands".to_string()))?;

        let stream = self.stream_mut()?;

        let (cmd_str, default_algo) = match hash_cmd.as_str() {
            "HASH" => (format!("HASH {}", path), "SHA-256"),
            "XMD5" => (format!("XMD5 {}", path), "MD5"),
            "XCRC" => (format!("XCRC {}", path), "CRC32"),
            "XSHA1" => (format!("XSHA1 {}", path), "SHA-1"),
            _ => return Err(ProviderError::Other(format!("Unknown hash command: {}", hash_cmd))),
        };

        let response = stream
            .custom_command(&cmd_str, &[suppaftp::Status::File, suppaftp::Status::CommandOk])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Hash command failed: {}", e)))?;

        let body = String::from_utf8_lossy(&response.body).into_owned();
        let mut result = std::collections::HashMap::new();

        if hash_cmd == "HASH" {
            // RFC draft HASH response: "<algo> <range> <hash> <path>"
            // e.g. "SHA-256 0-EOF abc123def456 /path/to/file.txt"
            let parts: Vec<&str> = body.splitn(4, ' ').collect();
            if parts.len() >= 3 {
                let algo = parts[0]; // actual algorithm from server
                let hash = parts[2];
                result.insert(algo.to_string(), hash.to_string());
            } else {
                result.insert(default_algo.to_string(), body.trim().to_string());
            }
        } else {
            // XMD5/XCRC/XSHA1: response is just the hex hash
            result.insert(default_algo.to_string(), body.trim().to_string());
        }

        Ok(result)
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
        assert_eq!(entry.modified.as_deref(), Some("2026-01-31 12:00:00Z"));
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
