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
    StorageProvider, ProviderError, ProviderType, RemoteEntry, MegaConfig,
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

    /// Helper to log to file for debugging
    fn log_debug(&self, msg: &str) {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/aeroftp-mega.log") {
            let _ = writeln!(file, "{}", msg);
        }
        println!("{}", msg); // Keep stdout just in case
    }

    /// Helper to run mega-* commands
    async fn run_mega_cmd(&self, cmd: &str, args: &[&str]) -> Result<String, String> {
        self.log_debug(&format!("[CMD] {} {:?}", cmd, args));
        
        let output = Command::new(cmd)
            .args(args)
            .output()
            .await
            .map_err(|e| {
                let err = format!("Failed to execute {}: {}", cmd, e);
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
}
