// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroCloud Configuration Module
// Persistent cloud sync configuration storage
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

/// Serializes concurrent writes to the cloud config file
static CONFIG_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Cloud sync configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudConfig {
    /// Is cloud sync enabled (config valid and user opted-in)
    pub enabled: bool,
    /// Paused by user. Config is retained but background sync is stopped until resumed.
    #[serde(default)]
    pub paused: bool,
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
    /// Sync direction for this AeroCloud folder (Task 2).
    /// Default Bidirectional for back-compat with existing configs.
    #[serde(default)]
    pub sync_direction: crate::sync::CompareDirection,
    /// For send-only / receive-only: when true (default), do not propagate
    /// deletes toward the read side (additive backup semantics). When false
    /// the folder acts as a mirror and will delete on the target side.
    #[serde(default = "default_preserve_remote_deletes")]
    pub preserve_remote_deletes: bool,
    /// Enable the AeroCompress overlay for this AeroCloud target.
    /// When combined with an AeroCrypt-bound profile, compression is applied
    /// before encryption. Without crypt, this is the transparent A2 overlay.
    #[serde(default)]
    pub compress_enabled: bool,
    /// zstd compression level for AeroCompress. Defaults to balanced level 3.
    #[serde(default = "default_compress_level")]
    pub compress_level: i32,
    /// Protocol type for the cloud connection (e.g., "ftp", "sftp", "googledrive", "s3")
    /// Default: "ftp" for backward compatibility with existing configs
    #[serde(default = "default_protocol_type")]
    pub protocol_type: String,
    /// Protocol-specific connection parameters stored as JSON
    /// FTP: {} (uses server_profile credentials from vault)
    /// S3: {"bucket": "...", "region": "...", "endpoint": "..."}
    /// OAuth2: {"client_id": "...", "client_secret": "...", "region": "..."}
    #[serde(default)]
    pub connection_params: serde_json::Value,
    /// Selective sync: remote folders excluded from sync.
    /// Paths are relative to remote_folder (e.g., ["backup/old", "media/raw"]).
    #[serde(default)]
    pub excluded_folders: Vec<String>,
    /// File versioning strategy for pre-overwrite/pre-delete archiving.
    #[serde(default)]
    pub versioning_strategy: crate::sync_versioning::VersioningStrategy,
    /// Real-time watcher debounce quiet period, in milliseconds. Filesystem
    /// events are batched until no new events arrive for this long before a
    /// watcher-triggered sync fires. Default 1500ms. Applied when the watcher
    /// (re)starts; clamped to [100, 60000] at use.
    #[serde(default = "default_watcher_debounce_ms")]
    pub watcher_debounce_ms: u64,
    /// Minimum seconds between two watcher-triggered syncs (cooldown). Prevents
    /// a feedback loop where a sync writes files the watcher then re-detects.
    /// Default 30s. 0 disables the cooldown. Read fresh each sync cycle.
    #[serde(default = "default_watcher_cooldown_secs")]
    pub watcher_cooldown_secs: u64,
}

fn default_protocol_type() -> String {
    "ftp".to_string()
}

fn default_preserve_remote_deletes() -> bool {
    true
}

pub fn default_compress_level() -> i32 {
    3
}

pub fn validate_compress_level(level: i32) -> Result<(), String> {
    if (1..=22).contains(&level) {
        Ok(())
    } else {
        Err(format!(
            "invalid compression level {level}: expected an integer in 1..=22"
        ))
    }
}

fn default_watcher_debounce_ms() -> u64 {
    1500
}

fn default_watcher_cooldown_secs() -> u64 {
    30
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
            paused: false,
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
                ".aeroignore".to_string(),
                ".aeroversions".to_string(),
            ],
            excluded_folders: Vec::new(),
            versioning_strategy: crate::sync_versioning::VersioningStrategy::default(),
            last_sync: None,
            conflict_strategy: ConflictStrategy::default(),
            public_url_base: None,
            protocol_type: default_protocol_type(),
            connection_params: serde_json::Value::Null,
            sync_direction: crate::sync::CompareDirection::Bidirectional,
            preserve_remote_deletes: default_preserve_remote_deletes(),
            compress_enabled: false,
            compress_level: default_compress_level(),
            watcher_debounce_ms: default_watcher_debounce_ms(),
            watcher_cooldown_secs: default_watcher_cooldown_secs(),
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
    HasConflicts { count: u32 },
    /// Error state
    Error { message: String },
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
    crate::portable::aeroftp_data_root()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("cloud_config.json")
}

/// Load cloud configuration from disk
pub fn load_cloud_config() -> CloudConfig {
    let config_path = get_config_path();

    if config_path.exists() {
        match fs::read_to_string(&config_path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(config) => return config,
                Err(e) => {
                    tracing::warn!("Failed to parse cloud config: {}", e);
                }
            },
            Err(e) => {
                tracing::warn!("Failed to read cloud config: {}", e);
            }
        }
    }

    CloudConfig::default()
}

/// Save cloud configuration to disk (serialized via write lock, atomic temp+rename)
pub fn save_cloud_config(config: &CloudConfig) -> Result<(), String> {
    let _lock = CONFIG_WRITE_LOCK
        .lock()
        .map_err(|_| "Config write lock poisoned".to_string())?;

    let config_path = get_config_path();

    // Ensure parent directory exists
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    // Atomic write: temp file + rename to prevent corruption on crash
    let tmp_path = config_path.with_extension("tmp");
    fs::write(&tmp_path, content).map_err(|e| format!("Failed to write temp config: {}", e))?;
    fs::rename(&tmp_path, &config_path)
        .map_err(|e| format!("Failed to rename temp config: {}", e))?;

    tracing::info!("Cloud config saved to {:?}", config_path);
    Ok(())
}

/// Ensure the local cloud folder exists
pub fn ensure_cloud_folder(config: &CloudConfig) -> Result<PathBuf, String> {
    let path = &config.local_folder;

    if !path.exists() {
        fs::create_dir_all(path).map_err(|e| format!("Failed to create cloud folder: {}", e))?;
        tracing::info!("Created cloud folder: {:?}", path);
    }

    Ok(path.clone())
}

/// Protocol types that require a saved server profile with credentials
const SERVER_PROTOCOLS: &[&str] = &["ftp", "ftps", "sftp", "webdav"];

/// Protocol types that require absolute remote paths
const ABSOLUTE_PATH_PROTOCOLS: &[&str] = &["ftp", "ftps", "sftp", "webdav"];

/// Validate cloud configuration
pub fn validate_config(config: &CloudConfig) -> Result<(), String> {
    // Server-based protocols require a server_profile for credential lookup
    if SERVER_PROTOCOLS.contains(&config.protocol_type.as_str()) && config.server_profile.is_empty()
    {
        return Err("No server profile selected".to_string());
    }

    // OAuth2/cloud providers require connection_params with credentials
    let oauth_providers = [
        "googledrive",
        "dropbox",
        "onedrive",
        "box",
        "pcloud",
        "zohoworkdrive",
    ];
    if oauth_providers.contains(&config.protocol_type.as_str()) {
        let params = config.connection_params.as_object();
        if params.is_none()
            || params.is_some_and(|p| {
                p.get("client_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
            })
        {
            return Err("OAuth2 provider requires client_id in connection_params".to_string());
        }
    }

    if config.remote_folder.is_empty() {
        return Err("Remote folder cannot be empty".to_string());
    }

    // Only server protocols require absolute paths; cloud providers accept "/" or relative
    if ABSOLUTE_PATH_PROTOCOLS.contains(&config.protocol_type.as_str())
        && !config.remote_folder.starts_with('/')
    {
        return Err("Remote folder must be an absolute path for server protocols".to_string());
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

        // Should fail - no server profile (FTP default)
        assert!(validate_config(&config).is_err());

        config.server_profile = "MyServer".to_string();

        // Should pass now
        assert!(validate_config(&config).is_ok());

        // Should fail - empty remote folder
        config.remote_folder = String::new();
        assert!(validate_config(&config).is_err());

        // Should fail - relative path for FTP
        config.remote_folder = "cloud/".to_string();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_validate_config_cloud_providers() {
        // S3 does not require server_profile or absolute path
        let config = CloudConfig {
            protocol_type: "s3".to_string(),
            remote_folder: "my-prefix/".to_string(),
            connection_params: serde_json::json!({"bucket": "my-bucket", "region": "us-east-1"}),
            ..Default::default()
        };
        assert!(validate_config(&config).is_ok());

        // OAuth2 requires client_id
        let config_oauth_empty = CloudConfig {
            protocol_type: "googledrive".to_string(),
            remote_folder: "my-prefix/".to_string(),
            connection_params: serde_json::json!({}),
            ..Default::default()
        };
        assert!(validate_config(&config_oauth_empty).is_err());

        let config_oauth_valid = CloudConfig {
            protocol_type: "googledrive".to_string(),
            remote_folder: "my-prefix/".to_string(),
            connection_params: serde_json::json!({"client_id": "abc123", "client_secret": "secret"}),
            ..Default::default()
        };
        assert!(validate_config(&config_oauth_valid).is_ok());
    }

    #[test]
    fn test_default_protocol_type_backward_compat() {
        // Simulate loading an old config without protocol_type
        let json = r#"{"enabled":true,"local_folder":"/tmp","remote_folder":"/cloud/","server_profile":"MyFTP","sync_interval_secs":3600,"sync_on_change":true,"sync_on_startup":false,"exclude_patterns":[],"conflict_strategy":"ask_user"}"#;
        let config: CloudConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.protocol_type, "ftp");
        assert!(config.connection_params.is_null());
    }

    #[test]
    fn test_watcher_knobs_default() {
        let config = CloudConfig::default();
        assert_eq!(config.watcher_debounce_ms, 1500);
        assert_eq!(config.watcher_cooldown_secs, 30);
    }

    #[test]
    fn test_compress_fields_default_and_backward_compat() {
        let config = CloudConfig::default();
        assert!(!config.compress_enabled);
        assert_eq!(config.compress_level, 3);

        let json = r#"{"enabled":true,"local_folder":"/tmp","remote_folder":"/cloud/","server_profile":"MyFTP","sync_interval_secs":3600,"sync_on_change":true,"sync_on_startup":false,"exclude_patterns":[],"conflict_strategy":"ask_user"}"#;
        let parsed: CloudConfig = serde_json::from_str(json).unwrap();
        assert!(!parsed.compress_enabled);
        assert_eq!(parsed.compress_level, 3);
    }

    #[test]
    fn test_validate_compress_level() {
        assert!(validate_compress_level(1).is_ok());
        assert!(validate_compress_level(22).is_ok());
        assert!(validate_compress_level(0).is_err());
        assert!(validate_compress_level(23).is_err());
    }

    #[test]
    fn test_watcher_knobs_backward_compat() {
        // An old config predating the watcher knobs must deserialize with the
        // documented defaults, not fail or zero them out.
        let json = r#"{"enabled":true,"local_folder":"/tmp","remote_folder":"/cloud/","server_profile":"MyFTP","sync_interval_secs":3600,"sync_on_change":true,"sync_on_startup":false,"exclude_patterns":[],"conflict_strategy":"ask_user"}"#;
        let config: CloudConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.watcher_debounce_ms, 1500);
        assert_eq!(config.watcher_cooldown_secs, 30);
    }

    #[test]
    fn test_watcher_knobs_explicit_roundtrip() {
        // Explicit values (including a 0 cooldown, meaning "no cooldown") survive
        // a serialize/deserialize round-trip unchanged.
        let config = CloudConfig {
            watcher_debounce_ms: 3000,
            watcher_cooldown_secs: 0,
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: CloudConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.watcher_debounce_ms, 3000);
        assert_eq!(parsed.watcher_cooldown_secs, 0);
    }
}
