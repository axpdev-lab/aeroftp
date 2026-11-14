// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroCloud Configuration Module
// Persistent cloud sync configuration storage
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
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
    /// Stable id of that profile. Names are not unique, so a config that
    /// carries the id is resolved by it; `None` for a config written before
    /// the id was stored, which keeps resolving by exact name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_profile_id: Option<String>,
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
    /// One-way Update keeps newer destinations; deletion requires a proven Cloud
    /// transfer to this side and unchanged destination metadata. Old indices grant no proof.
    #[serde(default)]
    pub update_only: bool,
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
            server_profile_id: None,
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
            update_only: false,
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
pub(crate) fn get_config_path() -> PathBuf {
    crate::portable::aeroftp_data_root()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("cloud_config.json")
}

/// Load cloud configuration from disk
pub fn load_cloud_config() -> CloudConfig {
    load_cloud_config_unlocked()
}

fn load_cloud_config_unlocked() -> CloudConfig {
    let config_path = get_config_path();

    if config_path.exists() {
        match fs::read_to_string(&config_path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(config) => return config,
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse cloud config: {}",
                        crate::util::describe_json_error(&e)
                    );
                }
            },
            Err(e) => {
                tracing::warn!("Failed to read cloud config: {}", e);
            }
        }
    }

    CloudConfig::default()
}

/// Strict load for the write path at an explicit location: a missing file
/// is the default, a file that exists but cannot be read or parsed is an
/// error, so a mutator never treats it as empty and overwrites what it
/// could not read. `exists()` is deliberately not used: it reports false on
/// metadata errors too, which would pick the default and lose the file.
fn load_cloud_config_strict_at(path: &Path) -> Result<CloudConfig, String> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CloudConfig::default());
        }
        Err(e) => {
            return Err(format!(
                "Failed to read cloud config '{}': {}",
                path.display(),
                e
            ));
        }
    };
    serde_json::from_str(&content).map_err(|e| {
        format!(
            "Failed to parse cloud config '{}': {}",
            path.display(),
            crate::util::describe_json_error(&e)
        )
    })
}

/// Strict load at the real config path: callers that read-then-save outside
/// [`with_cloud_config_mut`] (CLI commands, one-shot tools) use this so an
/// unreadable file is an error instead of an empty config that a later save
/// would write over the user's data.
pub fn load_cloud_config_strict() -> Result<CloudConfig, String> {
    load_cloud_config_strict_at(&get_config_path())
}

/// Load → mutate → save under one write lock, at an explicit path.
///
/// Use this for every read-modify-write of `cloud_config.json`. Taking the lock
/// only around `save_cloud_config` still allows two concurrent mutators on the
/// blocking pool to both load the same snapshot and last-write-win (lost
/// update). The main-thread command path used to serialise this for free;
/// `spawn_blocking` does not.
pub fn with_cloud_config_mut_at<F, T>(path: &Path, f: F) -> Result<T, String>
where
    F: FnOnce(&mut CloudConfig) -> Result<T, String>,
{
    let _lock = CONFIG_WRITE_LOCK
        .lock()
        .map_err(|_| "Config write lock poisoned".to_string())?;
    let mut config = load_cloud_config_strict_at(path)?;
    let before = serde_json::to_value(&config).ok();
    let out = f(&mut config)?;
    // A mutation that changed nothing writes nothing: the file keeps its
    // bytes, and a save that would fail cannot fail a no-op.
    if before.is_none() || serde_json::to_value(&config).ok() != before {
        save_cloud_config_unlocked_at(&config, path)?;
    }
    Ok(out)
}

pub fn with_cloud_config_mut<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&mut CloudConfig) -> Result<T, String>,
{
    with_cloud_config_mut_at(&get_config_path(), f)
}

/// Save cloud configuration to disk (serialized via write lock, atomic temp+rename)
pub fn save_cloud_config(config: &CloudConfig) -> Result<(), String> {
    let _lock = CONFIG_WRITE_LOCK
        .lock()
        .map_err(|_| "Config write lock poisoned".to_string())?;
    save_cloud_config_unlocked(config)
}

/// Save that may overwrite even an unreadable file. ONLY for explicit resets
/// where starting from defaults is the user's intent: the setup wizard
/// (`setup_aerocloud`) and `disable_aerocloud`. Every other save goes through
/// [`save_cloud_config`], which refuses to destroy a file it cannot read.
pub fn save_cloud_config_unchecked(config: &CloudConfig) -> Result<(), String> {
    let _lock = CONFIG_WRITE_LOCK
        .lock()
        .map_err(|_| "Config write lock poisoned".to_string())?;
    save_cloud_config_unlocked_at_force(config, &get_config_path())
}

fn save_cloud_config_unlocked(config: &CloudConfig) -> Result<(), String> {
    save_cloud_config_unlocked_at(config, &get_config_path())
}

fn save_cloud_config_unlocked_at(config: &CloudConfig, config_path: &Path) -> Result<(), String> {
    // A full-file save built on a default config must never destroy a file
    // that exists but cannot be read or parsed (same rule as the strict
    // loaders: NotFound is the only "absent" that may be overwritten).
    match fs::read_to_string(config_path) {
        Ok(content) => {
            if serde_json::from_str::<CloudConfig>(&content).is_err() {
                return Err(format!(
                    "Refusing to overwrite unreadable cloud config '{}'",
                    config_path.display()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "Cannot read existing cloud config '{}': {}",
                config_path.display(),
                e
            ));
        }
    }
    save_cloud_config_unlocked_at_force(config, config_path)
}

fn save_cloud_config_unlocked_at_force(
    config: &CloudConfig,
    config_path: &Path,
) -> Result<(), String> {
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
    fs::rename(&tmp_path, config_path)
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

/// A remote folder the user wrote is used as written: joined to the login
/// folder, compared with the listing paths, the badges and the share link.
/// A `.`, `..` or empty step (a doubled slash) would name another folder
/// there, so every surface that saves one (the setup, the settings, the
/// pairs, the CLI) refuses it, as written, on every protocol: it is never
/// collapsed into the folder it might have meant
/// ([`crate::util::path_location`]).
pub fn refuse_an_unusable_remote_folder(remote_folder: &str) -> Result<(), String> {
    crate::util::path_location::refuse_an_unusable_location("remote folder", remote_folder)
}

/// Validate cloud configuration
pub fn validate_config(config: &CloudConfig) -> Result<(), String> {
    // Server-based protocols require a server_profile for credential lookup
    if SERVER_PROTOCOLS.contains(&config.protocol_type.as_str()) && config.server_profile.is_empty()
    {
        return Err("No server profile selected".to_string());
    }

    // OAuth2 providers need an OAuth app: the saved profile's (resolved from the
    // vault at connect time), or the one a config written by the old setup
    // wizard carries in connection_params.
    let oauth_providers = [
        "googledrive",
        "dropbox",
        "onedrive",
        "box",
        "pcloud",
        "zohoworkdrive",
    ];
    if oauth_providers.contains(&config.protocol_type.as_str()) && config.server_profile.is_empty()
    {
        let params = config.connection_params.as_object();
        if params.is_none()
            || params.is_some_and(|p| {
                p.get("client_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
            })
        {
            return Err(
                "OAuth2 provider requires a server profile or client_id in connection_params"
                    .to_string(),
            );
        }
    }

    if config.remote_folder.is_empty() {
        return Err("Remote folder cannot be empty".to_string());
    }

    // Only server protocols require absolute paths; cloud providers accept "/" or relative.
    // A `~/...` folder is accepted on the login-relative protocols (SFTP, SCP,
    // FTP, FTPS): it names a folder under the login folder and is placed to an
    // absolute path on the first connect. WebDAV and a bare relative stay refused.
    if ABSOLUTE_PATH_PROTOCOLS.contains(&config.protocol_type.as_str())
        && !config.remote_folder.starts_with('/')
        && !(crate::crypt_overlay_provider::is_login_relative_scope(&config.remote_folder)
            && crate::crypt_overlay_provider::login_relative_protocol(&config.protocol_type))
    {
        return Err("Remote folder must be an absolute path for server protocols".to_string());
    }
    refuse_an_unusable_remote_folder(&config.remote_folder)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `cloud_config.json` that exists but does not parse is an error on
    /// the write path: the mutator reports it and the file bytes stay
    /// untouched, never overwritten by an empty config.
    /// A mutation that changes nothing writes nothing: the file keeps its
    /// exact bytes (a save would rewrite them in its own layout). A change
    /// is still saved.
    #[test]
    fn a_mutation_that_changes_nothing_leaves_the_config_file_alone() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_config.json");
        let compact = serde_json::to_string(&CloudConfig::default()).unwrap();
        std::fs::write(&path, &compact).unwrap();
        with_cloud_config_mut_at(&path, |_| Ok(())).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), compact);
        with_cloud_config_mut_at(&path, |config| {
            config.remote_folder = "/placed".to_string();
            Ok(())
        })
        .unwrap();
        assert_eq!(
            load_cloud_config_strict_at(&path).unwrap().remote_folder,
            "/placed"
        );
    }

    #[test]
    fn an_unparseable_config_is_never_overwritten() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_config.json");
        let bytes = b"{ not json".as_slice();
        std::fs::write(&path, bytes).unwrap();
        let err = with_cloud_config_mut_at(&path, |_| Ok(())).unwrap_err();
        assert!(err.contains("parse"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    /// A full-file save must refuse to overwrite a config that exists but
    /// does not parse: the GUI/settings save path would otherwise turn an
    /// unreadable file into defaults.
    /// The reset path really overwrites a config the guarded save refuses:
    /// the setup wizard and `disable_aerocloud` must still work when the
    /// A parse error names the kind and position, never the value: a
    /// credential in a mistyped field must not reach the log or the webview.
    #[test]
    fn a_parse_error_does_not_quote_the_config_values() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_config.json");
        std::fs::write(&path, r#"{"enabled": "SENTINEL-SECRET"}"#).unwrap();
        let err = load_cloud_config_strict_at(&path).unwrap_err();
        assert!(err.contains("parse"), "{err}");
        assert!(!err.contains("SENTINEL-SECRET"), "{err}");
    }

    /// file is corrupt, since resetting it is how the user recovers.
    #[test]
    fn the_reset_save_overwrites_an_unparseable_config() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_config.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(save_cloud_config_unlocked_at(&CloudConfig::default(), &path).is_err());
        save_cloud_config_unlocked_at_force(&CloudConfig::default(), &path).unwrap();
        assert!(load_cloud_config_strict_at(&path).is_ok());
    }

    /// Only the two explicit resets may bypass the guard: every caller of
    /// `save_cloud_config_unchecked` in the crate sits inside
    /// `setup_aerocloud` or `disable_aerocloud`. A new caller elsewhere
    /// would silently reopen the overwrite-an-unreadable-file path.
    #[test]
    fn only_the_explicit_resets_call_the_unchecked_save() {
        fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    rust_files(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        let mut callers = std::collections::BTreeSet::new();
        for file in files {
            if file.file_name().is_some_and(|n| n == "cloud_config.rs") {
                continue;
            }
            let code = std::fs::read_to_string(&file).unwrap();
            for (at, _) in code.match_indices("save_cloud_config_unchecked(") {
                let before = &code[..at];
                let fn_at = before.rfind("fn ").expect("call outside any fn");
                let name: String = before[fn_at + 3..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                callers.insert(name);
            }
        }
        let expected: std::collections::BTreeSet<String> = ["disable_aerocloud", "setup_aerocloud"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(callers, expected);
    }

    #[test]
    fn a_save_refuses_to_overwrite_an_unparseable_config() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_config.json");
        let bytes = b"{ not json".as_slice();
        std::fs::write(&path, bytes).unwrap();
        let err = save_cloud_config_unlocked_at(&CloudConfig::default(), &path).unwrap_err();
        assert!(err.contains("Refusing to overwrite"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        // A parseable existing config may still be overwritten.
        let good = serde_json::to_vec(&CloudConfig::default()).unwrap();
        std::fs::write(&path, &good).unwrap();
        save_cloud_config_unlocked_at(&CloudConfig::default(), &path).unwrap();
        assert_ne!(std::fs::read(&path).unwrap(), bytes);
    }

    /// Same guard on a metadata error: an unreadable FILE (permissions
    /// removed from the file itself, directory still writable) makes the
    /// save fail instead of overwriting it through tmp+rename.
    #[cfg(unix)]
    #[test]
    fn a_save_refuses_when_the_config_file_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("running as root: chmod cannot block the write, skipping");
            return;
        }
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_config.json");
        let bytes = b"{ \"enabled\": true }".as_slice();
        std::fs::write(&path, bytes).unwrap();
        struct PermGuard(std::path::PathBuf);
        impl Drop for PermGuard {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o644));
            }
        }
        let _guard = PermGuard(path.clone());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(save_cloud_config_unlocked_at(&CloudConfig::default(), &path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    /// An absent `cloud_config.json` still takes the default-plus-save path.
    #[test]
    fn an_absent_config_still_saves() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_config.json");
        with_cloud_config_mut_at(&path, |config| {
            config.enabled = true;
            Ok(())
        })
        .unwrap();
        let saved: CloudConfig =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(saved.enabled);
    }

    /// A metadata error on the config (unreadable parent directory) is an
    /// error, not a missing file: the closure never runs and nothing is
    /// written back.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_config_dir_is_an_error_not_a_default() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("running as root: chmod cannot block the read, skipping");
            return;
        }
        let data = tempfile::tempdir().unwrap();
        let dir = data.path().join("cfg");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("cloud_config.json");
        let bytes = b"{ \"enabled\": true }".as_slice();
        std::fs::write(&path, bytes).unwrap();
        struct PermGuard(std::path::PathBuf);
        impl Drop for PermGuard {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let _guard = PermGuard(dir.clone());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let mut ran = false;
        let err = with_cloud_config_mut_at(&path, |_| {
            ran = true;
            Ok(())
        })
        .unwrap_err();
        assert!(!ran, "closure must not run on an unreadable config: {err}");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

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

    /// A `..` under the login folder is refused where a folder is saved, in
    /// every spelling, and kept as written in the message; an absolute
    /// folder, or one on a protocol that does not open under the login
    /// folder, is not this rule's.
    #[test]
    fn a_folder_that_is_not_plain_is_refused_on_every_protocol() {
        for protocol in ["sftp", "scp", "ftp", "ftps", "webdav", "s3", "googledrive"] {
            for folder in [
                "~/..",
                "~/docs/../shared",
                "docs/../x",
                "~/a\\..\\b",
                "/srv/../etc",
                "/docs/./a",
                "~//docs",
                "/docs//a",
            ] {
                let config = CloudConfig {
                    server_profile: "nas".to_string(),
                    protocol_type: protocol.to_string(),
                    remote_folder: folder.to_string(),
                    connection_params: serde_json::json!({"bucket": "b", "client_id": "c"}),
                    ..CloudConfig::default()
                };
                assert!(validate_config(&config).is_err(), "{protocol} {folder}");
                let err = refuse_an_unusable_remote_folder(folder).unwrap_err();
                assert!(err.contains(&format!("'{folder}'")), "{err}");
            }
            for folder in ["~/docs", "~/docs..x", "~/..docs", "/srv/data/", "/", "~"] {
                assert!(
                    refuse_an_unusable_remote_folder(folder).is_ok(),
                    "{protocol} {folder}"
                );
            }
        }
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

        // A saved OAuth profile carries its app: no params needed.
        let config_oauth_profile = CloudConfig {
            protocol_type: "zohoworkdrive".to_string(),
            server_profile: "MyZoho".to_string(),
            remote_folder: "/cloud/".to_string(),
            connection_params: serde_json::Value::Null,
            ..Default::default()
        };
        assert!(
            validate_config(&config_oauth_profile).is_ok(),
            "an OAuth profile must be accepted without inline client credentials"
        );
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
