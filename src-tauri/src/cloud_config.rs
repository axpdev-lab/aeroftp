// AeroCloud Configuration Module
// Persistent cloud sync configuration storage
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Cloud sync configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudConfig {
    /// Is cloud sync enabled
    pub enabled: bool,
    /// Custom display name for this cloud connection
    #[serde(default)]
    pub cloud_name: String,
    /// Local folder to sync (e.g., ~/Documents/AeroCloud)
    pub local_folder: PathBuf,
    /// Remote folder on FTP server (e.g., /cloud/)
    pub remote_folder: String,
    /// Name of the saved server profile to use
    pub server_profile: String,
    /// Sync interval in seconds (0 = disabled)
    pub sync_interval_secs: u64,
    /// Enable real-time sync on file changes
    pub sync_on_change: bool,
    /// Start sync automatically when app launches
    pub sync_on_startup: bool,
    /// Patterns to exclude from sync
    pub exclude_patterns: Vec<String>,
    /// Last successful sync timestamp
    pub last_sync: Option<DateTime<Utc>>,
    /// Conflict resolution strategy
    pub conflict_strategy: ConflictStrategy,
    /// Public URL base for sharing links (e.g., https://cloud.example.com/)
    /// If set, enables "Share Link" functionality
    #[serde(default)]
    pub public_url_base: Option<String>,
}

/// How to handle file conflicts
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictStrategy {
    /// Always ask the user
    #[default]
    AskUser,
    /// Keep both files (rename with timestamp)
    KeepBoth,
    /// Prefer local version
    PreferLocal,
    /// Prefer remote version
    PreferRemote,
    /// Use newer file based on timestamp
    PreferNewer,
}

impl Default for CloudConfig {
    fn default() -> Self {
        // Default local folder: ~/Documents/AeroCloud
        let local_folder = dirs::document_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
            .join("AeroCloud");

        Self {
            enabled: false,
            cloud_name: String::new(),
            local_folder,
            remote_folder: "/cloud/".to_string(),
            server_profile: String::new(),
            sync_interval_secs: 86400, // 24 hours (watcher handles real-time, this is safety net)
            sync_on_change: true,
            sync_on_startup: false,
            exclude_patterns: vec![
                "node_modules".to_string(),
                ".git".to_string(),
                ".DS_Store".to_string(),
                "Thumbs.db".to_string(),
                "__pycache__".to_string(),
                "*.pyc".to_string(),
                ".env".to_string(),
                "target".to_string(),
                "*.tmp".to_string(),
                "*.swp".to_string(),
                "~*".to_string(),
            ],
            last_sync: None,
            conflict_strategy: ConflictStrategy::default(),
            public_url_base: None,
        }
    }
}

/// Current cloud sync status
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudSyncStatus {
    /// Not configured
    #[default]
    NotConfigured,
    /// Idle, waiting for next sync
    Idle {
        last_sync: Option<DateTime<Utc>>,
        next_sync: Option<DateTime<Utc>>,
    },
    /// Currently syncing
    Syncing {
        current_file: String,
        progress: f64,
        files_done: u32,
        files_total: u32,
    },
    /// Paused by user
    Paused,
    /// Has conflicts that need resolution
    HasConflicts {
        count: u32,
    },
    /// Error state
    Error {
        message: String,
    },
}

/// Cloud sync statistics
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CloudStats {
    pub files_synced: u64,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
    pub last_sync_duration_secs: u64,
    pub total_local_files: u64,
    pub total_remote_files: u64,
}

/// Get the path to the cloud config file
fn get_config_path() -> PathBuf {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
    config_dir.join("aeroftp").join("cloud_config.json")
}

/// Load cloud configuration from disk
pub fn load_cloud_config() -> CloudConfig {
    let config_path = get_config_path();
    
    if config_path.exists() {
        match fs::read_to_string(&config_path) {
            Ok(content) => {
                match serde_json::from_str(&content) {
                    Ok(config) => return config,
                    Err(e) => {
                        tracing::warn!("Failed to parse cloud config: {}", e);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to read cloud config: {}", e);
            }
        }
    }
    
    CloudConfig::default()
}

/// Save cloud configuration to disk
pub fn save_cloud_config(config: &CloudConfig) -> Result<(), String> {
    let config_path = get_config_path();
    
    // Ensure parent directory exists
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }
    
    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;
    
    fs::write(&config_path, content)
        .map_err(|e| format!("Failed to write config: {}", e))?;
    
    tracing::info!("Cloud config saved to {:?}", config_path);
    Ok(())
}

/// Ensure the local cloud folder exists
pub fn ensure_cloud_folder(config: &CloudConfig) -> Result<PathBuf, String> {
    let path = &config.local_folder;
    
    if !path.exists() {
        fs::create_dir_all(path)
            .map_err(|e| format!("Failed to create cloud folder: {}", e))?;
        tracing::info!("Created cloud folder: {:?}", path);
    }
    
    Ok(path.clone())
}

/// Validate cloud configuration
pub fn validate_config(config: &CloudConfig) -> Result<(), String> {
    if config.server_profile.is_empty() {
        return Err("No server profile selected".to_string());
    }
    
    if config.remote_folder.is_empty() {
        return Err("Remote folder cannot be empty".to_string());
    }
    
    if !config.remote_folder.starts_with('/') {
        return Err("Remote folder must be an absolute path".to_string());
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = CloudConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.sync_interval_secs, 86400);
        assert!(config.sync_on_change);
        assert!(!config.exclude_patterns.is_empty());
    }

    #[test]
    fn test_conflict_strategy_default() {
        let strategy = ConflictStrategy::default();
        assert_eq!(strategy, ConflictStrategy::AskUser);
    }

    #[test]
    fn test_validate_config() {
        let mut config = CloudConfig::default();
        
        // Should fail - no server profile
        assert!(validate_config(&config).is_err());
        
        config.server_profile = "MyServer".to_string();
        
        // Should pass now
        assert!(validate_config(&config).is_ok());
        
        // Should fail - empty remote folder
        config.remote_folder = String::new();
        assert!(validate_config(&config).is_err());
        
        // Should fail - relative path
        config.remote_folder = "cloud/".to_string();
        assert!(validate_config(&config).is_err());
    }
}
