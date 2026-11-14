// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroCloud Pairs Store (Q1 separate from AeroSync)
// cloud_pairs.json + CloudPairsConfig + CloudPathPair
// Never reuses MultiPathConfig / PathPair / multi_path.json (owned by AeroSync).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use crate::cloud_config::{default_compress_level, CloudConfig, ConflictStrategy};
use crate::sync::CompareDirection;
use crate::sync_versioning::VersioningStrategy;

/// Serializes concurrent writes to the cloud pairs file
static PAIRS_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// One sync pair for AeroCloud (local <-> remote under one profile).
/// Carries its own credentials profile, direction, excludes, strategies etc.
/// This is AeroCloud's unit (distinct from AeroSync's PathPair).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudPathPair {
    pub id: String,
    pub name: String,
    pub local_path: PathBuf,
    pub remote_path: String,
    pub enabled: bool,

    /// Name of the saved server profile (from vault)
    #[serde(default)]
    pub server_profile: String,

    /// "ftp", "sftp", "mega", etc. Resolved at sync time for CLI-created pairs.
    #[serde(default = "default_protocol_type")]
    pub protocol_type: String,

    /// Protocol-specific params (e.g. S3 bucket). Empty for profile-driven.
    #[serde(default)]
    pub connection_params: serde_json::Value,

    #[serde(default)]
    pub sync_direction: CompareDirection,

    #[serde(default = "default_preserve_remote_deletes")]
    pub preserve_remote_deletes: bool,
    /// One-way Update keeps newer destinations; deletion requires a proven Cloud
    /// transfer to this side and unchanged destination metadata. Old indices grant no proof.
    #[serde(default)]
    pub update_only: bool,

    #[serde(default)]
    pub compress_enabled: bool,

    #[serde(default = "default_compress_level")]
    pub compress_level: i32,

    #[serde(default)]
    pub conflict_strategy: ConflictStrategy,

    #[serde(default)]
    pub versioning_strategy: VersioningStrategy,

    /// Remote subfolders to skip (relative to remote_path).
    #[serde(default)]
    pub excluded_folders: Vec<String>,

    /// Local exclude patterns (mirrors CloudConfig exclude_patterns for the pair).
    #[serde(default)]
    pub exclude_patterns: Vec<String>,

    /// Per-pair last successful sync (for future per-pair telemetry / UI).
    pub last_sync: Option<DateTime<Utc>>,
}

fn default_protocol_type() -> String {
    "ftp".to_string()
}

fn default_preserve_remote_deletes() -> bool {
    true
}

impl CloudPathPair {
    /// Build from a legacy single CloudConfig (for seeding the first pair
    /// or one-time migration). Does not write the pairs file.
    pub fn from_legacy(legacy: &CloudConfig) -> Self {
        let name = if legacy.cloud_name.trim().is_empty() {
            "Primary".to_string()
        } else {
            legacy.cloud_name.clone()
        };
        CloudPathPair {
            id: "legacy-0".to_string(),
            name,
            local_path: legacy.local_folder.clone(),
            remote_path: legacy.remote_folder.clone(),
            enabled: legacy.enabled,
            server_profile: legacy.server_profile.clone(),
            protocol_type: legacy.protocol_type.clone(),
            connection_params: legacy.connection_params.clone(),
            sync_direction: legacy.sync_direction,
            preserve_remote_deletes: legacy.preserve_remote_deletes,
            update_only: legacy.update_only,
            compress_enabled: legacy.compress_enabled,
            compress_level: legacy.compress_level,
            conflict_strategy: legacy.conflict_strategy.clone(),
            versioning_strategy: legacy.versioning_strategy.clone(),
            excluded_folders: legacy.excluded_folders.clone(),
            exclude_patterns: legacy.exclude_patterns.clone(),
            last_sync: legacy.last_sync,
        }
    }

    /// Convert this pair into a CloudConfig that the existing sync engine
    /// (CloudService, sync_one_config, providers, index etc) can consume
    /// unchanged. Non-per-pair fields fall back to CloudConfig defaults.
    pub fn to_cloud_config(&self) -> CloudConfig {
        CloudConfig {
            enabled: self.enabled,
            // paused and scheduling flags remain under the global/legacy config
            // for v1 (global schedule + per-pair enable)
            local_folder: self.local_path.clone(),
            remote_folder: self.remote_path.clone(),
            server_profile: self.server_profile.clone(),
            sync_interval_secs: 86400,
            sync_on_change: true,
            sync_on_startup: false,
            exclude_patterns: if self.exclude_patterns.is_empty() {
                // fall back to the well-known defaults that CloudConfig::default sets
                CloudConfig::default().exclude_patterns
            } else {
                self.exclude_patterns.clone()
            },
            last_sync: self.last_sync,
            conflict_strategy: self.conflict_strategy.clone(),
            public_url_base: None,
            sync_direction: self.sync_direction,
            preserve_remote_deletes: self.preserve_remote_deletes,
            update_only: self.update_only,
            compress_enabled: self.compress_enabled,
            compress_level: self.compress_level,
            protocol_type: self.protocol_type.clone(),
            connection_params: self.connection_params.clone(),
            excluded_folders: self.excluded_folders.clone(),
            versioning_strategy: self.versioning_strategy.clone(),
            // other fields (paused, cloud_name) left at default
            ..CloudConfig::default()
        }
    }
}

/// Config file holding 0..N AeroCloud pairs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CloudPairsConfig {
    #[serde(default)]
    pub pairs: Vec<CloudPathPair>,
    /// Hint for future parallel execution (default serial for v1).
    #[serde(default)]
    pub parallel_pairs: bool,
}

/// Path to cloud_pairs.json (sibling to cloud_config.json)
pub(crate) fn get_pairs_path() -> PathBuf {
    crate::portable::aeroftp_data_root()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("cloud_pairs.json")
}

/// Load pairs config (absent file => empty pairs, back-compat path).
pub fn load_cloud_pairs_config() -> CloudPairsConfig {
    load_cloud_pairs_config_unlocked()
}

fn load_cloud_pairs_config_unlocked() -> CloudPairsConfig {
    let path = get_pairs_path();
    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(cfg) => return cfg,
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse cloud pairs: {}",
                        crate::util::describe_json_error(&e)
                    );
                }
            },
            Err(e) => {
                tracing::warn!("Failed to read cloud pairs: {}", e);
            }
        }
    }
    CloudPairsConfig::default()
}

/// Strict load for the write path at an explicit location: a missing file
/// is the default, a file that exists but cannot be read or parsed is an
/// error, so a mutator never treats it as empty and overwrites what it
/// could not read. `exists()` is deliberately not used: it reports false on
/// metadata errors too, which would pick the default and lose the file.
fn load_cloud_pairs_config_strict_at(path: &Path) -> Result<CloudPairsConfig, String> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CloudPairsConfig::default());
        }
        Err(e) => {
            return Err(format!(
                "Failed to read cloud pairs '{}': {}",
                path.display(),
                e
            ));
        }
    };
    serde_json::from_str(&content).map_err(|e| {
        format!(
            "Failed to parse cloud pairs '{}': {}",
            path.display(),
            crate::util::describe_json_error(&e)
        )
    })
}

/// Strict load at the real pairs path: callers that read-then-save outside
/// [`with_cloud_pairs_mut`] (CLI commands, one-shot tools) use this so an
/// unreadable file is an error instead of an empty config that a later save
/// would write over the user's data.
pub fn load_cloud_pairs_config_strict() -> Result<CloudPairsConfig, String> {
    load_cloud_pairs_config_strict_at(&get_pairs_path())
}

/// Load → mutate → save under one write lock (see `cloud_config::with_cloud_config_mut`),
/// at an explicit path.
pub fn with_cloud_pairs_mut_at<F, T>(path: &Path, f: F) -> Result<T, String>
where
    F: FnOnce(&mut CloudPairsConfig) -> Result<T, String>,
{
    let _lock = PAIRS_WRITE_LOCK
        .lock()
        .map_err(|_| "Pairs write lock poisoned".to_string())?;
    let mut config = load_cloud_pairs_config_strict_at(path)?;
    let before = serde_json::to_value(&config).ok();
    let out = f(&mut config)?;
    // A mutation that changed nothing writes nothing: the file keeps its
    // bytes, and a save that would fail cannot fail a no-op.
    if before.is_none() || serde_json::to_value(&config).ok() != before {
        save_cloud_pairs_config_unlocked_at(&config, path)?;
    }
    Ok(out)
}

pub fn with_cloud_pairs_mut<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&mut CloudPairsConfig) -> Result<T, String>,
{
    with_cloud_pairs_mut_at(&get_pairs_path(), f)
}

/// Save pairs config (atomic + lock protected).
pub fn save_cloud_pairs_config(config: &CloudPairsConfig) -> Result<(), String> {
    let _lock = PAIRS_WRITE_LOCK
        .lock()
        .map_err(|_| "Pairs write lock poisoned".to_string())?;
    save_cloud_pairs_config_unlocked(config)
}

fn save_cloud_pairs_config_unlocked(config: &CloudPairsConfig) -> Result<(), String> {
    save_cloud_pairs_config_unlocked_at(config, &get_pairs_path())
}

fn save_cloud_pairs_config_unlocked_at(
    config: &CloudPairsConfig,
    path: &Path,
) -> Result<(), String> {
    // A full-file save built on a default config must never destroy a file
    // that exists but cannot be read or parsed (same rule as the strict
    // loaders: NotFound is the only "absent" that may be overwritten).
    match fs::read_to_string(path) {
        Ok(content) => {
            if serde_json::from_str::<CloudPairsConfig>(&content).is_err() {
                return Err(format!(
                    "Refusing to overwrite unreadable cloud pairs '{}'",
                    path.display()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "Cannot read existing cloud pairs '{}': {}",
                path.display(),
                e
            ));
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create pairs dir: {}", e))?;
    }

    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize pairs: {}", e))?;

    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content).map_err(|e| format!("Failed to write temp pairs: {}", e))?;
    fs::rename(&tmp, path).map_err(|e| format!("Failed to rename temp pairs: {}", e))?;

    tracing::info!("Cloud pairs saved to {:?}", path);
    Ok(())
}

/// Return true if a (local,remote) pair already exists in the list.
/// Used as guard on add to avoid index collision (same pair twice shares baseline).
pub fn pair_exists(local: &std::path::Path, remote: &str, pairs: &[CloudPathPair]) -> bool {
    pairs
        .iter()
        .any(|p| p.local_path == local && p.remote_path == remote)
}

/// Create a new pair entry (no persist). id is caller-provided or simple.
pub fn new_cloud_path_pair(
    id: String,
    name: String,
    local: PathBuf,
    remote: String,
    profile: String,
) -> CloudPathPair {
    CloudPathPair {
        id,
        name,
        local_path: local,
        remote_path: remote,
        enabled: true,
        server_profile: profile,
        protocol_type: default_protocol_type(),
        connection_params: serde_json::Value::Null,
        sync_direction: CompareDirection::Bidirectional,
        preserve_remote_deletes: default_preserve_remote_deletes(),
        update_only: false,
        compress_enabled: false,
        compress_level: default_compress_level(),
        conflict_strategy: ConflictStrategy::default(),
        versioning_strategy: VersioningStrategy::default(),
        excluded_folders: Vec::new(),
        exclude_patterns: Vec::new(),
        last_sync: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `cloud_pairs.json` that exists but does not parse is an error on
    /// the write path: the mutator reports it and the file bytes stay
    /// untouched, never overwritten by an empty config.
    #[test]
    fn an_unparseable_pairs_file_is_never_overwritten() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_pairs.json");
        let bytes = b"{ not json".as_slice();
        std::fs::write(&path, bytes).unwrap();
        let err = with_cloud_pairs_mut_at(&path, |_| Ok(())).unwrap_err();
        assert!(err.contains("parse"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    /// A mutation that changes nothing writes nothing: the file keeps its
    /// exact bytes (a save would rewrite them in its own layout). A change
    /// is still saved.
    #[test]
    fn a_mutation_that_changes_nothing_leaves_the_pairs_file_alone() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_pairs.json");
        let bytes = br#"{"pairs":[],"parallel_pairs":false}"#.as_slice();
        std::fs::write(&path, bytes).unwrap();
        with_cloud_pairs_mut_at(&path, |_| Ok(())).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        with_cloud_pairs_mut_at(&path, |config| {
            config.parallel_pairs = true;
            Ok(())
        })
        .unwrap();
        assert!(
            load_cloud_pairs_config_strict_at(&path)
                .unwrap()
                .parallel_pairs
        );
    }

    /// A parse error names the kind and position, never the value: a
    /// credential in a mistyped field must not reach the log or the webview.
    #[test]
    fn a_parse_error_does_not_quote_the_pairs_values() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_pairs.json");
        std::fs::write(
            &path,
            r#"{"pairs": [], "parallel_pairs": "SENTINEL-SECRET"}"#,
        )
        .unwrap();
        let err = load_cloud_pairs_config_strict_at(&path).unwrap_err();
        assert!(err.contains("parse"), "{err}");
        assert!(!err.contains("SENTINEL-SECRET"), "{err}");
    }

    /// A full-file save must refuse to overwrite pairs that exist but do
    /// not parse.
    #[test]
    fn a_save_refuses_to_overwrite_unparseable_pairs() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_pairs.json");
        let bytes = b"{ not json".as_slice();
        std::fs::write(&path, bytes).unwrap();
        let err =
            save_cloud_pairs_config_unlocked_at(&CloudPairsConfig::default(), &path).unwrap_err();
        assert!(err.contains("Refusing to overwrite"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    /// Same guard on a metadata error: an unreadable FILE (permissions
    /// removed from the file itself, directory still writable) makes the
    /// save fail instead of overwriting it through tmp+rename.
    #[cfg(unix)]
    #[test]
    fn a_save_refuses_when_the_pairs_file_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("running as root: chmod cannot block the write, skipping");
            return;
        }
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_pairs.json");
        let bytes = b"{ \"parallel_pairs\": true }".as_slice();
        std::fs::write(&path, bytes).unwrap();
        struct PermGuard(std::path::PathBuf);
        impl Drop for PermGuard {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o644));
            }
        }
        let _guard = PermGuard(path.clone());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(save_cloud_pairs_config_unlocked_at(&CloudPairsConfig::default(), &path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    /// An absent `cloud_pairs.json` still takes the default-plus-save path.
    #[test]
    fn an_absent_pairs_file_still_saves() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("cloud_pairs.json");
        with_cloud_pairs_mut_at(&path, |config| {
            config.parallel_pairs = true;
            Ok(())
        })
        .unwrap();
        let saved: CloudPairsConfig =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(saved.parallel_pairs);
    }

    /// A metadata error on the pairs file (unreadable parent directory) is
    /// an error, not a missing file: the closure never runs.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_pairs_dir_is_an_error_not_a_default() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("running as root: chmod cannot block the read, skipping");
            return;
        }
        let data = tempfile::tempdir().unwrap();
        let dir = data.path().join("cfg");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("cloud_pairs.json");
        let bytes = b"{ \"parallel_pairs\": true }".as_slice();
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
        let err = with_cloud_pairs_mut_at(&path, |_| {
            ran = true;
            Ok(())
        })
        .unwrap_err();
        assert!(
            !ran,
            "closure must not run on an unreadable pairs file: {err}"
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn test_pair_exists_guard() {
        let p1 = CloudPathPair {
            id: "1".into(),
            name: "a".into(),
            local_path: "/tmp/l1".into(),
            remote_path: "/r1".into(),
            enabled: true,
            server_profile: "p".into(),
            protocol_type: "ftp".into(),
            connection_params: serde_json::Value::Null,
            sync_direction: CompareDirection::Bidirectional,
            preserve_remote_deletes: true,
            update_only: false,
            compress_enabled: false,
            compress_level: default_compress_level(),
            conflict_strategy: ConflictStrategy::default(),
            versioning_strategy: VersioningStrategy::default(),
            excluded_folders: vec![],
            exclude_patterns: vec![],
            last_sync: None,
        };
        let mut cfg = CloudPairsConfig {
            pairs: vec![p1.clone()],
            parallel_pairs: false,
        };
        assert!(pair_exists(&p1.local_path, &p1.remote_path, &cfg.pairs));
        cfg.pairs.clear();
        assert!(!pair_exists(&p1.local_path, &p1.remote_path, &cfg.pairs));
    }

    #[test]
    fn test_from_legacy_and_to_cloud_config_roundtrip() {
        let leg = CloudConfig {
            enabled: true,
            local_folder: "/tmp/leg".into(),
            remote_folder: "/rleg".into(),
            server_profile: "legprof".into(),
            protocol_type: "sftp".into(),
            sync_direction: CompareDirection::LocalToRemote,
            preserve_remote_deletes: false,
            update_only: true,
            compress_enabled: true,
            compress_level: 9,
            exclude_patterns: vec!["*.tmp".into()],
            ..CloudConfig::default()
        };
        let pair = CloudPathPair::from_legacy(&leg);
        assert_eq!(pair.server_profile, "legprof");
        assert_eq!(pair.protocol_type, "sftp");
        assert_eq!(pair.sync_direction, CompareDirection::LocalToRemote);
        assert!(!pair.preserve_remote_deletes);
        assert!(pair.update_only);
        assert!(pair.to_cloud_config().update_only);
        assert!(pair.compress_enabled);
        assert_eq!(pair.compress_level, 9);
        assert_eq!(pair.exclude_patterns, vec!["*.tmp".to_string()]);

        let back = pair.to_cloud_config();
        assert!(back.enabled);
        assert_eq!(back.server_profile, "legprof");
        assert_eq!(back.protocol_type, "sftp");
        assert!(back.compress_enabled);
        assert_eq!(back.compress_level, 9);
        // exclude fallback exercised
        assert!(!back.exclude_patterns.is_empty());
    }

    #[test]
    fn test_new_cloud_path_pair_defaults() {
        let p = new_cloud_path_pair(
            "id1".into(),
            "n".into(),
            "/l".into(),
            "/r".into(),
            "prof".into(),
        );
        assert_eq!(p.protocol_type, "ftp");
        assert!(p.enabled);
        assert_eq!(p.sync_direction, CompareDirection::Bidirectional);
        assert!(!p.compress_enabled);
        assert_eq!(p.compress_level, 3);
    }

    #[test]
    fn test_to_cloud_config_exclude_fallback() {
        let mut p =
            new_cloud_path_pair("x".into(), "x".into(), "/l".into(), "/r".into(), "p".into());
        p.exclude_patterns.clear();
        let c = p.to_cloud_config();
        // should have fallen back to CloudConfig default excludes (non empty)
        assert!(!c.exclude_patterns.is_empty());
    }

    #[test]
    fn test_to_cloud_config_carries_compress_overlay_fields() {
        let mut p =
            new_cloud_path_pair("x".into(), "x".into(), "/l".into(), "/r".into(), "p".into());
        p.compress_enabled = true;
        p.compress_level = 11;

        let c = p.to_cloud_config();
        assert!(c.compress_enabled);
        assert_eq!(c.compress_level, 11);
    }
}
