// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroCloud Pairs Store (Q1 separate from AeroSync)
// cloud_pairs.json + CloudPairsConfig + CloudPathPair
// Never reuses MultiPathConfig / PathPair / multi_path.json (owned by AeroSync).

#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use crate::cloud_config::{CloudConfig, ConflictStrategy};
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
fn get_pairs_path() -> PathBuf {
    crate::portable::aeroftp_data_root()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("cloud_pairs.json")
}

/// Load pairs config (absent file => empty pairs, back-compat path).
pub fn load_cloud_pairs_config() -> CloudPairsConfig {
    let path = get_pairs_path();
    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(cfg) => return cfg,
                Err(e) => {
                    tracing::warn!("Failed to parse cloud pairs: {}", e);
                }
            },
            Err(e) => {
                tracing::warn!("Failed to read cloud pairs: {}", e);
            }
        }
    }
    CloudPairsConfig::default()
}

/// Save pairs config (atomic + lock protected).
pub fn save_cloud_pairs_config(config: &CloudPairsConfig) -> Result<(), String> {
    let _lock = PAIRS_WRITE_LOCK
        .lock()
        .map_err(|_| "Pairs write lock poisoned".to_string())?;

    let path = get_pairs_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create pairs dir: {}", e))?;
    }

    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize pairs: {}", e))?;

    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content).map_err(|e| format!("Failed to write temp pairs: {}", e))?;
    fs::rename(&tmp, &path).map_err(|e| format!("Failed to rename temp pairs: {}", e))?;

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
        conflict_strategy: ConflictStrategy::default(),
        versioning_strategy: VersioningStrategy::default(),
        excluded_folders: Vec::new(),
        exclude_patterns: Vec::new(),
        last_sync: None,
    }
}
