//! MEGA Storage Provider - MEGAcmd Implementation
//! 
//! Uses official MEGAcmd CLI for reliable MEGA.nz integration
//! Requires MEGAcmd to be installed: https://mega.nz/cmd
//! On Ubuntu/Debian: sudo apt install megacmd

use async_trait::async_trait;
use tokio::process::Command;
use secrecy::ExposeSecret;
use std::path::Path;

use super::{
    StorageProvider, ProviderError, ProviderType, RemoteEntry, MegaConfig, StorageInfo,
};

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

    /// Resolve MEGAcmd executable path (checks PATH and common install locations on Windows)
    fn resolve_mega_cmd(cmd: &str) -> String {
        #[cfg(windows)]
        {
            // Check common MEGAcmd install paths on Windows
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
        cmd.to_string()
    }

    /// Helper to run mega-* commands
    async fn run_mega_cmd(&self, cmd: &str, args: &[&str]) -> Result<String, String> {
        self.log_debug(&format!("[CMD] {} {:?}", cmd, args));
        let resolved_cmd = Self::resolve_mega_cmd(cmd);

        let output = Command::new(&resolved_cmd)
            .args(args)
            .output()
            .await
            .map_err(|e| {
                let err = format!("Failed to execute {} (resolved: {}): {}", cmd, resolved_cmd, e);
                self.log_debug(&format!("[CMD ERROR] {}", err));
                err
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let err_msg = stderr.trim().to_string();
            let final_err = if err_msg.is_empty() { 
                "Unknown MEGAcmd error".to_string() 
            } else { 
                err_msg 
            };
            self.log_debug(&format!("[CMD FAILURE] {}", final_err));
            return Err(final_err);
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        // self.log_debug(&format!("[CMD OUTPUT] {}", stdout)); // Too verbose for list
        Ok(stdout)
    }

    /// Helper to resolve path relative to current_path
    fn resolve_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            path.to_string()
        } else if path.is_empty() || path == "." {
            self.current_path.clone()
        } else {
            // Join paths preventing double slash
            let base = self.current_path.trim_end_matches('/');
            format!("{}/{}", base, path)
        }
    }
}

#[async_trait]
impl StorageProvider for MegaProvider {
    fn provider_type(&self) -> ProviderType {
        ProviderType::Mega
    }

    fn display_name(&self) -> String {
        self.config.email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        // Reset path on connect
        self.current_path = "/".to_string();

        let password = self.config.password.expose_secret();
        
        tracing::info!("[MEGAcmd] Logging in as {}...", self.config.email);
        
        // First check if already logged in as this user
        if let Ok(whoami) = self.run_mega_cmd("mega-whoami", &[]).await {
            if whoami.contains(&self.config.email) {
                tracing::info!("[MEGAcmd] Already logged in (Existing Session).");
                self.connected = true;
                return Ok(());
            }
            // If logged in as someone else, logout
            tracing::info!("[MEGAcmd] Active session mismatch. Logging out...");
            let _ = self.run_mega_cmd("mega-logout", &[]).await;
        }

        let _ = self.run_mega_cmd("mega-login", &[&self.config.email, password])
            .await
            .map_err(|e| ProviderError::AuthenticationFailed(format!("MEGAcmd Login Failed: {}", e)))?;

        tracing::info!("[MEGAcmd] Login successful.");
        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.current_path = "/".to_string();
        
        let should_logout = self.config.logout_on_disconnect.unwrap_or(false);
        
        if should_logout {
            tracing::info!("[MEGAcmd] Disconnecting and killing session (logout_on_disconnect=true)");
            let _ = self.run_mega_cmd("mega-logout", &[]).await;
        } else {
             tracing::info!("[MEGAcmd] Disconnecting but KEEPING session active (logout_on_disconnect=false)");
        }
        
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        // Resolve path: if empty use current, else resolve relative/abs
        let target_path = self.resolve_path(path);

        tracing::debug!("[MEGAcmd] Listing path: {}", target_path);

        let output = self.run_mega_cmd("mega-ls", &["-l", &target_path])
            .await
            .map_err(|e| ProviderError::ServerError(format!("List failed for '{}': {}", target_path, e)))?;

        let mut entries = Vec::new();
        
        for line in output.lines() {
            if line.contains("FLAGS") && line.contains("VERS") { continue; }
            if line.starts_with('/') && line.ends_with(':') { continue; }

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 5 { continue; }
            
            let flags = parts[0];
            let size_str = parts[2];
            let date_str = parts[3];
            let time_str = parts[4];
            
            // Name starts at index 5
            let name = if parts.len() > 5 {
                parts[5..].join(" ")
            } else {
                continue; 
            };
            
            let is_dir = flags.starts_with('d'); 
            let size = size_str.parse::<u64>().unwrap_or(0);
            let modified = format!("{} {}", date_str, time_str);

            let full_path = if target_path == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", target_path.trim_end_matches('/'), name)
            };

            entries.push(RemoteEntry {
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
            });
        }

        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> { 
        Ok(self.current_path.clone()) 
    }
    
    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> { 
        // Resolve new path
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
        
        // Verify existence via ls
        tracing::debug!("[MEGAcmd] CD verifying path: {}", new_path);
        
        self.run_mega_cmd("mega-ls", &[&new_path])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Invalid directory '{}': {}", new_path, e)))?;
            
        self.current_path = new_path;
        Ok(())
    }
    
    async fn cd_up(&mut self) -> Result<(), ProviderError> { 
        self.cd("..").await 
    }
    
    async fn download(&mut self, r: &str, l: &str, _progress: Option<Box<dyn Fn(u64,u64)+Send>>) -> Result<(), ProviderError> {
        let abs_remote = self.resolve_path(r);
        self.log_debug(&format!("[MEGAcmd] Downloading '{}' to '{}'", abs_remote, l));
        
        match self.run_mega_cmd("mega-get", &[&abs_remote, l]).await {
            Ok(out) => {
                self.log_debug(&format!("[MEGAcmd] Download output: {}", out));
                Ok(())
            },
            Err(e) => {
                self.log_debug(&format!("[MEGAcmd] Download ERROR: {}", e));
                Err(ProviderError::ServerError(e))
            }
        }
    }
    
    async fn download_to_bytes(&mut self, _r: &str) -> Result<Vec<u8>, ProviderError> { 
        Err(ProviderError::ServerError("Download to bytes not supported by MEGAcmd wrapper".into())) 
    }
    
    async fn upload(&mut self, l: &str, r: &str, _progress: Option<Box<dyn Fn(u64,u64)+Send>>) -> Result<(), ProviderError> {
        self.run_mega_cmd("mega-put", &[l, r])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Upload failed: {}", e)))?;
        Ok(())
    }
    
    async fn mkdir(&mut self, p: &str) -> Result<(), ProviderError> {
        self.run_mega_cmd("mega-mkdir", &[p])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Mkdir failed: {}", e)))?;
        Ok(())
    }
    
    async fn delete(&mut self, p: &str) -> Result<(), ProviderError> {
        self.run_mega_cmd("mega-rm", &[p])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Delete failed: {}", e)))?;
        Ok(())
    }
    
    async fn rmdir(&mut self, p: &str) -> Result<(), ProviderError> { 
        self.delete(p).await 
    }
    
    async fn rmdir_recursive(&mut self, p: &str) -> Result<(), ProviderError> {
        self.run_mega_cmd("mega-rm", &["-r", p])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Recursive delete failed: {}", e)))?;
        Ok(())
    }
    
    async fn rename(&mut self, f: &str, t: &str) -> Result<(), ProviderError> {
        self.run_mega_cmd("mega-mv", &[f, t])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Rename failed: {}", e)))?;
        Ok(())
    }
    
    async fn stat(&mut self, p: &str) -> Result<RemoteEntry, ProviderError> { 
        // Mock stat using list?
        Ok(RemoteEntry {
            name: Path::new(p).file_name().unwrap_or_default().to_string_lossy().to_string(),
            path: p.into(), 
            is_dir: false, 
            size: 0,
            modified: None, permissions: None, owner: None, group: None,
            is_symlink: false, link_target: None, mime_type: None, 
            metadata: Default::default()
        })
    }
    
    async fn size(&mut self, _p: &str) -> Result<u64, ProviderError> { Ok(0) }
    
    async fn exists(&mut self, p: &str) -> Result<bool, ProviderError> { 
        // Very basic check: try to ls the file
        match self.run_mega_cmd("mega-ls", &[p]).await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }
    
    async fn keep_alive(&mut self) -> Result<(), ProviderError> { Ok(()) }
    
    async fn server_info(&mut self) -> Result<String, ProviderError> { 
        self.run_mega_cmd("mega-whoami", &[])
            .await
            .map_err(|e| ProviderError::ServerError(e))
    }
    
    fn supports_server_copy(&self) -> bool { true }

    async fn server_copy(&mut self, f: &str, t: &str) -> Result<(), ProviderError> {
        self.run_mega_cmd("mega-cp", &[f, t])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Copy failed: {}", e)))?;
        Ok(())
    }

    fn supports_share_links(&self) -> bool { true }

    async fn create_share_link(
        &mut self,
        path: &str,
        _expires_in_secs: Option<u64>,
    ) -> Result<String, ProviderError> {
        let abs_path = self.resolve_path(path);
        let output = self.run_mega_cmd("mega-export", &["-a", &abs_path])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Export link failed: {}", e)))?;

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
        self.run_mega_cmd("mega-export", &["-d", &abs_path])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Remove export link failed: {}", e)))?;
        Ok(())
    }

    fn supports_import_link(&self) -> bool { true }

    async fn import_link(&mut self, link: &str, dest: &str) -> Result<(), ProviderError> {
        let abs_dest = self.resolve_path(dest);
        self.run_mega_cmd("mega-import", &[link, &abs_dest])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Import link failed: {}", e)))?;
        Ok(())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let output = self.run_mega_cmd("mega-df", &[])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Storage info failed: {}", e)))?;

        // mega-df output example:
        // Cloud drive:     1234567890 of 53687091200 (2.29%)
        // or with -h:
        // Cloud drive:     1.15 GB of 50 GB (2.29%)
        // We parse the raw bytes version (without -h flag)
        let mut used: u64 = 0;
        let mut total: u64 = 0;

        for line in output.lines() {
            let line = line.trim();
            // Look for lines with "of" pattern containing byte counts
            if let Some(colon_pos) = line.find(':') {
                let rest = line[colon_pos + 1..].trim();
                let parts: Vec<&str> = rest.split_whitespace().collect();
                // Format: "<used> of <total> ..."
                if parts.len() >= 3 && parts[1] == "of" {
                    if let (Ok(u), Ok(t)) = (parts[0].parse::<u64>(), parts[2].parse::<u64>()) {
                        used += u;
                        total = t; // Total is the same across lines
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
        let output = self.run_mega_cmd("mega-du", &[&abs_path])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Disk usage failed: {}", e)))?;

        // mega-du output: "Total storage used: 123456789 bytes" or "/path: 123456789"
        for line in output.lines().rev() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(last) = parts.last() {
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
        let output = self.run_mega_cmd("mega-find", &[&abs_path, "--pattern", pattern])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Find failed: {}", e)))?;

        let mut entries = Vec::new();
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() { continue; }

            // mega-find outputs full paths, one per line
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
        self.run_mega_cmd("mega-speedlimit", &["-u", &ul, "-d", &dl])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Set speed limit failed: {}", e)))?;
        Ok(())
    }

    async fn get_speed_limit(&mut self) -> Result<(u64, u64), ProviderError> {
        let output = self.run_mega_cmd("mega-speedlimit", &[])
            .await
            .map_err(|e| ProviderError::ServerError(format!("Get speed limit failed: {}", e)))?;

        // mega-speedlimit output:
        // Upload speed limit = 0 (unlimited)
        // Download speed limit = 0 (unlimited)
        let mut upload: u64 = 0;
        let mut download: u64 = 0;

        for line in output.lines() {
            let line_lower = line.to_lowercase();
            if let Some(eq_pos) = line.find('=') {
                let val_str = line[eq_pos + 1..].trim();
                // Parse first number before any parenthetical
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
