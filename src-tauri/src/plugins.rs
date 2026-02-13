//! AeroAgent Plugin System
//!
//! Loads custom tool plugins from the app config directory.
//! Each plugin is a directory containing a plugin.json manifest
//! and executable scripts that receive JSON args on stdin
//! and return JSON results on stdout.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use tauri::Manager;
use tokio::io::AsyncWriteExt;
use tokio::time::{timeout, Duration};
use tracing::info;

const PLUGIN_TIMEOUT_SECS: u64 = 30;
const MAX_OUTPUT_BYTES: usize = 1_048_576; // 1 MB

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginToolParam {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String,
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Vec<PluginToolParam>,
    #[serde(rename = "dangerLevel", default = "default_danger")]
    pub danger_level: String,
    pub command: String,
}

fn default_danger() -> String {
    "medium".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub tools: Vec<PluginToolDef>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Get the plugins directory path
fn plugins_dir(app: &tauri::AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .expect("app config dir")
        .join("plugins")
}

/// List all installed plugins by scanning the plugins directory
#[tauri::command]
pub async fn list_plugins(app: tauri::AppHandle) -> Result<Vec<PluginManifest>, String> {
    let dir = plugins_dir(&app);
    if !dir.exists() {
        return Ok(vec![]);
    }

    let mut plugins = Vec::new();
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| format!("Failed to read plugins directory: {}", e))?;

    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let manifest_path = entry.path().join("plugin.json");
        if !manifest_path.exists() {
            continue;
        }
        match std::fs::read_to_string(&manifest_path) {
            Ok(content) => match serde_json::from_str::<PluginManifest>(&content) {
                Ok(mut manifest) => {
                    // Validate: id must be alphanumeric + underscore
                    if manifest
                        .id
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_')
                    {
                        // GPT-F02: Enforce minimum dangerLevel of "medium" for all plugin tools.
                        // Plugin authors must not bypass the approval gate by declaring "safe".
                        for tool in &mut manifest.tools {
                            if tool.danger_level == "safe" {
                                tool.danger_level = "medium".to_string();
                            }
                        }
                        plugins.push(manifest);
                    }
                }
                Err(e) => {
                    info!(
                        "Skipping invalid plugin manifest {:?}: {}",
                        manifest_path, e
                    );
                }
            },
            Err(e) => {
                info!("Failed to read plugin manifest {:?}: {}", manifest_path, e);
            }
        }
    }

    Ok(plugins)
}

/// Execute a plugin tool by spawning a subprocess
#[tauri::command]
pub async fn execute_plugin_tool(
    app: tauri::AppHandle,
    plugin_id: String,
    tool_name: String,
    args_json: String,
) -> Result<Value, String> {
    // Validate plugin_id (alphanumeric + underscore only)
    if !plugin_id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_')
    {
        return Err("Invalid plugin ID".to_string());
    }

    let plugin_dir = plugins_dir(&app).join(&plugin_id);
    let manifest_path = plugin_dir.join("plugin.json");

    if !manifest_path.exists() {
        return Err(format!("Plugin '{}' not found", plugin_id));
    }

    let manifest_content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("Failed to read plugin manifest: {}", e))?;
    let manifest: PluginManifest = serde_json::from_str(&manifest_content)
        .map_err(|e| format!("Invalid plugin manifest: {}", e))?;

    if !manifest.enabled {
        return Err(format!("Plugin '{}' is disabled", plugin_id));
    }

    let tool = manifest
        .tools
        .iter()
        .find(|t| t.name == tool_name)
        .ok_or_else(|| format!("Tool '{}' not found in plugin '{}'", tool_name, plugin_id))?;

    // SEC: Direct argv execution — no shell interpretation.
    // Reject shell metacharacters to prevent injection via crafted manifests.
    let command = &tool.command;
    const SHELL_METACHARACTERS: &[char] = &[
        '|', '&', ';', '`', '$', '(', ')', '>', '<', '{', '}', '\n', '\r', '!', '#',
    ];
    if command.chars().any(|c| SHELL_METACHARACTERS.contains(&c)) {
        return Err(format!(
            "Plugin command contains forbidden shell metacharacters: {}",
            tool_name
        ));
    }

    let argv: Vec<&str> = command.split_whitespace().collect();
    if argv.is_empty() {
        return Err(format!("Plugin '{}' has empty command", tool_name));
    }
    let program = argv[0];

    // Block path traversal (.. components) and absolute paths in the executable
    if program.contains("..") || program.starts_with('/') {
        return Err(format!(
            "Plugin command must be a relative path without traversal: {}",
            program
        ));
    }

    let mut child = tokio::process::Command::new(program)
        .args(&argv[1..])
        .current_dir(&plugin_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true) // Ensure process is killed on timeout or drop
        .spawn()
        .map_err(|e| format!("Failed to spawn plugin process: {}", e))?;

    // Write args to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(args_json.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to plugin stdin: {}", e))?;
        drop(stdin); // Close stdin
    }

    // Wait with timeout
    let output = timeout(
        Duration::from_secs(PLUGIN_TIMEOUT_SECS),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| {
        format!(
            "Plugin '{}' timed out after {}s",
            tool_name, PLUGIN_TIMEOUT_SECS
        )
    })?
    .map_err(|e| format!("Plugin process error: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Plugin '{}' failed (exit {}): {}",
            tool_name,
            output.status.code().unwrap_or(-1),
            &stderr[..stderr.len().min(500)]
        ));
    }

    let stdout = output.stdout;
    if stdout.len() > MAX_OUTPUT_BYTES {
        return Err(format!(
            "Plugin output exceeds {} bytes limit",
            MAX_OUTPUT_BYTES
        ));
    }

    let stdout_str =
        String::from_utf8(stdout).map_err(|_| "Plugin output is not valid UTF-8".to_string())?;

    // Try to parse as JSON, fall back to wrapping in a text result
    match serde_json::from_str::<Value>(&stdout_str) {
        Ok(val) => Ok(val),
        Err(_) => Ok(json!({ "result": stdout_str.trim() })),
    }
}

/// Install a plugin from a manifest JSON string
#[tauri::command]
pub async fn install_plugin(
    app: tauri::AppHandle,
    manifest_json: String,
) -> Result<String, String> {
    let manifest: PluginManifest =
        serde_json::from_str(&manifest_json).map_err(|e| format!("Invalid manifest: {}", e))?;

    // Validate id
    if !manifest
        .id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_')
        || manifest.id.is_empty()
    {
        return Err("Plugin ID must be non-empty alphanumeric with underscores".to_string());
    }

    let dir = plugins_dir(&app).join(&manifest.id);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create plugin directory: {}", e))?;

    let manifest_path = dir.join("plugin.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .map_err(|e| format!("Failed to write manifest: {}", e))?;

    info!("Installed plugin: {} v{}", manifest.id, manifest.version);
    Ok(manifest.id)
}

/// Remove a plugin by ID
#[tauri::command]
pub async fn remove_plugin(app: tauri::AppHandle, plugin_id: String) -> Result<(), String> {
    if !plugin_id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_')
    {
        return Err("Invalid plugin ID".to_string());
    }

    let dir = plugins_dir(&app).join(&plugin_id);
    if !dir.exists() {
        return Err(format!("Plugin '{}' not found", plugin_id));
    }

    std::fs::remove_dir_all(&dir).map_err(|e| format!("Failed to remove plugin: {}", e))?;

    info!("Removed plugin: {}", plugin_id);
    Ok(())
}
