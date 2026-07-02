//! Ephemeral, READ-ONLY mounts of UNLOCKED vaults (AeroMount Deliverable B,
//! #322 Ehud idea #1).
//!
//! The FUSE engine lives in the `aeroftp-cli` binary, so the GUI mounts an
//! unlocked vault by spawning `aeroftp-cli mount-vault` exactly like the remote
//! AeroMount spawns `aeroftp-cli mount`. The decisive difference: a vault mount
//! cannot outlive the in-memory keys, so it is EPHEMERAL and session-bound:
//!
//! - The vault password is handed to the child over its STDIN pipe (never argv,
//!   never persisted); the child unlocks the vault in its own memory and the
//!   provider is READ-ONLY.
//! - Mounts auto-unmount on vault lock / app quit: the GUI calls [`stop`] from
//!   `cryptomator_lock`, from the `.aerovault` browse-close path, and [`stop_all`]
//!   on app exit. `kill_on_drop(true)` is a backstop if the GUI dies.
//! - The mountpoint is a private `0700` directory under the runtime/cache dir, no
//!   `allow_other`; it is removed on unmount.
//!
//! Linux only for now (the FUSE engine is `#[cfg(target_os = "linux")]`); the
//! Windows WebDAV-bridge path for vaults is deferred (it would expose plaintext
//! over a loopback WebDAV server).

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use serde::Serialize;
use tokio::sync::Mutex as AsyncMutex;

#[cfg(target_os = "linux")]
use crate::mount_manager::locate_cli;

struct ActiveVaultMount {
    child: tokio::process::Child,
    mountpoint: PathBuf,
    kind: String,
    display_name: String,
    pid: u32,
}

static VAULT_MOUNTS: LazyLock<AsyncMutex<HashMap<String, ActiveVaultMount>>> =
    LazyLock::new(|| AsyncMutex::new(HashMap::new()));

/// A live vault mount, surfaced to the GUI.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VaultMountInfo {
    /// Stable key (Cryptomator `vault_id` or the `.aerovault` path).
    pub key: String,
    pub mountpoint: String,
    pub kind: String,
    pub display_name: String,
    pub pid: u32,
}

/// Reduce an arbitrary key to a safe single path segment for the mountpoint dir.
/// Only the Linux FUSE mount path uses this; kept under `test` so its unit test
/// builds on every platform.
#[cfg(any(target_os = "linux", test))]
fn sanitize(key: &str) -> String {
    let s: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let trimmed = s.trim_matches('_');
    let base = if trimmed.is_empty() { "vault" } else { trimmed };
    // Bound the length so a long path-key cannot blow past NAME_MAX.
    base.chars().take(48).collect()
}

/// Base directory for ephemeral vault mountpoints (`0700`). Prefers the per-user
/// runtime dir (tmpfs, auto-cleaned at logout); falls back to `~/.cache`.
#[cfg(target_os = "linux")]
fn mount_base_dir() -> Result<PathBuf, String> {
    let base = if let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") {
        if !rt.is_empty() {
            PathBuf::from(rt).join("aeroftp-aeromount")
        } else {
            cache_fallback()?
        }
    } else {
        cache_fallback()?
    };
    std::fs::create_dir_all(&base).map_err(|e| format!("create mount base: {e}"))?;
    set_private(&base);
    Ok(base)
}

#[cfg(target_os = "linux")]
fn cache_fallback() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    Ok(PathBuf::from(home).join(".cache/aeroftp/aeromount"))
}

#[cfg(target_os = "linux")]
fn set_private(p: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
}

/// Create a fresh empty `0700` mountpoint for `key`, clearing any stale leftover
/// (an empty dir from a previous, already-dead mount).
#[cfg(target_os = "linux")]
fn make_mountpoint(key: &str) -> Result<PathBuf, String> {
    use sha2::{Digest, Sha256};
    // Append a short hash of the FULL key so two distinct keys that sanitize to
    // the same prefix cannot collide onto one mountpoint (which would make
    // `is_mounted` report one vault's live mount as another's).
    let digest = hex::encode(Sha256::digest(key.as_bytes()));
    let name = format!("{}_{}", sanitize(key), &digest[..12]);
    let dir = mount_base_dir()?.join(name);
    if dir.exists() {
        // Only remove if empty (never clobber a live mount's contents).
        let _ = std::fs::remove_dir(&dir);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("create mountpoint: {e}"))?;
    set_private(&dir);
    Ok(dir)
}

/// True if `path` is currently a mountpoint, per `/proc/mounts`.
#[cfg(target_os = "linux")]
fn is_mounted(path: &std::path::Path) -> bool {
    let want = path.to_string_lossy();
    if let Ok(contents) = std::fs::read_to_string("/proc/mounts") {
        for line in contents.lines() {
            // fields: dev mountpoint fstype ... (mountpoint may be octal-escaped,
            // but our sanitized path has no spaces/specials, so a direct compare
            // is sufficient here).
            let mut it = line.split_whitespace();
            let _dev = it.next();
            if let Some(mp) = it.next() {
                if mp == want {
                    return true;
                }
            }
        }
    }
    false
}

/// Start an ephemeral read-only mount of an unlocked vault.
///
/// `kind` is `"cryptomator"` or `"aerovault"`; `vault_path` is the vault dir or
/// container file; `password` is forwarded to the child over stdin (empty = the
/// plaintext `.aerozip` lane for `aerovault`). Idempotent: if `key` is already
/// mounted, returns the existing mount.
#[cfg(target_os = "linux")]
pub async fn start(
    key: String,
    kind: String,
    vault_path: String,
    password: String,
    display_name: String,
) -> Result<VaultMountInfo, String> {
    use tokio::io::AsyncWriteExt;

    if kind != "cryptomator" && kind != "aerovault" {
        return Err(format!("Unknown vault kind: {kind}"));
    }

    {
        let active = VAULT_MOUNTS.lock().await;
        if let Some(m) = active.get(&key) {
            return Ok(VaultMountInfo {
                key: key.clone(),
                mountpoint: m.mountpoint.to_string_lossy().to_string(),
                kind: m.kind.clone(),
                display_name: m.display_name.clone(),
                pid: m.pid,
            });
        }
    }

    let mountpoint = make_mountpoint(&key)?;
    let cli = locate_cli()?;

    let mut cmd = tokio::process::Command::new(&cli);
    cmd.arg("mount-vault")
        .arg(&mountpoint)
        .arg("--kind")
        .arg(&kind)
        .arg("--vault-path")
        .arg(&vault_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn aeroftp-cli mount-vault: {e}"))?;

    // Hand the password to the child over stdin, then close it (EOF). The string
    // is written exactly (the CLI trims at most one trailing newline, which we do
    // not add), so the password round-trips byte-for-byte.
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(password.as_bytes()).await {
            let _ = child.start_kill();
            let _ = std::fs::remove_dir(&mountpoint);
            return Err(format!("Failed to send password to mount: {e}"));
        }
        let _ = stdin.shutdown().await; // close the pipe -> child sees EOF
    }
    drop(password);

    let pid = child.id().unwrap_or(0);

    // Wait for the FUSE session to attach, or for the child to exit early (e.g.
    // wrong password / bad vault). Poll up to ~8s.
    let mut mounted = false;
    for _ in 0..80 {
        if let Ok(Some(status)) = child.try_wait() {
            // Child exited before mounting: surface its stderr.
            let mut msg = format!("mount exited early (status {status})");
            if let Some(mut err) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let mut buf = String::new();
                let _ = err.read_to_string(&mut buf).await;
                let buf = buf.trim();
                if !buf.is_empty() {
                    msg = buf.lines().last().unwrap_or(buf).to_string();
                }
            }
            let _ = std::fs::remove_dir(&mountpoint);
            return Err(msg);
        }
        if is_mounted(&mountpoint) {
            mounted = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    if !mounted {
        let _ = child.start_kill();
        let _ = child.wait().await;
        let _ = std::fs::remove_dir(&mountpoint);
        return Err("Mount did not attach in time".to_string());
    }

    let info = VaultMountInfo {
        key: key.clone(),
        mountpoint: mountpoint.to_string_lossy().to_string(),
        kind: kind.clone(),
        display_name: display_name.clone(),
        pid,
    };
    VAULT_MOUNTS.lock().await.insert(
        key,
        ActiveVaultMount {
            child,
            mountpoint,
            kind,
            display_name,
            pid,
        },
    );
    Ok(info)
}

#[cfg(not(target_os = "linux"))]
pub async fn start(
    _key: String,
    _kind: String,
    _vault_path: String,
    _password: String,
    _display_name: String,
) -> Result<VaultMountInfo, String> {
    Err("Mounting an unlocked vault is only supported on Linux for now".to_string())
}

/// Stop and unmount the vault mount for `key` (no-op if not mounted). Graceful
/// SIGTERM first (the CLI unmounts cleanly), then force-kill, then remove the
/// mountpoint directory.
pub async fn stop(key: &str) -> Result<(), String> {
    let entry = {
        let mut active = VAULT_MOUNTS.lock().await;
        active.remove(key)
    };
    let Some(mut entry) = entry else {
        return Ok(());
    };
    terminate(&mut entry).await;
    let _ = std::fs::remove_dir(&entry.mountpoint);
    Ok(())
}

/// Stop every active vault mount (app quit).
pub async fn stop_all() {
    let drained: Vec<(String, ActiveVaultMount)> = {
        let mut active = VAULT_MOUNTS.lock().await;
        active.drain().collect()
    };
    for (_key, mut entry) in drained {
        terminate(&mut entry).await;
        let _ = std::fs::remove_dir(&entry.mountpoint);
    }
}

/// SIGTERM the child, wait up to 5s for a graceful FUSE unmount, then force-kill.
async fn terminate(entry: &mut ActiveVaultMount) {
    #[cfg(unix)]
    unsafe {
        if entry.pid > 0 {
            libc::kill(entry.pid as i32, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = entry.child.start_kill();
    }
    let waited = tokio::time::timeout(std::time::Duration::from_secs(5), entry.child.wait()).await;
    if waited.is_err() {
        let _ = entry.child.start_kill();
        let _ = entry.child.wait().await;
    }
}

/// Snapshot of all live vault mounts.
pub async fn list() -> Vec<VaultMountInfo> {
    let active = VAULT_MOUNTS.lock().await;
    active
        .iter()
        .map(|(key, m)| VaultMountInfo {
            key: key.clone(),
            mountpoint: m.mountpoint.to_string_lossy().to_string(),
            kind: m.kind.clone(),
            display_name: m.display_name.clone(),
            pid: m.pid,
        })
        .collect()
}

/// Open the mountpoint for `key` in the OS file manager.
pub async fn open_in_file_manager(key: &str) -> Result<(), String> {
    let mp = {
        let active = VAULT_MOUNTS.lock().await;
        active
            .get(key)
            .map(|m| m.mountpoint.clone())
            .ok_or_else(|| "Vault is not mounted".to_string())?
    };
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open").arg(&mp).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&mp).spawn();
    #[cfg(windows)]
    let result = std::process::Command::new("explorer").arg(&mp).spawn();
    result
        .map(|_| ())
        .map_err(|e| format!("Failed to open file manager: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_makes_one_safe_segment() {
        assert_eq!(
            sanitize("/home/u/My Vault.aerovault"),
            "home_u_My_Vault_aerovault"
        );
        assert_eq!(sanitize("////"), "vault");
        assert!(sanitize(&"x".repeat(200)).len() <= 48);
        assert!(!sanitize("a/b").contains('/'));
    }
}
