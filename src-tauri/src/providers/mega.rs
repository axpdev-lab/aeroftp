//! MEGA Storage Provider - MEGAcmd Implementation
//!
//! Uses official MEGAcmd CLI for reliable MEGA.nz integration
//! Requires MEGAcmd to be installed: https://mega.nz/cmd
//! On Ubuntu/Debian: sudo apt install megacmd

use async_trait::async_trait;
use tokio::process::Command;
use tokio::io::AsyncWriteExt;
use secrecy::ExposeSecret;
use std::path::Path;

use super::{
    StorageProvider, ProviderError, ProviderType, RemoteEntry, MegaConfig, StorageInfo,
};

/// Timeout for all MEGAcmd commands (seconds).
const MEGA_CMD_TIMEOUT_SECS: u64 = 60;

/// Maximum number of automatic retries for transient errors.
const MAX_RETRIES: usize = 2;

/// Delay between retries (milliseconds).
const RETRY_DELAY_MS: u64 = 2000;

pub struct MegaProvider {
    config: MegaConfig,
    connected: bool,
    current_path: String,
}

impl MegaProvider {
    pub fn new(config: MegaConfig) -> Self {
        Self {
            config,
            connected: false,
            current_path: "/".to_string(),
        }
    }

    /// Debug logging through tracing infrastructure (no file I/O)
    fn log_debug(&self, msg: &str) {
        tracing::debug!(target: "mega", "{}", msg);
    }

    /// Resolve MEGAcmd executable path (checks PATH and common install locations)
    fn resolve_mega_cmd(cmd: &str) -> String {
        #[cfg(windows)]
        {
            let program_files = std::env::var("ProgramFiles")
                .unwrap_or_else(|_| r"C:\Program Files".to_string());
            let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
            let candidates = [
                format!(r"{}\MEGAcmd\{}.bat", program_files, cmd),
                format!(r"{}\MEGAcmd\{}.exe", program_files, cmd),
                format!(r"{}\MEGAcmd\{}.bat", local_appdata, cmd),
            ];
            for candidate in &candidates {
                if std::path::Path::new(candidate).exists() {
                    return candidate.clone();
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            // ARCH-05: Check macOS install locations
            let candidates = [
                format!("/Applications/MEGAcmd.app/Contents/MacOS/{}", cmd),
                format!("/usr/local/bin/{}", cmd),
                format!("/opt/homebrew/bin/{}", cmd),
            ];
            for candidate in &candidates {
                if std::path::Path::new(candidate).exists() {
                    return candidate.clone();
                }
            }
        }
        cmd.to_string()
    }

    /// Classify MEGAcmd stderr into typed ProviderError (ERR-01).
    fn classify_mega_error(stderr: &str) -> ProviderError {
        let lower = stderr.to_lowercase();
        if lower.contains("not logged in") || lower.contains("not logged-in") || lower.contains("login required") {
            ProviderError::AuthenticationFailed(format!("MEGAcmd: {}", stderr))
        } else if lower.contains("not found") || lower.contains("no such file") || lower.contains("couldn't find") {
            ProviderError::NotFound(stderr.to_string())
        } else if lower.contains("over quota") || lower.contains("storage quota") || lower.contains("over storage") {
            ProviderError::ServerError("Storage quota exceeded".to_string())
        } else if lower.contains("permission denied") || lower.contains("access denied") {
            ProviderError::PermissionDenied(stderr.to_string())
        } else if lower.contains("already exists") {
            ProviderError::AlreadyExists(stderr.to_string())
        } else if lower.contains("not empty") {
            ProviderError::DirectoryNotEmpty(stderr.to_string())
        } else if lower.contains("temporarily unavailable") || lower.contains("try again") || lower.contains("server busy") {
            ProviderError::ServerError(format!("Transient error: {}", stderr))
        } else {
            ProviderError::ServerError(stderr.to_string())
        }
    }

    /// Check if an error is transient and worth retrying (ERR-02).
    fn is_transient_error(stderr: &str) -> bool {
        let lower = stderr.to_lowercase();
        lower.contains("temporarily unavailable")
            || lower.contains("try again")
            || lower.contains("server busy")
            || lower.contains("connection reset")
            || lower.contains("timed out")
    }

    /// Helper to run mega-* commands with timeout, error classification, and retry (ARCH-04, ERR-01, ERR-02).
    async fn run_mega_cmd(&self, cmd: &str, args: &[&str]) -> Result<String, ProviderError> {
        self.log_debug(&format!("[CMD] {} {:?}", cmd, args));
        let resolved_cmd = Self::resolve_mega_cmd(cmd);

        let mut last_err = ProviderError::Unknown("No attempts made".to_string());

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                tracing::debug!(target: "mega", "[RETRY] attempt {}/{} for {} {:?}", attempt, MAX_RETRIES, cmd, args);
                tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
            }

            let output_future = Command::new(&resolved_cmd)
                .args(args)
                .output();

            let output = match tokio::time::timeout(
                std::time::Duration::from_secs(MEGA_CMD_TIMEOUT_SECS),
                output_future,
            ).await {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => {
                    let err = format!("Failed to execute {} (resolved: {}): {}", cmd, resolved_cmd, e);
                    self.log_debug(&format!("[CMD ERROR] {}", err));
                    return Err(ProviderError::ServerError(err));
                }
                Err(_) => {
                    self.log_debug(&format!("[CMD TIMEOUT] {} exceeded {}s", cmd, MEGA_CMD_TIMEOUT_SECS));
                    return Err(ProviderError::Timeout);
                }
            };

            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                return Ok(stdout);
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            let err_msg = stderr.trim().to_string();
            let final_err = if err_msg.is_empty() {
                "Unknown MEGAcmd error".to_string()
            } else {
                err_msg
            };
            self.log_debug(&format!("[CMD FAILURE] {}", final_err));

            // ERR-02: Retry on transient errors
            if Self::is_transient_error(&final_err) && attempt < MAX_RETRIES {
                last_err = Self::classify_mega_error(&final_err);
                continue;
            }

            return Err(Self::classify_mega_error(&final_err));
        }

        Err(last_err)
    }

    /// Run a mega command with automatic re-auth on session expiry (ERR-04).
    /// Use this for operational commands (not for login/logout themselves).
    async fn run_mega_cmd_with_reauth(&mut self, cmd: &str, args: &[&str]) -> Result<String, ProviderError> {
        match self.run_mega_cmd(cmd, args).await {
            Ok(out) => Ok(out),
            Err(ProviderError::AuthenticationFailed(_)) => {
                tracing::info!(target: "mega", "[REAUTH] Session expired, re-authenticating...");
                self.do_login().await?;
                // Retry the original command once after re-auth
                self.run_mega_cmd(cmd, args).await
            }
            Err(e) => Err(e),
        }
    }

    /// Internal login logic — used by connect() and re-auth (ARCH-03/AUTH-01: password via stdin).
    async fn do_login(&mut self) -> Result<(), ProviderError> {
        let password = self.config.password.expose_secret().to_string();
        let resolved_cmd = Self::resolve_mega_cmd("mega-login");

        let login_future = async {
            let mut child = Command::new(&resolved_cmd)
                .arg(&self.config.email)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| ProviderError::ConnectionFailed(
                    format!("Failed to spawn mega-login: {}", e)
                ))?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(password.as_bytes()).await.map_err(|e| {
                    ProviderError::ConnectionFailed(format!("Failed to write password to stdin: {}", e))
                })?;
                stdin.write_all(b"\n").await.ok();
                drop(stdin);
            }

            let output = child.wait_with_output().await.map_err(|e| {
                ProviderError::ConnectionFailed(format!("mega-login wait failed: {}", e))
            })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let err_msg = stderr.trim();
                return Err(ProviderError::AuthenticationFailed(
                    format!("MEGAcmd Login Failed: {}", if err_msg.is_empty() { "Unknown error" } else { err_msg })
                ));
            }

            Ok(())
        };

        match tokio::time::timeout(
            std::time::Duration::from_secs(MEGA_CMD_TIMEOUT_SECS),
            login_future,
        ).await {
            Ok(result) => result,
            Err(_) => Err(ProviderError::Timeout),
        }
    }

    /// Try to start MEGAcmd daemon if not running (ARCH-02).
    async fn ensure_daemon_running(&self) -> Result<(), ProviderError> {
        // Try mega-version as a connectivity check
        match self.run_mega_cmd("mega-version", &[]).await {
            Ok(_) => Ok(()),
            Err(ProviderError::ServerError(ref e)) if e.contains("Failed to execute") => {
                // Binary not found — daemon can't be started
                Err(ProviderError::ConnectionFailed(
                    "MEGAcmd is not installed. Install it from https://mega.nz/cmd".to_string()
                ))
            }
            Err(_) => {
                // Daemon might not be running — try to start it
                tracing::info!(target: "mega", "[DAEMON] Attempting to start MEGAcmd daemon...");
                let server_cmd = Self::resolve_mega_cmd("mega-cmd-server");
                let _ = Command::new(&server_cmd)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();

                // Give daemon time to start
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                // Verify daemon is now accessible
                match self.run_mega_cmd("mega-version", &[]).await {
                    Ok(_) => Ok(()),
                    Err(_) => Err(ProviderError::ConnectionFailed(
                        "MEGAcmd daemon failed to start. Ensure MEGAcmd is installed: https://mega.nz/cmd".to_string()
                    )),
                }
            }
        }
    }

    /// Helper to resolve path relative to current_path
    fn resolve_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            path.to_string()
        } else if path.is_empty() || path == "." {
            self.current_path.clone()
        } else {
            let base = self.current_path.trim_end_matches('/');
            format!("{}/{}", base, path)
        }
    }

    /// Parse a single mega-ls -l output line into a RemoteEntry (CQ-01: defensive parsing).
    fn parse_ls_line(line: &str, parent_path: &str) -> Option<RemoteEntry> {
        // Skip header and section lines
        if line.contains("FLAGS") && line.contains("VERS") { return None; }
        if line.starts_with('/') && line.ends_with(':') { return None; }

        let parts: Vec<&str> = line.split_whitespace().collect();
        // CQ-01: Require minimum 6 columns (flags, vers, size, date, time, name)
        if parts.len() < 6 {
            if !line.trim().is_empty() {
                tracing::debug!(target: "mega", "[PARSE] Skipping unparseable line ({} columns): {:?}", parts.len(), line);
            }
            return None;
        }

        let flags = parts[0];
        let size_str = parts[2];
        let date_str = parts[3];
        let time_str = parts[4];

        let name = parts[5..].join(" ");
        if name.is_empty() {
            tracing::debug!(target: "mega", "[PARSE] Skipping line with empty name: {:?}", line);
            return None;
        }

        let is_dir = flags.starts_with('d');
        let size = size_str.parse::<u64>().unwrap_or(0);
        let modified = format!("{} {}", date_str, time_str);

        let full_path = if parent_path == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", parent_path.trim_end_matches('/'), name)
        };

        Some(RemoteEntry {
            name,
            path: full_path,
            is_dir,
            size,
            modified: Some(modified),
            is_symlink: false,
            link_target: None,
            permissions: None,
            owner: None,
            group: None,
            mime_type: None,
            metadata: Default::default(),
        })
    }
}

#[async_trait]
impl StorageProvider for MegaProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Mega
    }

    fn display_name(&self) -> String {
        self.config.email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        self.current_path = "/".to_string();

        // ARCH-01: Check MEGAcmd installation and ARCH-02: ensure daemon is running
        self.ensure_daemon_running().await?;

        tracing::info!("[MEGAcmd] Logging in as {}...", self.config.email);

        // AUTH-03/AUTH-05: Check if already logged in as this user (exact match)
        if let Ok(whoami) = self.run_mega_cmd("mega-whoami", &[]).await {
            let whoami_trimmed = whoami.trim();
            if whoami_trimmed == self.config.email || whoami_trimmed.ends_with(&format!(" {}", self.config.email)) {
                tracing::info!("[MEGAcmd] Already logged in (Existing Session).");
                self.connected = true;
                return Ok(());
            }
            // If logged in as someone else, logout
            tracing::info!("[MEGAcmd] Active session mismatch. Logging out...");
            if let Err(e) = self.run_mega_cmd("mega-logout", &[]).await {
                // AUTH-04: Log warning on logout failure
                tracing::warn!("[MEGAcmd] Logout failed during session switch: {}", e);
            }
        }

        // ARCH-03/AUTH-01: Password via stdin pipe (not CLI argument)
        self.do_login().await?;

        tracing::info!("[MEGAcmd] Login successful.");
        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.current_path = "/".to_string();

        let should_logout = self.config.logout_on_disconnect.unwrap_or(false);

        if should_logout {
            tracing::info!("[MEGAcmd] Disconnecting and killing session (logout_on_disconnect=true)");
            if let Err(e) = self.run_mega_cmd("mega-logout", &[]).await {
                tracing::warn!("[MEGAcmd] Logout on disconnect failed: {}", e);
            }
        } else {
            tracing::info!("[MEGAcmd] Disconnecting but KEEPING session active (logout_on_disconnect=false)");
            // AUTH-05/AUTH-22: MEGAcmd daemon handles session persistence internally.
        }

        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let target_path = self.resolve_path(path);

        tracing::debug!("[MEGAcmd] Listing path: {}", target_path);

        let output = self.run_mega_cmd_with_reauth("mega-ls", &["-l", &target_path]).await
            .map_err(|e| match e {
                ProviderError::NotFound(_) => ProviderError::NotFound(format!("Path not found: {}", target_path)),
                other => other,
            })?;

        let mut entries = Vec::new();

        for line in output.lines() {
            if let Some(entry) = Self::parse_ls_line(line, &target_path) {
                entries.push(entry);
            }
        }

        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path == ".." {
            if self.current_path == "/" {
                "/".to_string()
            } else {
                let parent = std::path::Path::new(&self.current_path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or("/".to_string());
                if parent.is_empty() { "/".to_string() } else { parent }
            }
        } else {
            self.resolve_path(path)
        };

        tracing::debug!("[MEGAcmd] CD verifying path: {}", new_path);

        self.run_mega_cmd_with_reauth("mega-ls", &[&new_path]).await
            .map_err(|e| match e {
                ProviderError::NotFound(_) => ProviderError::NotFound(format!("Invalid directory: {}", new_path)),
                other => other,
            })?;

        self.current_path = new_path;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn download(&mut self, r: &str, l: &str, progress: Option<Box<dyn Fn(u64,u64)+Send>>) -> Result<(), ProviderError> {
        let abs_remote = self.resolve_path(r);
        self.log_debug(&format!("[MEGAcmd] Downloading '{}' to '{}'", abs_remote, l));

        // XFER-08: Signal start
        if let Some(ref cb) = progress {
            cb(0, 0);
        }

        // XFER-03: Add --resume flag for resume support
        match self.run_mega_cmd_with_reauth("mega-get", &["--resume", &abs_remote, l]).await {
            Ok(out) => {
                self.log_debug(&format!("[MEGAcmd] Download output: {}", out));
                // XFER-02: Signal completion with file size
                if let Some(ref cb) = progress {
                    match std::fs::metadata(l) {
                        Ok(meta) => cb(meta.len(), meta.len()),
                        Err(_) => cb(1, 1),
                    }
                }
                Ok(())
            },
            Err(e) => {
                self.log_debug(&format!("[MEGAcmd] Download ERROR: {}", e));
                Err(e)
            }
        }
    }

    async fn download_to_bytes(&mut self, r: &str) -> Result<Vec<u8>, ProviderError> {
        // XFER-01: Implement via temp file
        let abs_remote = self.resolve_path(r);
        let file_name = Path::new(&abs_remote)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("mega_tmp_{}", uuid::Uuid::new_v4()));

        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join(format!("aeroftp_mega_{}", file_name));
        let temp_str = temp_path.to_string_lossy().to_string();

        self.run_mega_cmd_with_reauth("mega-get", &[&abs_remote, &temp_str]).await
            .map_err(|e| ProviderError::TransferFailed(format!("Download to bytes failed: {}", e)))?;

        let bytes = tokio::fs::read(&temp_path).await.map_err(|e| {
            ProviderError::IoError(e)
        })?;

        // Clean up temp file
        if let Err(e) = tokio::fs::remove_file(&temp_path).await {
            tracing::debug!(target: "mega", "[CLEANUP] Failed to remove temp file {:?}: {}", temp_path, e);
        }

        Ok(bytes)
    }

    async fn upload(&mut self, l: &str, r: &str, progress: Option<Box<dyn Fn(u64,u64)+Send>>) -> Result<(), ProviderError> {
        // XFER-04/CQ-09: Resolve remote path
        let abs_remote = self.resolve_path(r);

        // XFER-02: Signal start
        if let Some(ref cb) = progress {
            cb(0, 0);
        }

        self.run_mega_cmd_with_reauth("mega-put", &[l, &abs_remote]).await
            .map_err(|e| ProviderError::TransferFailed(format!("Upload failed: {}", e)))?;

        // XFER-02: Signal completion with local file size
        if let Some(ref cb) = progress {
            match std::fs::metadata(l) {
                Ok(meta) => cb(meta.len(), meta.len()),
                Err(_) => cb(1, 1),
            }
        }

        Ok(())
    }

    async fn mkdir(&mut self, p: &str) -> Result<(), ProviderError> {
        // CQ-06: Resolve path
        let p = self.resolve_path(p);
        self.run_mega_cmd_with_reauth("mega-mkdir", &[&p]).await?;
        Ok(())
    }

    async fn delete(&mut self, p: &str) -> Result<(), ProviderError> {
        // CQ-07: Resolve path
        let p = self.resolve_path(p);
        // Permanent delete: -f bypasses rubbish bin. Soft delete available via move_to_trash().
        self.run_mega_cmd_with_reauth("mega-rm", &["-f", &p]).await?;
        Ok(())
    }

    async fn rmdir(&mut self, p: &str) -> Result<(), ProviderError> {
        // CQ-08: -r for recursive, -f for permanent delete (bypasses rubbish bin).
        let p = self.resolve_path(p);
        self.run_mega_cmd_with_reauth("mega-rm", &["-r", "-f", &p]).await?;
        Ok(())
    }

    async fn rmdir_recursive(&mut self, p: &str) -> Result<(), ProviderError> {
        let p = self.resolve_path(p);
        self.run_mega_cmd_with_reauth("mega-rm", &["-r", &p]).await?;
        Ok(())
    }

    async fn rename(&mut self, f: &str, t: &str) -> Result<(), ProviderError> {
        let f = self.resolve_path(f);
        let t = self.resolve_path(t);
        self.run_mega_cmd_with_reauth("mega-mv", &[&f, &t]).await?;
        Ok(())
    }

    async fn stat(&mut self, p: &str) -> Result<RemoteEntry, ProviderError> {
        let abs_path = self.resolve_path(p);
        let target_name = Path::new(&abs_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // For root, return a synthetic directory entry
        if abs_path == "/" || target_name.is_empty() {
            return Ok(RemoteEntry {
                name: "/".to_string(),
                path: "/".to_string(),
                is_dir: true,
                size: 0,
                modified: None,
                is_symlink: false,
                link_target: None,
                permissions: None,
                owner: None,
                group: None,
                mime_type: None,
                metadata: Default::default(),
            });
        }

        // List the parent directory and find the matching entry by name
        let parent = Path::new(&abs_path)
            .parent()
            .map(|pp| pp.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string());
        let parent = if parent.is_empty() { "/".to_string() } else { parent };

        let output = self.run_mega_cmd_with_reauth("mega-ls", &["-l", &parent]).await
            .map_err(|e| ProviderError::ServerError(format!("stat failed for '{}': {}", abs_path, e)))?;

        for line in output.lines() {
            if let Some(entry) = Self::parse_ls_line(line, &parent) {
                if entry.name == target_name {
                    return Ok(RemoteEntry {
                        path: abs_path,
                        ..entry
                    });
                }
            }
        }

        Err(ProviderError::NotFound(format!("Path not found: {}", abs_path)))
    }

    async fn size(&mut self, p: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(p).await?;
        Ok(entry.size)
    }

    async fn exists(&mut self, p: &str) -> Result<bool, ProviderError> {
        let p = self.resolve_path(p);
        match self.run_mega_cmd_with_reauth("mega-ls", &[&p]).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(_) => Ok(false),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> { Ok(()) }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        self.run_mega_cmd_with_reauth("mega-whoami", &[]).await
    }

    fn supports_server_copy(&self) -> bool { true }

    async fn server_copy(&mut self, f: &str, t: &str) -> Result<(), ProviderError> {
        let f = self.resolve_path(f);
        let t = self.resolve_path(t);
        self.run_mega_cmd_with_reauth("mega-cp", &[&f, &t]).await?;
        Ok(())
    }

    fn supports_share_links(&self) -> bool { true }

    async fn create_share_link(
        &mut self,
        path: &str,
        expires_in_secs: Option<u64>,
    ) -> Result<String, ProviderError> {
        let abs_path = self.resolve_path(path);

        // SHARE-01: MEGAcmd's mega-export does not support link expiry.
        if let Some(secs) = expires_in_secs {
            tracing::warn!(target: "mega", "Link expiry requested ({}s) but MEGAcmd mega-export does not support expiry. Creating permanent link.", secs);
        }

        let output = self.run_mega_cmd_with_reauth("mega-export", &["-a", &abs_path]).await?;

        // mega-export output format: "Exported /path: https://mega.nz/..."
        for line in output.lines() {
            if let Some(url_start) = line.find("https://mega.nz/") {
                return Ok(line[url_start..].trim().to_string());
            }
        }

        Err(ProviderError::ParseError(
            format!("Could not parse export link from MEGAcmd output: {}", output.trim())
        ))
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        let abs_path = self.resolve_path(path);
        self.run_mega_cmd_with_reauth("mega-export", &["-d", &abs_path]).await?;
        Ok(())
    }

    fn supports_import_link(&self) -> bool { true }

    async fn import_link(&mut self, link: &str, dest: &str) -> Result<(), ProviderError> {
        let abs_dest = self.resolve_path(dest);
        self.run_mega_cmd_with_reauth("mega-import", &[link, &abs_dest]).await?;
        Ok(())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let output = self.run_mega_cmd_with_reauth("mega-df", &[]).await?;

        // CQ-05: Only parse the "Total" or "Cloud drive" line for storage info
        let mut used: u64 = 0;
        let mut total: u64 = 0;

        for line in output.lines() {
            let line = line.trim();
            // Look for lines with "of" pattern containing byte counts
            if let Some(colon_pos) = line.find(':') {
                let label = line[..colon_pos].trim().to_lowercase();
                let rest = line[colon_pos + 1..].trim();
                let parts: Vec<&str> = rest.split_whitespace().collect();
                // Format: "<used> of <total> ..."
                if parts.len() >= 3 && parts[1] == "of" {
                    if let (Ok(u), Ok(t)) = (parts[0].parse::<u64>(), parts[2].parse::<u64>()) {
                        // Prefer "Total" line; fall back to "Cloud drive"
                        if label.contains("total") {
                            used = u;
                            total = t;
                            break;
                        } else if label.contains("cloud drive") {
                            used = u;
                            total = t;
                            // Don't break — a "Total" line may follow
                        }
                    }
                }
            }
        }

        Ok(StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
        })
    }

    async fn disk_usage(&mut self, path: &str) -> Result<u64, ProviderError> {
        let abs_path = self.resolve_path(path);
        let output = self.run_mega_cmd_with_reauth("mega-du", &[&abs_path]).await?;

        // CQ-04: More specific line matching for disk usage output.
        // mega-du output: "Total storage used: 123456789 bytes" or "/path: 123456789"
        // Parse from last line that contains a numeric value.
        for line in output.lines().rev() {
            let line = line.trim();
            if line.is_empty() { continue; }
            // Try to find last numeric token
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(last) = parts.last() {
                // Strip "bytes" suffix if present
                let num_str = last.trim_end_matches("bytes").trim();
                if let Ok(bytes) = num_str.parse::<u64>() {
                    return Ok(bytes);
                }
                // Also try the raw last token
                if let Ok(bytes) = last.parse::<u64>() {
                    return Ok(bytes);
                }
            }
        }

        Err(ProviderError::ParseError(
            format!("Could not parse disk usage from: {}", output.trim())
        ))
    }

    fn supports_find(&self) -> bool { true }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let abs_path = self.resolve_path(path);
        let output = self.run_mega_cmd_with_reauth("mega-find", &[&abs_path, "--pattern", pattern]).await?;

        let mut entries = Vec::new();
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() { continue; }

            // mega-find outputs full paths, one per line
            // CQ-03: Metadata not available from mega-find — size/modified left as defaults
            let name = Path::new(line)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| line.to_string());

            let is_dir = line.ends_with('/');
            let clean_path = line.trim_end_matches('/').to_string();

            entries.push(RemoteEntry {
                name: name.trim_end_matches('/').to_string(),
                path: clean_path,
                is_dir,
                size: 0,
                modified: None,
                is_symlink: false,
                link_target: None,
                permissions: None,
                owner: None,
                group: None,
                mime_type: None,
                metadata: Default::default(),
            });
        }

        Ok(entries)
    }

    async fn set_speed_limit(&mut self, upload_kb: u64, download_kb: u64) -> Result<(), ProviderError> {
        let ul = upload_kb.to_string();
        let dl = download_kb.to_string();
        self.run_mega_cmd_with_reauth("mega-speedlimit", &["-u", &ul, "-d", &dl]).await?;
        Ok(())
    }

    async fn get_speed_limit(&mut self) -> Result<(u64, u64), ProviderError> {
        let output = self.run_mega_cmd_with_reauth("mega-speedlimit", &[]).await?;

        let mut upload: u64 = 0;
        let mut download: u64 = 0;

        for line in output.lines() {
            let line_lower = line.to_lowercase();
            if let Some(eq_pos) = line.find('=') {
                let val_str = line[eq_pos + 1..].trim();
                let num_str = val_str.split_whitespace().next().unwrap_or("0");
                let val = num_str.parse::<u64>().unwrap_or(0);
                if line_lower.contains("upload") {
                    upload = val;
                } else if line_lower.contains("download") {
                    download = val;
                }
            }
        }

        Ok((upload, download))
    }
}

/// MEGA-specific methods (trash management, etc.)
impl MegaProvider {
    /// Move a file or directory to the MEGA rubbish bin (soft delete).
    pub async fn move_to_trash(&mut self, path: &str) -> Result<(), ProviderError> {
        let p = self.resolve_path(path);
        // mega-rm without -f moves to rubbish bin. -r handles directories.
        self.run_mega_cmd_with_reauth("mega-rm", &["-r", &p]).await?;
        Ok(())
    }

    /// TRASH-02: List items in the MEGA rubbish bin via `mega-ls /Rubbish`.
    pub async fn list_trash(&mut self) -> Result<Vec<RemoteEntry>, ProviderError> {
        let output = self.run_mega_cmd_with_reauth("mega-ls", &["-l", "/Rubbish"]).await
            .map_err(|e| match e {
                ProviderError::NotFound(_) => {
                    // Empty rubbish or path doesn't exist — return empty list
                    ProviderError::NotFound("Rubbish bin empty".to_string())
                }
                other => other,
            });

        match output {
            Ok(out) => {
                let mut entries = Vec::new();
                for line in out.lines() {
                    if let Some(entry) = Self::parse_ls_line(line, "/Rubbish") {
                        entries.push(entry);
                    }
                }
                Ok(entries)
            }
            Err(ProviderError::NotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// TRASH-03: Restore an item from rubbish bin to a destination path.
    pub async fn restore_from_trash(&mut self, filename: &str, dest: &str) -> Result<(), ProviderError> {
        let rubbish_path = format!("/Rubbish/{}", filename.trim_start_matches('/'));
        let abs_dest = self.resolve_path(dest);
        self.run_mega_cmd_with_reauth("mega-mv", &[&rubbish_path, &abs_dest]).await?;
        Ok(())
    }

    /// TRASH-04: Permanently delete an item from the rubbish bin.
    pub async fn permanent_delete_from_trash(&mut self, filename: &str) -> Result<(), ProviderError> {
        let rubbish_path = format!("/Rubbish/{}", filename.trim_start_matches('/'));
        // Use -f flag for permanent deletion
        self.run_mega_cmd_with_reauth("mega-rm", &["-f", &rubbish_path]).await?;
        Ok(())
    }

    // TODO: SHARE-02: User-to-user sharing via `mega-share` command
    // TODO: AUTH-07/QUOTA-01: Transfer quota tracking
    // TODO: QUOTA-02: Pro status detection
}
