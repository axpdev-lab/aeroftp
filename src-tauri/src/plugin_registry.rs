//! Plugin Registry — fetches and installs plugins from a remote GitHub-based registry.
//!
//! Registry URL: https://raw.githubusercontent.com/axpdev-lab/aeroftp-plugins/main/registry.json
//! Plugins are downloaded, integrity-verified (SHA-256), and installed to ~/.config/aeroftp/plugins/.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use crate::plugins::PluginManifest;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::Manager;

#[allow(dead_code)]
const REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/axpdev-lab/aeroftp-plugins/main/registry.json";
#[allow(dead_code)]
const CACHE_TTL_SECS: u64 = 3600; // 1 hour
#[allow(dead_code)]
const FETCH_TIMEOUT_SECS: u64 = 30;
const REMOTE_PLUGIN_REGISTRY_DISABLED_REASON: &str =
    "Remote plugin registry is temporarily disabled until AeroFTP verifies a signed registry client-side. Local installed plugins continue to work.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryFile {
    /// Relative path within the plugin directory (e.g., "run.sh")
    pub path: String,
    /// Raw download URL
    pub url: String,
    /// Expected SHA-256 hex digest
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    /// Category: "file-management", "ai-tools", "automation", "integration"
    pub category: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub stars: u32,
    /// GitHub repo URL of the plugin
    pub repo_url: String,
    /// Raw URL to the plugin.json manifest
    pub manifest_url: String,
    /// Files to download (scripts, assets)
    pub files: Vec<RegistryFile>,
}

/// In-memory cache for the registry
#[allow(dead_code)]
struct RegistryCache {
    entries: Vec<RegistryEntry>,
    fetched_at: Instant,
}

#[allow(dead_code)]
static REGISTRY_CACHE: Mutex<Option<RegistryCache>> = Mutex::new(None);

#[allow(dead_code)]
fn http_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()
        .unwrap_or_default()
}

/// Fetch the plugin registry (cached for 1 hour)
#[tauri::command]
pub async fn fetch_plugin_registry() -> Result<Vec<RegistryEntry>, String> {
    Err(REMOTE_PLUGIN_REGISTRY_DISABLED_REASON.to_string())
}

/// Get the plugins directory path
#[allow(dead_code)]
fn plugins_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Failed to resolve app config dir: {e}"))?
        .join("plugins"))
}

/// Install a plugin from the registry by its ID
#[tauri::command]
pub async fn install_plugin_from_registry(
    app: tauri::AppHandle,
    plugin_id: String,
) -> Result<PluginManifest, String> {
    let _ = app;
    let _ = plugin_id;
    Err(REMOTE_PLUGIN_REGISTRY_DISABLED_REASON.to_string())
}
