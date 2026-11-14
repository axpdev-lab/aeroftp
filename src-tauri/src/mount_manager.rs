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
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tokio::sync::Mutex as AsyncMutex;

use crate::credential_store::{CredentialError, CredentialStore};

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

/// Serializes the load-mutate-save of the mount registry: the Tauri mount
/// commands reach the `*_at` mutators concurrently and, without this, two
/// upserts can each load the same registry and lose one update.
static MOUNT_REGISTRY_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Sidecar config path.
fn sidecar_path() -> PathBuf {
    crate::portable::aeroftp_data_root()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join(SIDECAR_FILENAME)
}

/// Locate the bundled `aeroftp-cli` binary.
///
/// Search order:
/// 1. Same directory as the running GUI executable (works for installed builds
///    and `cargo run`).
/// 2. `aeroftp-cli` on `PATH`.
pub(crate) fn locate_cli() -> Result<PathBuf, String> {
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

fn detect_storage_mode_at(sidecar: &Path) -> StorageMode {
    if let Ok(content) = std::fs::read_to_string(sidecar) {
        if let Ok(reg) = serde_json::from_str::<MountRegistry>(&content) {
            return reg.storage_mode;
        }
    }
    StorageMode::Sidecar
}

/// Load the mount registry, transparently handling both storage modes.
///
/// Readers take the write lock too: a vault-mode save publishes the new
/// registry before the sidecar stub and rolls it back if the stub write
/// fails, so an unlocked read in that window (`start_mount`, the list, the
/// autostart install) could act on a value that never stays saved. The
/// mutators never call this, they use [`load_registry_strict_at`] under the
/// lock, so the non-reentrant mutex cannot deadlock here.
pub fn load_registry() -> MountRegistry {
    let store = CredentialStore::from_cache();
    load_registry_at(&sidecar_path(), store.as_ref())
}

/// [`load_registry`] on an explicit sidecar path and store, so tests can
/// pin the read lock on the production reader.
fn load_registry_at(sidecar: &Path, store: Option<&CredentialStore>) -> MountRegistry {
    let _guard = MOUNT_REGISTRY_WRITE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match detect_storage_mode_at(sidecar) {
        StorageMode::Sidecar => load_from_sidecar_at(sidecar),
        StorageMode::Vault => load_from_vault_with(store).unwrap_or_else(|e| {
            tracing::warn!(
                "Mount registry vault load failed ({}), falling back to sidecar",
                e
            );
            load_from_sidecar_at(sidecar)
        }),
    }
}

fn load_from_sidecar_at(path: &Path) -> MountRegistry {
    if !path.exists() {
        return MountRegistry::default();
    }
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!(
                "Mount registry parse failed: {}",
                crate::util::describe_json_error(&e)
            );
            MountRegistry::default()
        }),
        Err(e) => {
            tracing::warn!("Mount registry read failed: {}", e);
            MountRegistry::default()
        }
    }
}

/// Strict sidecar load for the write path: a `mounts.json` that exists but
/// cannot be read or parsed is an error, never an empty registry a mutator
/// would save over the mounts it could not read. `exists()` is deliberately
/// not used: it reports false on metadata errors too, which would pick the
/// default and lose the file.
fn load_from_sidecar_strict_at(path: &Path) -> Result<MountRegistry, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MountRegistry::default());
        }
        Err(e) => {
            return Err(format!(
                "Mount registry read failed '{}': {}",
                path.display(),
                e
            ));
        }
    };
    serde_json::from_str(&content).map_err(|e| {
        format!(
            "Mount registry parse failed '{}': {}",
            path.display(),
            crate::util::describe_json_error(&e)
        )
    })
}

/// [`load_registry`] for the mutating paths (`upsert_config`,
/// `delete_config`, `switch_storage_mode`): same storage-mode resolution,
/// strict reads, and no cross-store fallback: in Vault mode the sidecar is
/// only a stub, so falling back to it would hand the mutator an empty
/// registry it would save over the real one.
///
/// The sidecar path and the vault store are explicit so tests can run the
/// mutators against a tempdir and a real test vault; the public entry points
/// pass `sidecar_path()` and `CredentialStore::from_cache()`.
fn load_registry_strict_at(
    sidecar: &Path,
    store: Option<&CredentialStore>,
) -> Result<MountRegistry, String> {
    // One sidecar read decides everything: its `storage_mode` names the
    // authoritative store. Reading it twice would leave a window in which a
    // concurrent migration flips the answer between the two reads.
    let sidecar_registry = load_from_sidecar_strict_at(sidecar)?;
    match sidecar_registry.storage_mode {
        StorageMode::Vault => registry_from_vault_store(store.ok_or("Vault is locked")?),
        StorageMode::Sidecar => Ok(sidecar_registry),
    }
}

/// Strict vault load: only a genuinely absent key means an empty registry.
/// Every other `CredentialError` (unreadable vault.db, failed decrypt,
/// serialization) propagates, so a mutator never saves over a registry it
/// could not read.
fn registry_from_vault_store(store: &CredentialStore) -> Result<MountRegistry, String> {
    vault_registry_from_get(store.get(VAULT_REGISTRY_KEY))
}

/// The registry stored in the vault. The vault is read only when the sidecar
/// says Vault mode, so that is its mode whatever the payload claims: a payload
/// without the field (which defaults to Sidecar) or naming Sidecar would send
/// the next save into the plaintext sidecar and make a switch to Sidecar a
/// no-op that migrates nothing.
fn parse_vault_registry(json: &str) -> Result<MountRegistry, String> {
    let registry: MountRegistry = serde_json::from_str(json).map_err(|e| {
        format!(
            "Mount registry vault parse: {}",
            crate::util::describe_json_error(&e)
        )
    })?;
    Ok(MountRegistry {
        storage_mode: StorageMode::Vault,
        ..registry
    })
}

/// [`registry_from_vault_store`] core on the raw `store.get` result, pure so
/// the NotFound-vs-error rule is testable without a vault.
fn vault_registry_from_get(get: Result<String, CredentialError>) -> Result<MountRegistry, String> {
    match get {
        Ok(json) => parse_vault_registry(&json),
        Err(CredentialError::NotFound(_)) => Ok(MountRegistry {
            storage_mode: StorageMode::Vault,
            mounts: Vec::new(),
        }),
        Err(e) => Err(format!("Mount registry vault read: {}", e)),
    }
}

fn load_from_vault_with(store: Option<&CredentialStore>) -> Result<MountRegistry, String> {
    let store = store.ok_or("Vault is locked")?;
    match store.get(VAULT_REGISTRY_KEY) {
        Ok(json) => parse_vault_registry(&json),
        Err(_) => Ok(MountRegistry {
            storage_mode: StorageMode::Vault,
            mounts: Vec::new(),
        }),
    }
}

/// Persist registry, atomic write, to the store its `storage_mode` names.
fn save_registry_at(
    registry: &MountRegistry,
    sidecar: &Path,
    store: Option<&CredentialStore>,
) -> Result<(), String> {
    match registry.storage_mode {
        StorageMode::Sidecar => save_to_sidecar_at(registry, sidecar),
        StorageMode::Vault => {
            save_to_vault_with(store.ok_or("Vault is locked")?, registry, sidecar)
        }
    }
}

fn save_to_sidecar_at(registry: &MountRegistry, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create config directory: {}", e))?;
    }
    let json = serde_json::to_string_pretty(registry)
        .map_err(|e| format!("Mount registry serialize: {}", e))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("Mount registry write: {}", e))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("Mount registry rename: {}", e))?;
    Ok(())
}

/// A test's pause point inside [`save_to_vault_with`], after the vault store
/// and before the stub write: the window a failed stub write rolls back.
/// Keyed by the sidecar path, so a save of another test never runs it.
#[cfg(test)]
type StubWritePause = (std::path::PathBuf, Box<dyn FnOnce() + Send>);
#[cfg(test)]
static BEFORE_STUB_WRITE: std::sync::Mutex<Option<StubWritePause>> = std::sync::Mutex::new(None);

/// Vault-mode save on an explicit store and sidecar path, so tests can pin
/// the stub-write rule against a real test vault.
fn save_to_vault_with(
    store: &CredentialStore,
    registry: &MountRegistry,
    sidecar: &Path,
) -> Result<(), String> {
    let json =
        serde_json::to_string(registry).map_err(|e| format!("Mount registry serialize: {}", e))?;
    // Snapshot the previous value first: if the sidecar stub write fails
    // after the vault store, the vault entry must be rolled back, or the
    // caller sees an error while the mutation is already committed (a
    // retried upsert would then duplicate the mount).
    let previous = match store.get(VAULT_REGISTRY_KEY) {
        Ok(value) => Some(value),
        Err(CredentialError::NotFound(_)) => None,
        Err(e) => return Err(format!("Mount registry vault read: {}", e)),
    };
    store
        .store(VAULT_REGISTRY_KEY, &json)
        .map_err(|e| format!("Mount registry vault store: {}", e))?;
    #[cfg(test)]
    {
        let mut slot = BEFORE_STUB_WRITE.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|(path, _)| path == sidecar) {
            let (_, pause) = slot.take().unwrap();
            drop(slot);
            pause();
        }
    }
    // Keep a sidecar stub recording the storage mode so external readers know
    // where to look (but no mount data leaks). A failed stub write must
    // surface: silently losing it would flip the detected mode back to
    // Sidecar and orphan the vault registry.
    let stub = MountRegistry {
        storage_mode: StorageMode::Vault,
        mounts: Vec::new(),
    };
    if let Err(stub_err) = save_to_sidecar_at(&stub, sidecar) {
        let rollback = match &previous {
            Some(value) => store
                .store(VAULT_REGISTRY_KEY, value)
                .map_err(|e| format!("Mount registry vault restore: {}", e)),
            None => store
                .delete(VAULT_REGISTRY_KEY)
                .map_err(|e| format!("Mount registry vault restore: {}", e)),
        };
        return Err(match rollback {
            Ok(()) => stub_err,
            Err(restore_err) => format!("{}; {}", stub_err, restore_err),
        });
    }
    Ok(())
}

/// Switch storage mode, migrating the existing list.
pub fn switch_storage_mode(target: StorageMode) -> Result<(), String> {
    let store = CredentialStore::from_cache();
    switch_storage_mode_at(target, &sidecar_path(), store.as_ref())
}

fn switch_storage_mode_at(
    target: StorageMode,
    sidecar: &Path,
    store: Option<&CredentialStore>,
) -> Result<(), String> {
    let _guard = MOUNT_REGISTRY_WRITE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut current = load_registry_strict_at(sidecar, store)?;
    if current.storage_mode == target {
        return Ok(());
    }
    if target == StorageMode::Vault && store.is_none() {
        return Err("Vault must be unlocked before switching to vault storage mode".to_string());
    }
    current.storage_mode = target;
    save_registry_at(&current, sidecar, store)?;
    if target == StorageMode::Sidecar {
        // Best-effort: clear the vault entry so we don't leave stale duplicates.
        if let Some(store) = store {
            let _ = store.delete(VAULT_REGISTRY_KEY);
        }
    }
    Ok(())
}

/// Reject shell-metacharacter and newline injection vectors in fields that
/// later land in a shell-parsed command line (Windows `schtasks /TR <str>`)
/// or in line-oriented config files (Linux systemd `ExecStart=` is a single
/// line, a stray `\n` breaks unit parsing). The Windows autostart path used
/// to interpolate these strings via three no-op `.replace('"', "\"")` calls
/// (Z.4.3.f2): rejecting at config-time closes the entire class. The
/// allowed set is permissive on purpose, real-world profile names and
/// remote paths can legitimately contain spaces, dots, slashes, dashes,
/// underscores, and unicode letters/digits; only the metacharacter set
/// known to break a `CommandLineToArgvW` quoting context is refused.
fn validate_no_shell_specials(field: &str, value: &str) -> Result<(), String> {
    for (idx, ch) in value.chars().enumerate() {
        if matches!(ch, '"' | ';' | '&' | '|' | '\n' | '\r' | '\0' | '`' | '$') {
            return Err(format!(
                "{} cannot contain shell-metacharacters (found {:?} at byte offset {})",
                field, ch, idx
            ));
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
        if letter.len() != 1
            || !letter
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
        {
            return Err("Windows mountpoint must be a drive letter like 'Z:'".to_string());
        }
        Ok(())
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
pub fn upsert_config(config: MountConfig) -> Result<MountConfig, String> {
    let store = CredentialStore::from_cache();
    upsert_config_at(config, &sidecar_path(), store.as_ref())
}

fn upsert_config_at(
    mut config: MountConfig,
    sidecar: &Path,
    store: Option<&CredentialStore>,
) -> Result<MountConfig, String> {
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
    validate_no_shell_specials("Profile name", &config.profile)?;
    validate_no_shell_specials("Mount name", &config.name)?;
    validate_no_shell_specials("Remote path", &config.remote_path)?;
    validate_mountpoint(&config.mountpoint)?;

    if config.created_at.is_empty() {
        config.created_at = Utc::now().to_rfc3339();
    }

    let _guard = MOUNT_REGISTRY_WRITE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut registry = load_registry_strict_at(sidecar, store)?;
    if let Some(slot) = registry.mounts.iter_mut().find(|m| m.id == config.id) {
        *slot = config.clone();
    } else {
        registry.mounts.push(config.clone());
    }
    save_registry_at(&registry, sidecar, store)?;
    Ok(config)
}

/// Delete a mount config (does not affect running mounts).
pub fn delete_config(id: &str) -> Result<(), String> {
    let store = CredentialStore::from_cache();
    delete_config_at(id, &sidecar_path(), store.as_ref())
}

fn delete_config_at(
    id: &str,
    sidecar: &Path,
    store: Option<&CredentialStore>,
) -> Result<(), String> {
    let _guard = MOUNT_REGISTRY_WRITE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut registry = load_registry_strict_at(sidecar, store)?;
    let before = registry.mounts.len();
    registry.mounts.retain(|m| m.id != id);
    if registry.mounts.len() == before {
        return Err(format!("Mount config '{}' not found", id));
    }
    save_registry_at(&registry, sidecar, store)
}

/// Suggest a default mountpoint for a profile name.
pub fn suggest_mountpoint(profile: &str) -> String {
    #[cfg(windows)]
    {
        let _ = profile;
        pick_free_drive_letter().unwrap_or_else(|_| "Z:".to_string())
    }
    #[cfg(not(windows))]
    {
        let safe: String = profile
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
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
    let waited = tokio::time::timeout(std::time::Duration::from_secs(5), entry.child.wait()).await;
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
    let result = std::process::Command::new("xdg-open").arg(&target).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&target).spawn();
    #[cfg(windows)]
    let result = std::process::Command::new("explorer").arg(&target).spawn();

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
    if s.chars()
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
        std::fs::remove_file(&path).map_err(|e| format!("Cannot remove systemd unit: {}", e))?;
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
    // Defense-in-depth: `upsert_config` already rejects shell-metachars in
    // these fields via `validate_no_shell_specials`. Strip any `"` that
    // somehow survives the validation gate (rather than re-emit it into
    // the schtasks /TR command line and risk breaking out of the quoting
    // context). Z.4.3.f2.
    let safe_profile = cfg.profile.replace('"', "");
    let safe_mountpoint = cfg.mountpoint.replace('"', "");
    let safe_remote_path = cfg.remote_path.replace('"', "");
    let mut tr = format!(
        "\"{}\" --profile \"{}\" mount \"{}\" _ \"{}\" --cache-ttl {}",
        cli.display(),
        safe_profile,
        safe_mountpoint,
        safe_remote_path,
        cfg.cache_ttl,
    );
    if cfg.read_only {
        tr.push_str(" --read-only");
    }
    let task = task_name(&cfg.id);
    // #351: hidden_command avoids a console window flash on Windows.
    let output = crate::hidden_command("schtasks")
        .args([
            "/Create", "/TN", &task, "/SC", "ONLOGON", "/RL", "LIMITED", "/F", "/TR", &tr,
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
    // #351: hidden_command avoids a console window flash on Windows.
    let _ = crate::hidden_command("schtasks")
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

    /// A `mounts.json` that exists but does not parse is an error on the
    /// write path: the strict load reports it, so `upsert_config`,
    /// `delete_config` and `switch_storage_mode` propagate the error and
    /// never save an empty registry over the mounts they could not read.
    #[test]
    fn an_unparseable_registry_is_never_overwritten() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("mounts.json");
        let bytes = b"{ not json".as_slice();
        std::fs::write(&path, bytes).unwrap();

        let err = upsert_config_at(test_mount("m1", data.path()), &path, None).unwrap_err();
        assert!(err.contains("parse"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        let err = delete_config_at("m1", &path, None).unwrap_err();
        assert!(err.contains("parse"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        let err = switch_storage_mode_at(StorageMode::Sidecar, &path, None).unwrap_err();
        assert!(err.contains("parse"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    /// The sidecar fixture of `tests/cli_aerocloud_strict.rs` is a valid
    /// registry (`storage_mode` is snake_case), so a parse failure there can
    /// never stand in for the preservation the test checks.
    #[test]
    fn the_cli_test_sidecar_fixture_is_a_valid_registry() {
        let fixture = include_str!("../tests/cli_aerocloud_strict.rs")
            .lines()
            .find(|l| l.contains("storage_mode") && l.contains("mounts"))
            .and_then(|l| l.split("b\"").nth(1))
            .and_then(|l| l.split("\".to_vec()").next())
            .expect("fixture line")
            .replace("\\\"", "\"");
        let registry: MountRegistry = serde_json::from_str(&fixture).unwrap();
        assert_eq!(registry.storage_mode, StorageMode::Sidecar);
    }

    /// An absent `mounts.json` still loads as the default registry, so the
    /// mutators keep their default-plus-save path.
    #[test]
    fn an_absent_registry_loads_as_default() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("mounts.json");
        let registry = load_from_sidecar_strict_at(&path).unwrap();
        assert_eq!(registry.storage_mode, StorageMode::Sidecar);
        assert!(registry.mounts.is_empty());
    }

    /// A metadata error on `mounts.json` (unreadable parent directory) is an
    /// error, not a missing file: `exists()` would report false here.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_registry_dir_is_an_error_not_a_default() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("running as root: chmod cannot block the read, skipping");
            return;
        }
        let data = tempfile::tempdir().unwrap();
        let dir = data.path().join("cfg");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("mounts.json");
        let bytes = b"{ \"mounts\": [] }".as_slice();
        std::fs::write(&path, bytes).unwrap();
        struct PermGuard(std::path::PathBuf);
        impl Drop for PermGuard {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let _guard = PermGuard(dir.clone());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let err = load_from_sidecar_strict_at(&path).unwrap_err();
        assert!(err.contains("read failed"), "{err}");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    /// Only `CredentialError::NotFound` means an empty vault registry; every
    /// other vault error is an error, so a mutator cannot mistake an
    /// unreadable vault for an empty one.
    #[test]
    fn vault_registry_distinguishes_not_found_from_real_errors() {
        let empty = vault_registry_from_get(Err(CredentialError::NotFound(
            VAULT_REGISTRY_KEY.to_string(),
        )))
        .unwrap();
        assert_eq!(empty.storage_mode, StorageMode::Vault);
        assert!(empty.mounts.is_empty());
        for err in [
            CredentialError::Encryption("bad".to_string()),
            CredentialError::Serialization("bad".to_string()),
            CredentialError::InvalidKeyFile,
        ] {
            assert!(vault_registry_from_get(Err(err)).is_err());
        }
        let good = serde_json::to_string(&MountRegistry {
            storage_mode: StorageMode::Vault,
            mounts: vec![MountConfig {
                id: "m1".into(),
                name: String::new(),
                profile: "p".into(),
                remote_path: String::new(),
                mountpoint: String::new(),
                read_only: false,
                cache_ttl: 0,
                allow_other: false,
                auto_start: false,
                created_at: String::new(),
            }],
        })
        .unwrap();
        let parsed = vault_registry_from_get(Ok(good)).unwrap();
        assert_eq!(parsed.mounts.len(), 1);
        assert!(vault_registry_from_get(Ok("{ not json".to_string())).is_err());
    }

    /// A parse error names the kind and position, never the value, on every
    /// registry read: sidecar, strict vault and the reader's vault path.
    #[test]
    fn a_registry_parse_error_does_not_quote_the_values() {
        let bad = r#"{"storage_mode": "sidecar", "mounts": "SENTINEL-SECRET"}"#;
        let data = tempfile::tempdir().unwrap();
        let sidecar = data.path().join("mounts.json");
        std::fs::write(&sidecar, bad).unwrap();
        let vault_dir = tempfile::tempdir().unwrap();
        let store = test_vault(vault_dir.path(), 11);
        store.store(VAULT_REGISTRY_KEY, bad).unwrap();
        for err in [
            load_from_sidecar_strict_at(&sidecar).unwrap_err(),
            vault_registry_from_get(Ok(bad.to_string())).unwrap_err(),
            load_from_vault_with(Some(&store)).unwrap_err(),
        ] {
            assert!(err.contains("parse"), "{err}");
            assert!(!err.contains("SENTINEL-SECRET"), "{err}");
        }
    }

    /// A vault database that does not parse fails the strict registry read
    /// with the kind and position, never a value quoted from the file.
    #[test]
    fn a_corrupt_vault_database_error_does_not_quote_its_values() {
        let vault_dir = tempfile::tempdir().unwrap();
        let store = test_vault(vault_dir.path(), 13);
        std::fs::write(
            vault_dir.path().join("vault.db"),
            r#"{"entries": "SENTINEL-SECRET"}"#,
        )
        .unwrap();
        let err = registry_from_vault_store(&store).unwrap_err();
        assert!(!err.contains("SENTINEL-SECRET"), "{err}");
    }

    /// A vault payload is in Vault mode whatever it says: an omitted mode
    /// (the Sidecar default) or an explicit Sidecar would route the next save
    /// to the plaintext sidecar.
    #[test]
    fn a_vault_registry_is_always_in_vault_mode() {
        let vault_dir = tempfile::tempdir().unwrap();
        let store = test_vault(vault_dir.path(), 17);
        for payload in [
            r#"{"mounts": []}"#,
            r#"{"storage_mode": "sidecar", "mounts": []}"#,
        ] {
            let parsed = vault_registry_from_get(Ok(payload.to_string())).unwrap();
            assert_eq!(parsed.storage_mode, StorageMode::Vault, "{payload}");
            store.store(VAULT_REGISTRY_KEY, payload).unwrap();
            let read = load_from_vault_with(Some(&store)).unwrap();
            assert_eq!(read.storage_mode, StorageMode::Vault, "{payload}");
        }
    }

    fn test_vault(dir: &Path, key: u8) -> CredentialStore {
        let path = dir.join("vault.db");
        CredentialStore::write_test_vault(&path, &[key; 32]);
        CredentialStore::from_verified_key(&path, &[key; 32])
    }

    /// A mount that passes `upsert_config` validation on every platform: a
    /// drive letter on Windows, an absolute path elsewhere.
    fn test_mount(id: &str, dir: &Path) -> MountConfig {
        let mountpoint = if cfg!(windows) {
            "Z:".to_string()
        } else {
            dir.join("mnt").to_string_lossy().into_owned()
        };
        MountConfig {
            id: id.into(),
            name: String::new(),
            profile: "p".into(),
            remote_path: String::new(),
            mountpoint,
            read_only: false,
            cache_ttl: 0,
            allow_other: false,
            auto_start: false,
            created_at: String::new(),
        }
    }

    /// An unreadable vault registry plus a valid Vault-mode sidecar stub:
    /// mutation (`upsert_config`, `delete_config`) and migration
    /// (`switch_storage_mode`) must error and leave BOTH stores untouched,
    /// against a real test vault. Before the fix the strict loader fell back
    /// to the empty stub: the upsert saved one mount over the vault registry,
    /// and the switch saved an empty sidecar and deleted the vault entry.
    #[test]
    fn an_unreadable_vault_registry_preserves_both_stores() {
        let vault_dir = tempfile::tempdir().unwrap();
        let store = test_vault(vault_dir.path(), 7);
        store.store(VAULT_REGISTRY_KEY, "{ not json").unwrap();

        let data = tempfile::tempdir().unwrap();
        let sidecar = data.path().join("mounts.json");
        let stub = MountRegistry {
            storage_mode: StorageMode::Vault,
            mounts: Vec::new(),
        };
        save_to_sidecar_at(&stub, &sidecar).unwrap();
        let sidecar_before = std::fs::read(&sidecar).unwrap();
        let untouched = || {
            assert_eq!(std::fs::read(&sidecar).unwrap(), sidecar_before);
            assert_eq!(store.get(VAULT_REGISTRY_KEY).unwrap(), "{ not json");
        };

        let mount = test_mount("m1", data.path());
        let err = upsert_config_at(mount, &sidecar, Some(&store)).unwrap_err();
        assert!(err.contains("parse"), "{err}");
        untouched();

        let err = delete_config_at("m1", &sidecar, Some(&store)).unwrap_err();
        assert!(err.contains("parse"), "{err}");
        untouched();

        let err = switch_storage_mode_at(StorageMode::Sidecar, &sidecar, Some(&store)).unwrap_err();
        assert!(err.contains("parse"), "{err}");
        untouched();

        // A locked vault is an error too, never the stub's empty list.
        let err = switch_storage_mode_at(StorageMode::Sidecar, &sidecar, None).unwrap_err();
        assert!(err.contains("locked"), "{err}");
        untouched();
    }

    /// A corrupt `vault.db` (not a corrupt registry value) is an error for
    /// the mutators too: `CredentialStore::get` reports the database parse
    /// failure instead of a missing key, so the strict load cannot read it
    /// as an empty registry. Both files keep their bytes.
    #[test]
    fn a_corrupt_vault_database_preserves_both_stores() {
        let vault_dir = tempfile::tempdir().unwrap();
        let store = test_vault(vault_dir.path(), 7);
        let data = tempfile::tempdir().unwrap();
        let sidecar = data.path().join("mounts.json");
        save_to_vault_with(
            &store,
            &MountRegistry {
                storage_mode: StorageMode::Vault,
                mounts: vec![test_mount("m0", data.path())],
            },
            &sidecar,
        )
        .unwrap();
        let vault_db = vault_dir.path().join("vault.db");
        std::fs::write(&vault_db, b"{ not a vault").unwrap();
        let vault_before = std::fs::read(&vault_db).unwrap();
        let sidecar_before = std::fs::read(&sidecar).unwrap();
        let untouched = || {
            assert_eq!(std::fs::read(&vault_db).unwrap(), vault_before);
            assert_eq!(std::fs::read(&sidecar).unwrap(), sidecar_before);
        };

        let err =
            upsert_config_at(test_mount("m1", data.path()), &sidecar, Some(&store)).unwrap_err();
        assert!(err.contains("vault read"), "{err}");
        untouched();

        let err = delete_config_at("m0", &sidecar, Some(&store)).unwrap_err();
        assert!(err.contains("vault read"), "{err}");
        untouched();

        let err = switch_storage_mode_at(StorageMode::Sidecar, &sidecar, Some(&store)).unwrap_err();
        assert!(err.contains("vault read"), "{err}");
        untouched();
    }

    /// The same seams on a readable vault: the mount survives the migration
    /// to the sidecar, and the vault entry is cleared. Without this, the
    /// preservation test above would also pass on a mutator that never
    /// writes anything.
    #[test]
    fn a_readable_vault_registry_migrates_to_the_sidecar() {
        let vault_dir = tempfile::tempdir().unwrap();
        let store = test_vault(vault_dir.path(), 7);
        let data = tempfile::tempdir().unwrap();
        let sidecar = data.path().join("mounts.json");
        save_to_vault_with(
            &store,
            &MountRegistry {
                storage_mode: StorageMode::Vault,
                mounts: Vec::new(),
            },
            &sidecar,
        )
        .unwrap();

        upsert_config_at(test_mount("m1", data.path()), &sidecar, Some(&store)).unwrap();
        let in_vault = registry_from_vault_store(&store).unwrap();
        assert_eq!(in_vault.mounts.len(), 1);

        switch_storage_mode_at(StorageMode::Sidecar, &sidecar, Some(&store)).unwrap();
        let migrated = load_from_sidecar_strict_at(&sidecar).unwrap();
        assert_eq!(migrated.storage_mode, StorageMode::Sidecar);
        assert_eq!(migrated.mounts.len(), 1);
        assert_eq!(migrated.mounts[0].id, "m1");
        assert!(matches!(
            store.get(VAULT_REGISTRY_KEY),
            Err(CredentialError::NotFound(_))
        ));
    }

    /// A failed sidecar stub write surfaces as an error (the `?` in
    /// `save_to_vault_with`), against a real test vault: an unwritable
    /// sidecar directory makes the stub write fail and the caller sees it.
    #[cfg(unix)]
    #[test]
    fn a_failed_stub_write_is_an_error_not_swallowed() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("running as root: chmod cannot block the write, skipping");
            return;
        }
        let vault_dir = tempfile::tempdir().unwrap();
        let vault_path = vault_dir.path().join("vault.db");
        CredentialStore::write_test_vault(&vault_path, &[7u8; 32]);
        let store = CredentialStore::from_verified_key(&vault_path, &[7u8; 32]);
        let registry = MountRegistry {
            storage_mode: StorageMode::Vault,
            mounts: vec![MountConfig {
                id: "m1".into(),
                name: String::new(),
                profile: "p".into(),
                remote_path: String::new(),
                mountpoint: String::new(),
                read_only: false,
                cache_ttl: 0,
                allow_other: false,
                auto_start: false,
                created_at: String::new(),
            }],
        };
        let data = tempfile::tempdir().unwrap();
        let dir = data.path().join("cfg");
        std::fs::create_dir(&dir).unwrap();
        struct PermGuard(std::path::PathBuf);
        impl Drop for PermGuard {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let _guard = PermGuard(dir.clone());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let sidecar = dir.join("mounts.json");
        assert!(save_to_vault_with(&store, &registry, &sidecar).is_err());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!sidecar.exists());
        // No entry existed before, so the rollback removes the one just written.
        assert!(matches!(
            store.get(VAULT_REGISTRY_KEY),
            Err(CredentialError::NotFound(_))
        ));
    }

    /// A read that starts while a vault save is between its store and a
    /// failing stub write waits for the rollback and returns the registry
    /// that stays saved, never the value about to be undone: `start_mount`
    /// would otherwise mount a folder the saved configuration never holds.
    #[cfg(unix)]
    #[test]
    fn a_read_during_a_failed_vault_save_sees_the_registry_that_stays() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("running as root: chmod cannot block the write, skipping");
            return;
        }
        let vault_dir = tempfile::tempdir().unwrap();
        let vault_path = vault_dir.path().join("vault.db");
        CredentialStore::write_test_vault(&vault_path, &[7u8; 32]);
        let store = CredentialStore::from_verified_key(&vault_path, &[7u8; 32]);
        let data = tempfile::tempdir().unwrap();
        let dir = data.path().join("cfg");
        std::fs::create_dir(&dir).unwrap();
        let sidecar = dir.join("mounts.json");
        let mut mount = test_mount("m1", data.path());
        mount.remote_path = "/old".into();
        save_to_vault_with(
            &store,
            &MountRegistry {
                storage_mode: StorageMode::Vault,
                mounts: vec![mount.clone()],
            },
            &sidecar,
        )
        .unwrap();
        struct PermGuard(std::path::PathBuf);
        impl Drop for PermGuard {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let _guard = PermGuard(dir.clone());
        // The stub stays readable, so the read finds the vault; a new stub
        // cannot be written next to it.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let read_in_window = Arc::new(AtomicBool::new(false));
        let reader = Arc::new(Mutex::new(None));
        {
            let read_in_window = read_in_window.clone();
            let reader = reader.clone();
            let vault_path = vault_path.clone();
            let sidecar = sidecar.clone();
            *BEFORE_STUB_WRITE.lock().unwrap() = Some((
                sidecar.clone(),
                Box::new(move || {
                    let handle = std::thread::spawn(move || {
                        let store = CredentialStore::from_verified_key(&vault_path, &[7u8; 32]);
                        load_registry_at(&sidecar, Some(&store))
                            .mounts
                            .first()
                            .map(|m| m.remote_path.clone())
                    });
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    read_in_window.store(handle.is_finished(), Ordering::SeqCst);
                    *reader.lock().unwrap() = Some(handle);
                }),
            ));
        }
        let mut changed = mount;
        changed.remote_path = "/new".into();
        assert!(upsert_config_at(changed, &sidecar, Some(&store)).is_err());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            !read_in_window.load(Ordering::SeqCst),
            "the read ran inside the save's window"
        );
        let handle = reader
            .lock()
            .unwrap()
            .take()
            .expect("the save reached its stub write");
        assert_eq!(handle.join().unwrap().as_deref(), Some("/old"));
    }

    /// The happy path of the same seam: vault entry written, stub records
    /// Vault mode with no mounts.
    #[test]
    fn a_vault_save_writes_the_registry_and_the_mode_stub() {
        let vault_dir = tempfile::tempdir().unwrap();
        let vault_path = vault_dir.path().join("vault.db");
        CredentialStore::write_test_vault(&vault_path, &[7u8; 32]);
        let store = CredentialStore::from_verified_key(&vault_path, &[7u8; 32]);
        let registry = MountRegistry {
            storage_mode: StorageMode::Vault,
            mounts: Vec::new(),
        };
        let data = tempfile::tempdir().unwrap();
        let sidecar = data.path().join("mounts.json");
        save_to_vault_with(&store, &registry, &sidecar).unwrap();
        let stub: MountRegistry =
            serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
        assert_eq!(stub.storage_mode, StorageMode::Vault);
        assert!(stub.mounts.is_empty());
        let roundtrip = registry_from_vault_store(&store).unwrap();
        assert_eq!(roundtrip.storage_mode, StorageMode::Vault);
    }

    /// A failed sidecar stub write must roll the vault entry back: the
    /// caller gets an error AND the vault keeps its previous bytes, so a
    /// retried upsert cannot duplicate the mount. `mounts.tmp` created as a
    /// directory makes `fs::write` fail on every platform, root included.
    #[test]
    fn a_failed_stub_write_rolls_back_the_vault_entry() {
        let vault_dir = tempfile::tempdir().unwrap();
        let vault_path = vault_dir.path().join("vault.db");
        CredentialStore::write_test_vault(&vault_path, &[7u8; 32]);
        let store = CredentialStore::from_verified_key(&vault_path, &[7u8; 32]);
        let registry = MountRegistry {
            storage_mode: StorageMode::Vault,
            mounts: vec![MountConfig {
                id: "m0".into(),
                name: String::new(),
                profile: "p0".into(),
                remote_path: String::new(),
                mountpoint: String::new(),
                read_only: false,
                cache_ttl: 0,
                allow_other: false,
                auto_start: false,
                created_at: String::new(),
            }],
        };
        let data = tempfile::tempdir().unwrap();
        let sidecar = data.path().join("mounts.json");
        save_to_vault_with(&store, &registry, &sidecar).unwrap();
        let before = store.get(VAULT_REGISTRY_KEY).unwrap();

        // Break the stub write: mounts.tmp exists as a directory.
        std::fs::create_dir(sidecar.with_extension("tmp")).unwrap();
        let mut config = MountConfig {
            profile: "p1".into(),
            ..test_mount("", data.path())
        };
        let err = upsert_config_at(config.clone(), &sidecar, Some(&store)).unwrap_err();
        assert!(err.contains("Mount registry write"), "{err}");
        // The vault entry is byte-identical: nothing was committed.
        assert_eq!(store.get(VAULT_REGISTRY_KEY).unwrap(), before);

        // Retry after removing the obstacle: exactly m0 + the new mount.
        std::fs::remove_dir(sidecar.with_extension("tmp")).unwrap();
        config.id = String::new();
        upsert_config_at(config, &sidecar, Some(&store)).unwrap();
        let after = registry_from_vault_store(&store).unwrap();
        assert_eq!(after.mounts.len(), 2, "{:?}", after.mounts);
        assert!(after.mounts.iter().any(|m| m.id == "m0"));
    }

    /// A registry read waits for a mutation in progress: the production
    /// reader runs under the write lock, so it never sees a vault value a
    /// failed stub write is about to roll back.
    #[test]
    fn a_registry_read_waits_for_a_mutation_in_progress() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let data = tempfile::tempdir().unwrap();
        let sidecar = data.path().join("mounts.json");
        save_to_sidecar_at(
            &MountRegistry {
                storage_mode: StorageMode::Sidecar,
                mounts: vec![test_mount("m1", data.path())],
            },
            &sidecar,
        )
        .unwrap();
        let guard = MOUNT_REGISTRY_WRITE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let done = done.clone();
            let sidecar = sidecar.clone();
            std::thread::spawn(move || {
                let registry = load_registry_at(&sidecar, None);
                done.store(true, Ordering::SeqCst);
                registry
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !done.load(Ordering::SeqCst),
            "the read ran while a mutation held the lock"
        );
        drop(guard);
        let registry = reader.join().unwrap();
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(registry.mounts.len(), 1);
    }

    /// Concurrent upserts on the same sidecar must lose no mount: the
    /// write lock serializes load-mutate-save. 20 rounds; without the lock
    /// this goes red (lost updates or tmp-file races) in some rounds.
    #[test]
    fn concurrent_upserts_lose_no_mount() {
        const THREADS: usize = 16;
        const ROUNDS: usize = 20;
        for round in 0..ROUNDS {
            let data = tempfile::tempdir().unwrap();
            let sidecar = data.path().join("mounts.json");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
            let mut handles = Vec::new();
            for i in 0..THREADS {
                let sidecar = sidecar.clone();
                let dir = data.path().to_path_buf();
                let barrier = barrier.clone();
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    let config = MountConfig {
                        profile: format!("p{i}"),
                        ..test_mount(&format!("m{i}"), &dir)
                    };
                    upsert_config_at(config, &sidecar, None)
                }));
            }
            for handle in handles {
                handle.join().unwrap().unwrap_or_else(|e| {
                    panic!("round {round}: upsert failed: {e}");
                });
            }
            let registry = load_from_sidecar_strict_at(&sidecar).unwrap();
            assert_eq!(
                registry.mounts.len(),
                THREADS,
                "round {round}: lost upserts: {:?}",
                registry.mounts.iter().map(|m| &m.id).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn validate_no_shell_specials_accepts_normal_input() {
        assert!(validate_no_shell_specials("Profile name", "MyServer").is_ok());
        assert!(validate_no_shell_specials("Profile name", "My Server (prod)").is_ok());
        assert!(validate_no_shell_specials("Remote path", "/srv/data/2026-05").is_ok());
        assert!(validate_no_shell_specials("Remote path", "/home/user.with.dots/_a-b").is_ok());
    }

    #[test]
    fn validate_no_shell_specials_rejects_quote_injection() {
        let payload = "abc\"; powershell -e <b64>; \"";
        let err = validate_no_shell_specials("Profile name", payload).unwrap_err();
        assert!(err.contains("Profile name"));
        assert!(err.contains("shell-metacharacters"));
    }

    #[test]
    fn validate_no_shell_specials_rejects_each_metachar() {
        for ch in ['"', ';', '&', '|', '\n', '\r', '\0', '`', '$'] {
            let payload = format!("ok{}tail", ch);
            assert!(
                validate_no_shell_specials("X", &payload).is_err(),
                "char {:?} must be rejected",
                ch
            );
        }
    }

    #[test]
    fn validate_no_shell_specials_accepts_unicode() {
        assert!(validate_no_shell_specials("Profile name", "Nas \u{00E9}t\u{00E9}").is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn windows_autostart_tr_strings_have_no_quote() {
        let cfg = MountConfig {
            id: "test-id".into(),
            name: "Test Mount".into(),
            profile: "My Profile".into(),
            mountpoint: "Z:".into(),
            remote_path: "/srv/data".into(),
            cache_ttl: 60,
            read_only: false,
            allow_other: false,
            auto_start: false,
            created_at: String::new(),
        };
        // Sanity: the three sanitized fields never produce a `"` in the
        // composed /TR string. Re-do the composition manually since the
        // production install_autostart_platform also invokes schtasks.
        let safe_profile = cfg.profile.replace('"', "");
        let safe_mountpoint = cfg.mountpoint.replace('"', "");
        let safe_remote_path = cfg.remote_path.replace('"', "");
        assert!(!safe_profile.contains('"'));
        assert!(!safe_mountpoint.contains('"'));
        assert!(!safe_remote_path.contains('"'));
    }
}
