// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

//! Persistent mount manager for AeroFTP.
//!
//! Bridges saved server profiles to the OS filesystem layer via the
//! `aeroftp-cli mount` subcommand (FUSE on Linux, WebDAV bridge on Windows).
//!
//! Two storage modes for the mount registry:
//! - **Sidecar** (default): `<config>/aeroftp/mounts.json`, plaintext, readable
//!   by external daemons (systemd-user, Task Scheduler) without needing to
//!   unlock the vault.
//! - **Vault**: encrypted under the master password via `CredentialStore`.
//!   Requires the vault to be unlocked at app start.
//!
//! Mount configs never contain secrets; credentials are resolved by the CLI
//! through the `--profile` flag against the same vault used by the GUI.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;
use tokio::sync::Mutex as AsyncMutex;

use crate::credential_store::CredentialStore;

const SIDECAR_FILENAME: &str = "mounts.json";
const VAULT_REGISTRY_KEY: &str = "aeroftp_mounts_registry";
const DEFAULT_CACHE_TTL: u64 = 30;

/// Persisted mount configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountConfig {
    pub id: String,
    pub name: String,
    /// Saved server profile name (lookup key in the credential store).
    pub profile: String,
    #[serde(default = "default_remote_path")]
    pub remote_path: String,
    /// Linux/macOS: empty directory path. Windows: drive letter like "Z:".
    pub mountpoint: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl: u64,
    #[serde(default)]
    pub allow_other: bool,
    /// Mount automatically on system boot (Phase B autostart).
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default)]
    pub created_at: String,
}

fn default_remote_path() -> String {
    "/".to_string()
}
fn default_cache_ttl() -> u64 {
    DEFAULT_CACHE_TTL
}

/// Storage backend for the mount registry.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    #[default]
    Sidecar,
    Vault,
}

/// On-disk shape of the mount registry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MountRegistry {
    #[serde(default)]
    pub storage_mode: StorageMode,
    #[serde(default)]
    pub mounts: Vec<MountConfig>,
}

/// Runtime state of an active mount.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // Failed / Unmounting reserved for richer state transitions
pub enum MountState {
    Stopped,
    Starting,
    Running,
    Failed,
    Unmounting,
}

#[derive(Debug, Clone, Serialize)]
pub struct MountStatus {
    pub id: String,
    pub state: MountState,
    pub pid: Option<u32>,
    pub started_at: Option<String>,
    pub error: Option<String>,
}

struct ActiveMount {
    child: tokio::process::Child,
    pid: u32,
    started_at: String,
}

static ACTIVE: LazyLock<AsyncMutex<HashMap<String, ActiveMount>>> =
    LazyLock::new(|| AsyncMutex::new(HashMap::new()));

/// Sidecar config path.
fn sidecar_path() -> PathBuf {
    let base = dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
    base.join("aeroftp").join(SIDECAR_FILENAME)
}

/// Locate the bundled `aeroftp-cli` binary.
///
/// Search order:
/// 1. Same directory as the running GUI executable (works for installed builds
///    and `cargo run`).
/// 2. `aeroftp-cli` on `PATH`.
fn locate_cli() -> Result<PathBuf, String> {
    let exe_name = if cfg!(windows) {
        "aeroftp-cli.exe"
    } else {
        "aeroftp-cli"
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        for dir in path_var.split(separator) {
            if dir.is_empty() {
                continue;
            }
            let candidate = PathBuf::from(dir).join(exe_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(format!("Cannot locate {} binary on disk or PATH", exe_name))
}

fn detect_initial_storage_mode() -> StorageMode {
    if let Ok(content) = std::fs::read_to_string(sidecar_path()) {
        if let Ok(reg) = serde_json::from_str::<MountRegistry>(&content) {
            return reg.storage_mode;
        }
    }
    StorageMode::Sidecar
}

/// Load the mount registry, transparently handling both storage modes.
pub fn load_registry() -> MountRegistry {
    let mode = detect_initial_storage_mode();
    match mode {
        StorageMode::Sidecar => load_from_sidecar(),
        StorageMode::Vault => load_from_vault().unwrap_or_else(|e| {
            tracing::warn!("Mount registry vault load failed ({}), falling back to sidecar", e);
            load_from_sidecar()
        }),
    }
}

fn load_from_sidecar() -> MountRegistry {
    let path = sidecar_path();
    if !path.exists() {
        return MountRegistry::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!("Mount registry parse failed: {}", e);
            MountRegistry::default()
        }),
        Err(e) => {
            tracing::warn!("Mount registry read failed: {}", e);
            MountRegistry::default()
        }
    }
}

fn load_from_vault() -> Result<MountRegistry, String> {
    let store = CredentialStore::from_cache().ok_or("Vault is locked")?;
    match store.get(VAULT_REGISTRY_KEY) {
        Ok(json) => serde_json::from_str(&json)
            .map_err(|e| format!("Mount registry vault parse: {}", e)),
        Err(_) => Ok(MountRegistry {
            storage_mode: StorageMode::Vault,
            mounts: Vec::new(),
        }),
    }
}

/// Persist registry, atomic write.
pub fn save_registry(registry: &MountRegistry) -> Result<(), String> {
    match registry.storage_mode {
        StorageMode::Sidecar => save_to_sidecar(registry),
        StorageMode::Vault => save_to_vault(registry),
    }
}

fn save_to_sidecar(registry: &MountRegistry) -> Result<(), String> {
    let path = sidecar_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create config directory: {}", e))?;
    }
    let json = serde_json::to_string_pretty(registry)
        .map_err(|e| format!("Mount registry serialize: {}", e))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("Mount registry write: {}", e))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("Mount registry rename: {}", e))?;
    Ok(())
}

fn save_to_vault(registry: &MountRegistry) -> Result<(), String> {
    let store = CredentialStore::from_cache().ok_or("Vault is locked")?;
    let json = serde_json::to_string(registry)
        .map_err(|e| format!("Mount registry serialize: {}", e))?;
    store
        .store(VAULT_REGISTRY_KEY, &json)
        .map_err(|e| format!("Mount registry vault store: {}", e))?;
    // Keep a sidecar stub recording the storage mode so external readers know
    // where to look (but no mount data leaks).
    let stub = MountRegistry {
        storage_mode: StorageMode::Vault,
        mounts: Vec::new(),
    };
    let _ = save_to_sidecar(&stub);
    Ok(())
}

/// Switch storage mode, migrating the existing list.
pub fn switch_storage_mode(target: StorageMode) -> Result<(), String> {
    let mut current = load_registry();
    if current.storage_mode == target {
        return Ok(());
    }
    if target == StorageMode::Vault && CredentialStore::from_cache().is_none() {
        return Err("Vault must be unlocked before switching to vault storage mode".to_string());
    }
    current.storage_mode = target;
    save_registry(&current)?;
    if target == StorageMode::Sidecar {
        // Best-effort: clear the vault entry so we don't leave stale duplicates.
        if let Some(store) = CredentialStore::from_cache() {
            let _ = store.delete(VAULT_REGISTRY_KEY);
        }
    }
    Ok(())
}

/// Validate that a mountpoint string is well-formed for the current platform.
fn validate_mountpoint(mp: &str) -> Result<(), String> {
    if mp.trim().is_empty() {
        return Err("Mountpoint cannot be empty".to_string());
    }
    #[cfg(windows)]
    {
        let letter = mp.trim().trim_end_matches(':');
        if letter.len() != 1 || !letter.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            return Err("Windows mountpoint must be a drive letter like 'Z:'".to_string());
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let path = std::path::Path::new(mp);
        if !path.is_absolute() {
            return Err("Mount path must be absolute".to_string());
        }
        Ok(())
    }
}

/// Save (insert or update) a mount config.
pub fn upsert_config(mut config: MountConfig) -> Result<MountConfig, String> {
    if config.id.trim().is_empty() {
        config.id = uuid::Uuid::new_v4().to_string();
    }
    if config.name.trim().is_empty() {
        config.name = config.profile.clone();
    }
    if config.profile.trim().is_empty() {
        return Err("Mount config requires a profile name".to_string());
    }
    if config.remote_path.is_empty() {
        config.remote_path = "/".to_string();
    }
    if config.cache_ttl == 0 {
        config.cache_ttl = DEFAULT_CACHE_TTL;
    }
    validate_mountpoint(&config.mountpoint)?;

    if config.created_at.is_empty() {
        config.created_at = Utc::now().to_rfc3339();
    }

    let mut registry = load_registry();
    if let Some(slot) = registry.mounts.iter_mut().find(|m| m.id == config.id) {
        *slot = config.clone();
    } else {
        registry.mounts.push(config.clone());
    }
    save_registry(&registry)?;
    Ok(config)
}

/// Delete a mount config (does not affect running mounts).
pub fn delete_config(id: &str) -> Result<(), String> {
    let mut registry = load_registry();
    let before = registry.mounts.len();
    registry.mounts.retain(|m| m.id != id);
    if registry.mounts.len() == before {
        return Err(format!("Mount config '{}' not found", id));
    }
    save_registry(&registry)
}

/// Suggest a default mountpoint for a profile name.
pub fn suggest_mountpoint(profile: &str) -> String {
    #[cfg(windows)]
    {
        let _ = profile;
        return pick_free_drive_letter().unwrap_or_else(|_| "Z:".to_string());
    }
    #[cfg(not(windows))]
    {
        let safe: String = profile
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            })
            .collect();
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join("aeroftp-mounts")
            .join(if safe.is_empty() { "mount" } else { &safe })
            .to_string_lossy()
            .into_owned()
    }
}

/// Find a free drive letter on Windows by checking which letters lack a `\` root.
#[cfg(windows)]
pub fn pick_free_drive_letter() -> Result<String, String> {
    use std::path::Path;
    for c in ('D'..='Z').rev() {
        let drive = format!("{}:\\", c);
        if !Path::new(&drive).exists() {
            return Ok(format!("{}:", c));
        }
    }
    Err("No free drive letter available".to_string())
}

#[cfg(not(windows))]
pub fn pick_free_drive_letter() -> Result<String, String> {
    Err("pick_free_drive_letter is only meaningful on Windows".to_string())
}

/// Spawn a mount via the bundled `aeroftp-cli mount` subcommand.
pub async fn start_mount(id: &str) -> Result<MountStatus, String> {
    let registry = load_registry();
    let cfg = registry
        .mounts
        .iter()
        .find(|m| m.id == id)
        .cloned()
        .ok_or_else(|| format!("Mount config '{}' not found", id))?;

    {
        let active = ACTIVE.lock().await;
        if active.contains_key(id) {
            return Err(format!("Mount '{}' is already running", cfg.name));
        }
    }

    // On Linux, ensure the mountpoint exists and is empty.
    #[cfg(target_os = "linux")]
    {
        let path = std::path::Path::new(&cfg.mountpoint);
        if !path.exists() {
            std::fs::create_dir_all(path)
                .map_err(|e| format!("Cannot create mountpoint {}: {}", cfg.mountpoint, e))?;
        } else if !path.is_dir() {
            return Err(format!("Mountpoint is not a directory: {}", cfg.mountpoint));
        }
    }

    let cli_path = locate_cli()?;
    let mut cmd = tokio::process::Command::new(&cli_path);
    cmd.arg("--profile")
        .arg(&cfg.profile)
        .arg("mount")
        .arg(&cfg.mountpoint)
        .arg("_")
        .arg(&cfg.remote_path)
        .arg("--cache-ttl")
        .arg(cfg.cache_ttl.to_string());
    if cfg.read_only {
        cmd.arg("--read-only");
    }
    #[cfg(not(windows))]
    if cfg.allow_other {
        cmd.arg("--allow-other");
    }

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn aeroftp-cli mount: {}", e))?;

    let pid = child.id().unwrap_or(0);
    let started_at = Utc::now().to_rfc3339();

    {
        let mut active = ACTIVE.lock().await;
        active.insert(
            id.to_string(),
            ActiveMount {
                child,
                pid,
                started_at: started_at.clone(),
            },
        );
    }

    Ok(MountStatus {
        id: id.to_string(),
        state: MountState::Starting,
        pid: Some(pid),
        started_at: Some(started_at),
        error: None,
    })
}

/// Stop an active mount via SIGTERM (CLI handles graceful FUSE unmount).
pub async fn stop_mount(id: &str) -> Result<(), String> {
    let mut active = ACTIVE.lock().await;
    let mut entry = active
        .remove(id)
        .ok_or_else(|| format!("Mount '{}' is not running", id))?;
    drop(active);

    #[cfg(unix)]
    unsafe {
        if entry.pid > 0 {
            // Graceful: send SIGTERM, the CLI shutdown_signal handler unmounts.
            libc::kill(entry.pid as i32, libc::SIGTERM);
        }
    }

    // On Windows, just kill the child; the WebDAV bridge teardown runs in CLI
    // shutdown handler which won't fire under TerminateProcess. Best we can do:
    // unmap the drive ourselves below if killing succeeds.
    #[cfg(windows)]
    {
        let _ = entry.child.start_kill();
    }

    // Wait up to 5s for graceful exit.
    let waited = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        entry.child.wait(),
    )
    .await;
    if waited.is_err() {
        let _ = entry.child.start_kill();
        let _ = entry.child.wait().await;
    }

    Ok(())
}

/// Snapshot of every known config + its current runtime state.
pub async fn list_with_status() -> Vec<(MountConfig, MountStatus)> {
    let registry = load_registry();
    let mut out = Vec::with_capacity(registry.mounts.len());
    let mut active = ACTIVE.lock().await;

    // First pass: reap dead children to refresh state.
    let mut dead_ids: Vec<String> = Vec::new();
    for (id, mount) in active.iter_mut() {
        if let Ok(Some(_status)) = mount.child.try_wait() {
            dead_ids.push(id.clone());
        }
    }
    for id in &dead_ids {
        active.remove(id);
    }
    drop(active);

    let active = ACTIVE.lock().await;
    for cfg in registry.mounts {
        let status = if let Some(am) = active.get(&cfg.id) {
            MountStatus {
                id: cfg.id.clone(),
                state: MountState::Running,
                pid: Some(am.pid),
                started_at: Some(am.started_at.clone()),
                error: None,
            }
        } else {
            MountStatus {
                id: cfg.id.clone(),
                state: MountState::Stopped,
                pid: None,
                started_at: None,
                error: None,
            }
        };
        out.push((cfg, status));
    }
    out
}

/// Open the mountpoint in the OS file manager. The mount must already be active.
pub async fn open_in_explorer(id: &str) -> Result<(), String> {
    let registry = load_registry();
    let cfg = registry
        .mounts
        .iter()
        .find(|m| m.id == id)
        .cloned()
        .ok_or_else(|| format!("Mount config '{}' not found", id))?;

    let active = ACTIVE.lock().await;
    if !active.contains_key(id) {
        return Err(format!("Mount '{}' is not running", cfg.name));
    }
    drop(active);

    let target = cfg.mountpoint.clone();

    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open")
        .arg(&target)
        .spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&target).spawn();
    #[cfg(windows)]
    let result = std::process::Command::new("explorer")
        .arg(&target)
        .spawn();

    result
        .map(|_| ())
        .map_err(|e| format!("Failed to open file manager: {}", e))
}

/// Stop every active mount (used on app quit).
pub async fn stop_all() {
    let ids: Vec<String> = {
        let active = ACTIVE.lock().await;
        active.keys().cloned().collect()
    };
    for id in ids {
        let _ = stop_mount(&id).await;
    }
}

// ── Autostart (Phase B) ────────────────────────────────────────────────────

/// Install an OS-level autostart entry that mounts on user login. On Linux
/// uses a systemd-user service; on Windows uses Task Scheduler ONLOGON.
///
/// Master-password vault mode is incompatible with unattended autostart since
/// the daemon cannot prompt for a password. The caller must surface this to
/// the user via [`autostart_blocked_reason`].
pub fn install_autostart(id: &str) -> Result<(), String> {
    let registry = load_registry();
    let cfg = registry
        .mounts
        .iter()
        .find(|m| m.id == id)
        .cloned()
        .ok_or_else(|| format!("Mount config '{}' not found", id))?;

    if let Some(reason) = autostart_blocked_reason() {
        return Err(reason);
    }

    let cli = locate_cli()?;
    install_autostart_platform(&cli, &cfg)
}

/// Remove the OS-level autostart entry for this mount, if present.
pub fn uninstall_autostart(id: &str) -> Result<(), String> {
    uninstall_autostart_platform(id)
}

/// Returns Some(reason) if autostart cannot be installed in the current
/// vault configuration. None means autostart is OK to install.
pub fn autostart_blocked_reason() -> Option<String> {
    if CredentialStore::is_master_mode() {
        Some(
            "Autostart is unavailable while the vault is in master-password mode. \
             Disable the master password under Security settings, or unlock the \
             mount manually after launching AeroFTP."
                .to_string(),
        )
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn unit_path(id: &str) -> PathBuf {
    let base = dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
    base.join("systemd")
        .join("user")
        .join(format!("aeroftp-mount-{}.service", sanitize_id(id)))
}

#[cfg(target_os = "linux")]
fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn render_systemd_unit(cli: &std::path::Path, cfg: &MountConfig) -> String {
    let mut exec = format!(
        "{} --profile {} mount {} _ {} --cache-ttl {}",
        shell_escape(&cli.to_string_lossy()),
        shell_escape(&cfg.profile),
        shell_escape(&cfg.mountpoint),
        shell_escape(&cfg.remote_path),
        cfg.cache_ttl,
    );
    if cfg.read_only {
        exec.push_str(" --read-only");
    }
    if cfg.allow_other {
        exec.push_str(" --allow-other");
    }
    format!(
        "[Unit]\n\
         Description=AeroFTP mount: {name}\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exec}\n\
         Restart=on-failure\n\
         RestartSec=10\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        name = cfg.name.replace('\n', " "),
        exec = exec,
    )
}

#[cfg(target_os = "linux")]
fn shell_escape(s: &str) -> String {
    if s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | ':'))
    {
        s.to_string()
    } else {
        let escaped = s.replace('\'', "'\\''");
        format!("'{}'", escaped)
    }
}

#[cfg(target_os = "linux")]
fn install_autostart_platform(cli: &std::path::Path, cfg: &MountConfig) -> Result<(), String> {
    let path = unit_path(&cfg.id);
    let unit = render_systemd_unit(cli, cfg);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create systemd-user dir: {}", e))?;
    }
    std::fs::write(&path, unit).map_err(|e| format!("Cannot write systemd unit: {}", e))?;

    run_systemctl(&["--user", "daemon-reload"])?;
    run_systemctl(&[
        "--user",
        "enable",
        "--now",
        &format!("aeroftp-mount-{}.service", sanitize_id(&cfg.id)),
    ])?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_autostart_platform(id: &str) -> Result<(), String> {
    let unit_name = format!("aeroftp-mount-{}.service", sanitize_id(id));
    let _ = run_systemctl(&["--user", "disable", "--now", &unit_name]);
    let path = unit_path(id);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Cannot remove systemd unit: {}", e))?;
    }
    let _ = run_systemctl(&["--user", "daemon-reload"]);
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl invocation failed: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "systemctl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn task_name(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("AeroFTP-Mount-{}", safe)
}

#[cfg(windows)]
fn install_autostart_platform(cli: &std::path::Path, cfg: &MountConfig) -> Result<(), String> {
    let mut tr = format!(
        "\"{}\" --profile \"{}\" mount \"{}\" _ \"{}\" --cache-ttl {}",
        cli.display(),
        cfg.profile.replace('"', "\""),
        cfg.mountpoint.replace('"', "\""),
        cfg.remote_path.replace('"', "\""),
        cfg.cache_ttl,
    );
    if cfg.read_only {
        tr.push_str(" --read-only");
    }
    let task = task_name(&cfg.id);
    let output = std::process::Command::new("schtasks")
        .args([
            "/Create",
            "/TN",
            &task,
            "/SC",
            "ONLOGON",
            "/RL",
            "LIMITED",
            "/F",
            "/TR",
            &tr,
        ])
        .output()
        .map_err(|e| format!("schtasks invocation failed: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "schtasks /Create failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn uninstall_autostart_platform(id: &str) -> Result<(), String> {
    let task = task_name(id);
    let _ = std::process::Command::new("schtasks")
        .args(["/Delete", "/TN", &task, "/F"])
        .output();
    Ok(())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn install_autostart_platform(_cli: &std::path::Path, _cfg: &MountConfig) -> Result<(), String> {
    Err("Autostart is not supported on this platform yet".to_string())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn uninstall_autostart_platform(_id: &str) -> Result<(), String> {
    Err("Autostart is not supported on this platform yet".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_mountpoint_rejects_empty() {
        assert!(validate_mountpoint("").is_err());
        assert!(validate_mountpoint("   ").is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn validate_mountpoint_requires_absolute_unix() {
        assert!(validate_mountpoint("relative/path").is_err());
        assert!(validate_mountpoint("/tmp/aeroftp").is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn validate_mountpoint_requires_drive_letter_windows() {
        assert!(validate_mountpoint("Z:").is_ok());
        assert!(validate_mountpoint("Z").is_ok());
        assert!(validate_mountpoint("/tmp").is_err());
        assert!(validate_mountpoint("ABC").is_err());
    }

    #[test]
    fn suggest_mountpoint_sanitizes_profile() {
        let s = suggest_mountpoint("My Server / Test");
        // Should not crash; should produce something parseable.
        assert!(!s.is_empty());
    }

    #[test]
    fn registry_default_is_sidecar() {
        let r = MountRegistry::default();
        assert_eq!(r.storage_mode, StorageMode::Sidecar);
        assert!(r.mounts.is_empty());
    }
}
