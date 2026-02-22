//! Plugin Registry — fetches and installs plugins from a remote GitHub-based registry.
//!
//! Registry URL: https://raw.githubusercontent.com/axpnet/aeroftp-plugins/main/registry.json
//! Plugins are downloaded, integrity-verified (SHA-256), and installed to ~/.config/aeroftp/plugins/.

use crate::plugins::PluginManifest;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::Manager;
use tracing::info;

const REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/axpnet/aeroftp-plugins/main/registry.json";
const CACHE_TTL_SECS: u64 = 3600; // 1 hour
const FETCH_TIMEOUT_SECS: u64 = 30;

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
struct RegistryCache {
    entries: Vec<RegistryEntry>,
    fetched_at: Instant,
}

static REGISTRY_CACHE: Mutex<Option<RegistryCache>> = Mutex::new(None);

fn http_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()
        .unwrap_or_default()
}

/// Fetch the plugin registry (cached for 1 hour)
#[tauri::command]
pub async fn fetch_plugin_registry() -> Result<Vec<RegistryEntry>, String> {
    // Check cache
    {
        let cache = REGISTRY_CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(ref c) = *cache {
            if c.fetched_at.elapsed() < Duration::from_secs(CACHE_TTL_SECS) {
                return Ok(c.entries.clone());
            }
        }
    }

    // Fetch from remote
    info!("Fetching plugin registry from {}", REGISTRY_URL);
    let client = http_client();
    let resp = client
        .get(REGISTRY_URL)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch registry: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("Registry returned HTTP {}", resp.status()));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read registry body: {e}"))?;

    let entries: Vec<RegistryEntry> =
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse registry JSON: {e}"))?;

    // Update cache
    {
        let mut cache = REGISTRY_CACHE.lock().unwrap_or_else(|p| p.into_inner());
        *cache = Some(RegistryCache {
            entries: entries.clone(),
            fetched_at: Instant::now(),
        });
    }

    info!("Registry loaded: {} plugins available", entries.len());
    Ok(entries)
}

/// Get the plugins directory path
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
    // Validate plugin ID (alphanumeric + hyphens only)
    if !plugin_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Invalid plugin ID: only alphanumeric, hyphens, underscores allowed".into());
    }

    // Find in registry
    let entries = fetch_plugin_registry().await?;
    let entry = entries
        .iter()
        .find(|e| e.id == plugin_id)
        .ok_or_else(|| format!("Plugin '{}' not found in registry", plugin_id))?;

    let plugin_dir = plugins_dir(&app)?.join(&plugin_id);

    // Create plugin directory
    std::fs::create_dir_all(&plugin_dir)
        .map_err(|e| format!("Failed to create plugin directory: {e}"))?;

    let client = http_client();

    // Download and verify manifest
    info!("Downloading manifest for plugin '{}'", plugin_id);
    let manifest_resp = client
        .get(&entry.manifest_url)
        .send()
        .await
        .map_err(|e| format!("Failed to download manifest: {e}"))?;

    let manifest_text = manifest_resp
        .text()
        .await
        .map_err(|e| format!("Failed to read manifest: {e}"))?;

    let manifest: PluginManifest = serde_json::from_str(&manifest_text)
        .map_err(|e| format!("Failed to parse plugin manifest: {e}"))?;

    // Download and verify each file
    for file in &entry.files {
        info!("Downloading plugin file: {}", file.path);

        // Validate path — no traversal
        if file.path.contains("..") || file.path.starts_with('/') {
            return Err(format!("Invalid file path in registry: {}", file.path));
        }

        let file_resp = client
            .get(&file.url)
            .send()
            .await
            .map_err(|e| format!("Failed to download '{}': {e}", file.path))?;

        let data = file_resp
            .bytes()
            .await
            .map_err(|e| format!("Failed to read '{}': {e}", file.path))?;

        // Verify SHA-256 integrity
        let hash = format!("{:x}", Sha256::digest(&data));
        if hash != file.sha256 {
            // Cleanup
            let _ = std::fs::remove_dir_all(&plugin_dir);
            return Err(format!(
                "Integrity check failed for '{}': expected {}, got {}",
                file.path,
                &file.sha256[..12],
                &hash[..12]
            ));
        }

        // Write file
        let target_path = plugin_dir.join(&file.path);
        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory for '{}': {e}", file.path))?;
        }
        std::fs::write(&target_path, &data)
            .map_err(|e| format!("Failed to write '{}': {e}", file.path))?;

        // Set executable permission on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if file.path.ends_with(".sh") || file.path.ends_with(".py") {
                let _ = std::fs::set_permissions(&target_path, std::fs::Permissions::from_mode(0o755));
            }
        }
    }

    // Write manifest with integrity hashes
    let manifest_path = plugin_dir.join("plugin.json");
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("Failed to serialize manifest: {e}"))?;
    std::fs::write(&manifest_path, &manifest_json)
        .map_err(|e| format!("Failed to write manifest: {e}"))?;

    info!(
        "Plugin '{}' v{} installed successfully",
        plugin_id, manifest.version
    );
    Ok(manifest)
}
