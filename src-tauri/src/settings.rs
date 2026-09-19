// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#[cfg(feature = "aerorsync")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "aerorsync")]
use std::fs;
#[cfg(feature = "aerorsync")]
use std::path::{Path, PathBuf};
#[cfg(feature = "aerorsync")]
use std::sync::{LazyLock, Mutex};

#[cfg(feature = "aerorsync")]
static SETTINGS_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[cfg(feature = "aerorsync")]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct NativeRsyncSettings {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    mode: Option<NativeRsyncMode>,
}

#[cfg(feature = "aerorsync")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NativeRsyncMode {
    Auto,
    Classic,
    Native,
}

#[cfg(feature = "aerorsync")]
impl NativeRsyncSettings {
    fn effective_mode(&self) -> NativeRsyncMode {
        self.mode.unwrap_or(if self.enabled {
            NativeRsyncMode::Auto
        } else {
            NativeRsyncMode::Classic
        })
    }
}

#[cfg(feature = "aerorsync")]
fn native_rsync_config_path() -> Result<PathBuf, String> {
    Ok(crate::portable::aeroftp_data_root()
        .ok_or_else(|| "Cannot determine AeroFTP data root".to_string())?
        .join("native_rsync.toml"))
}

#[cfg(feature = "aerorsync")]
/// Runtime gate for the `aerorsync` native rsync backend.
///
/// Fresh installs default to **ON** since Z.1.5 (2026-05-12): the host-key
/// algorithm negotiation asymmetry that previously kept this OFF has been
/// fixed by aligning the host-key algorithm preference between the libssh2
/// leg (`ssh_transport.rs::AERORSYNC_HOST_KEY_ALGS`) and the russh leg
/// (`russh_session_transport.rs` `Preferred.key`). Both legs now select the
/// same host key on servers exposing multiple algorithms, so SHA-256
/// fingerprint pinning is deterministic across reconnects.
///
/// The function name, the persisted TOML filename (`native_rsync.toml`) and
/// the `native_rsync_enabled` TOML key all retain the legacy naming that
/// predated the `aerorsync` rebrand: renaming them would break upgrade
/// paths for users who already toggled the flag on.
pub fn load_native_rsync_enabled() -> bool {
    mode_is_enabled(load_native_rsync_mode())
}

#[cfg(feature = "aerorsync")]
fn mode_is_enabled(mode: NativeRsyncMode) -> bool {
    !matches!(mode, NativeRsyncMode::Classic)
}

#[cfg(feature = "aerorsync")]
pub fn load_native_rsync_mode() -> NativeRsyncMode {
    match native_rsync_config_path() {
        Ok(path) => load_native_rsync_mode_from(&path),
        Err(error) => {
            tracing::warn!("native rsync settings path unavailable: {}", error);
            NativeRsyncMode::Classic
        }
    }
}

/// The loader proper, on an explicit file. Split from the resolver so the
/// tests can point it at a scratch file: the data root comes from
/// `dirs::config_dir()`, which follows `XDG_CONFIG_HOME` on Linux only, so
/// redirecting that variable left the tests reading and writing the real
/// `%APPDATA%` / `~/Library/Application Support` settings on Windows and macOS.
#[cfg(feature = "aerorsync")]
fn load_native_rsync_mode_from(path: &Path) -> NativeRsyncMode {
    if !path.exists() {
        // Fresh-install default: ON since Z.1.5 (2026-05-12). Users who
        // previously set the toggle (either ON or OFF) keep their stored
        // value because the TOML exists; only first-run installs hit this
        // branch and benefit from cross-OS delta sync out of the box.
        //
        // Historical context: from commit `aca4577c` (2026-04-XX) through
        // Z.1.4 closure (2026-05-12), the default was OFF because the two
        // SSH libraries (`ssh2` for classic SFTP, `russh` for the native
        // probe) negotiated different host-key algorithms on servers
        // exposing more than one, producing fingerprint pinning mismatches.
        // The fix lives in `AERORSYNC_HOST_KEY_ALGS` (see ssh_transport.rs)
        // and the russh `Preferred.key` override in russh_session_transport.
        return NativeRsyncMode::Auto;
    }

    match fs::read_to_string(path) {
        Ok(content) => match toml::from_str::<NativeRsyncSettings>(&content) {
            Ok(settings) => settings.effective_mode(),
            Err(error) => {
                tracing::warn!(
                    "native rsync settings parse failed ({}): {}",
                    path.display(),
                    error
                );
                NativeRsyncMode::Classic
            }
        },
        Err(error) => {
            tracing::warn!(
                "native rsync settings read failed ({}): {}",
                path.display(),
                error
            );
            NativeRsyncMode::Classic
        }
    }
}

#[cfg(feature = "aerorsync")]
pub fn set_native_rsync_enabled(enabled: bool) -> Result<(), String> {
    set_native_rsync_mode(if enabled {
        NativeRsyncMode::Auto
    } else {
        NativeRsyncMode::Classic
    })
}

#[cfg(feature = "aerorsync")]
pub fn set_native_rsync_mode(mode: NativeRsyncMode) -> Result<(), String> {
    set_native_rsync_mode_at(&native_rsync_config_path()?, mode)
}

/// The writer proper, on an explicit file; see `load_native_rsync_mode_from`.
#[cfg(feature = "aerorsync")]
fn set_native_rsync_mode_at(path: &Path, mode: NativeRsyncMode) -> Result<(), String> {
    let _lock = SETTINGS_WRITE_LOCK
        .lock()
        .map_err(|_| "Native rsync settings write lock poisoned".to_string())?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    let content = toml::to_string_pretty(&NativeRsyncSettings {
        enabled: !matches!(mode, NativeRsyncMode::Classic),
        mode: Some(mode),
    })
    .map_err(|e| format!("Failed to serialize native rsync settings: {}", e))?;

    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, content).map_err(|e| format!("Failed to write temp config: {}", e))?;
    fs::rename(&tmp_path, path).map_err(|e| format!("Failed to rename temp config: {}", e))?;
    Ok(())
}

#[tauri::command]
pub fn native_rsync_feature_compiled() -> bool {
    // Post PR-T11: the native dispatch in `SftpProvider::delta_transport()`
    // is cross-platform. The toggle is eligible on any OS that compiled
    // with the `aerorsync` cargo feature, Windows included.
    // The binary-rsync classic fallback is still Unix-only; Windows
    // without the feature drops to plain SFTP silently (handled inside
    // `classic_binary_fallback`).
    cfg!(feature = "aerorsync")
}

// The four accessors below all reach `native_rsync.toml` under the AeroFTP data
// root: the getters stat and read it, the setters take a process-wide
// write lock and then do write + rename. That is disk I/O plus a lock on a
// config directory that can perfectly well be on a network home, so none of
// them belongs on the main thread, however small the value they return is.

#[cfg(feature = "aerorsync")]
#[tauri::command]
pub async fn native_rsync_enabled_get() -> bool {
    tokio::task::spawn_blocking(load_native_rsync_enabled)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!("native_rsync_enabled_get task failed: {err}");
            // Same fallback the loader itself uses when the path is unavailable.
            false
        })
}

#[cfg(feature = "aerorsync")]
#[tauri::command]
pub async fn native_rsync_enabled_set(enabled: bool) -> Result<(), String> {
    tokio::task::spawn_blocking(move || set_native_rsync_enabled(enabled))
        .await
        .unwrap_or_else(|err| Err(format!("native rsync settings write failed: {err}")))
}

/// The persisted mode as the string the GUI and the CLI both speak.
///
/// Synchronous, and stays that way: the CLI calls it from plain `fn`s and has
/// no main thread to protect. The Tauri command below is the wrapper that does.
#[cfg(feature = "aerorsync")]
pub fn native_rsync_mode_str() -> String {
    match load_native_rsync_mode() {
        NativeRsyncMode::Auto => "auto",
        NativeRsyncMode::Classic => "classic",
        NativeRsyncMode::Native => "native",
    }
    .to_string()
}

#[cfg(feature = "aerorsync")]
#[tauri::command]
pub async fn native_rsync_mode_get() -> String {
    tokio::task::spawn_blocking(native_rsync_mode_str)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!("native_rsync_mode_get task failed: {err}");
            // Same fallback the loader itself uses when the path is unavailable.
            "classic".to_string()
        })
}

#[cfg(feature = "aerorsync")]
#[tauri::command]
pub async fn native_rsync_mode_set(mode: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || native_rsync_mode_set_blocking(mode))
        .await
        .unwrap_or_else(|err| Err(format!("native rsync settings write failed: {err}")))
}

#[cfg(feature = "aerorsync")]
fn native_rsync_mode_set_blocking(mode: String) -> Result<(), String> {
    let mode = match mode.as_str() {
        "auto" => NativeRsyncMode::Auto,
        "classic" => NativeRsyncMode::Classic,
        "native" => NativeRsyncMode::Native,
        other => {
            return Err(format!(
                "Invalid native rsync mode `{other}`; expected auto, classic or native"
            ))
        }
    };
    // Z.4.5 R2: refuse `classic` when no rsync binary is reachable on
    // PATH (typical Windows install). Otherwise the GUI would persist
    // a mode that every subsequent transfer has to silently work
    // around. `auto` and `native` are always accepted because the
    // native engine is built-in.
    if matches!(mode, NativeRsyncMode::Classic) && detect_classic_rsync_path().is_none() {
        return Err(
            "classic mode rejected: no rsync binary on PATH. Install rsync (Linux/macOS \
             package, Windows via WSL/cygwin/scoop) or use `native` / `auto`."
                .to_string(),
        );
    }
    set_native_rsync_mode(mode)
}

/// Whether the classic `rsync` (or `rsync.exe` on Windows) binary is
/// reachable on PATH. Used by the Settings panel to disable the
/// `classic` toggle so the operator does not select a mode the
/// machine cannot honor. Returns `(available, optional_path)` so the
/// UI can tell the user *where* the binary was found when they hover
/// the chip.
///
/// `async`: `detect_classic_rsync_path` walks every entry of `PATH` and calls
/// `is_file()` on each candidate. A single `PATH` entry pointing at an
/// unreachable network mount is enough to park that walk in the kernel, and on
/// a sync command the window parks with it.
#[cfg(feature = "aerorsync")]
#[tauri::command]
pub async fn native_rsync_classic_available() -> ClassicRsyncAvailability {
    tokio::task::spawn_blocking(|| match detect_classic_rsync_path() {
        Some(path) => ClassicRsyncAvailability {
            available: true,
            path: Some(path.display().to_string()),
        },
        None => ClassicRsyncAvailability {
            available: false,
            path: None,
        },
    })
    .await
    .unwrap_or_else(|err| {
        tracing::warn!("native_rsync_classic_available task failed: {err}");
        ClassicRsyncAvailability {
            available: false,
            path: None,
        }
    })
}

#[cfg(feature = "aerorsync")]
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClassicRsyncAvailability {
    pub available: bool,
    pub path: Option<String>,
}

/// PATH walk for the classic rsync binary. Pure filesystem check, no
/// subprocess, no version probe. Returns the first hit so the caller
/// can render the resolved path back to the user.
#[cfg(feature = "aerorsync")]
fn detect_classic_rsync_path() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "rsync.exe" } else { "rsync" };
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// =============================================================================
// Tests (U-06): persistence semantics for the native rsync runtime toggle.
// =============================================================================
//
// Every test works on a file inside its own temporary directory and calls the
// path-taking loader and writer. Nothing here touches the environment: an
// earlier version redirected `XDG_CONFIG_HOME`, which moves `dirs::config_dir`
// on Linux only, so on Windows and macOS the tests shared the real settings
// file and failed depending on the order they ran in.
#[cfg(all(test, feature = "aerorsync"))]
mod tests {
    use super::*;

    struct Scratch {
        _dir: tempfile::TempDir,
        path: PathBuf,
    }

    fn scratch() -> Scratch {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("aeroftp").join("native_rsync.toml");
        Scratch { _dir: dir, path }
    }

    #[test]
    fn load_returns_true_when_config_absent() {
        // Z.1.5 (2026-05-12): fresh-install default flipped to ON after the
        // host-key algorithm negotiation asymmetry was fixed.
        let s = scratch();
        assert!(mode_is_enabled(load_native_rsync_mode_from(&s.path)));
        assert_eq!(load_native_rsync_mode_from(&s.path), NativeRsyncMode::Auto);
    }

    #[test]
    fn set_then_load_roundtrips_true() {
        let s = scratch();
        set_native_rsync_mode_at(&s.path, NativeRsyncMode::Auto).expect("write ok");
        assert!(mode_is_enabled(load_native_rsync_mode_from(&s.path)));
        assert_eq!(load_native_rsync_mode_from(&s.path), NativeRsyncMode::Auto);
    }

    #[test]
    fn set_then_load_roundtrips_false() {
        let s = scratch();
        set_native_rsync_mode_at(&s.path, NativeRsyncMode::Auto).expect("enable ok");
        set_native_rsync_mode_at(&s.path, NativeRsyncMode::Classic).expect("disable ok");
        assert!(!mode_is_enabled(load_native_rsync_mode_from(&s.path)));
        assert_eq!(
            load_native_rsync_mode_from(&s.path),
            NativeRsyncMode::Classic
        );
    }

    #[test]
    fn set_then_load_roundtrips_native_mode() {
        let s = scratch();
        set_native_rsync_mode_at(&s.path, NativeRsyncMode::Native).expect("native ok");
        assert!(mode_is_enabled(load_native_rsync_mode_from(&s.path)));
        assert_eq!(
            load_native_rsync_mode_from(&s.path),
            NativeRsyncMode::Native
        );
    }

    #[test]
    fn legacy_enabled_toml_maps_to_auto() {
        let s = scratch();
        let path = &s.path;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"enabled = true\n").unwrap();
        assert_eq!(load_native_rsync_mode_from(&s.path), NativeRsyncMode::Auto);
    }

    #[test]
    fn legacy_disabled_toml_maps_to_classic() {
        let s = scratch();
        let path = &s.path;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"enabled = false\n").unwrap();
        assert_eq!(
            load_native_rsync_mode_from(&s.path),
            NativeRsyncMode::Classic
        );
    }

    #[test]
    fn malformed_config_falls_back_to_disabled_and_does_not_panic() {
        let s = scratch();
        // Write garbage directly to the target file, simulating a
        // partial write or a user mistake.
        let path = &s.path;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"this is <<not toml>>").unwrap();
        assert!(
            !mode_is_enabled(load_native_rsync_mode_from(&s.path)),
            "malformed config must be treated as disabled (opt-in by user action only)"
        );
    }

    #[test]
    fn set_uses_atomic_temp_rename() {
        let s = scratch();
        let path = &s.path;
        set_native_rsync_mode_at(&s.path, NativeRsyncMode::Auto).unwrap();
        // After a successful set, the `.tmp` sibling must not exist -
        // the rename is the atomic commit.
        let tmp = path.with_extension("tmp");
        assert!(!tmp.exists(), "temp file must be renamed away: {tmp:?}");
        assert!(path.exists(), "config file must exist after set");
    }

    #[test]
    fn feature_probe_reports_compiled_feature_cross_platform() {
        // PR-T11 made native dispatch cross-platform. Inside this
        // `#[cfg(feature = "aerorsync")]` module the command must
        // report the compiled feature on every OS, Windows included.
        assert!(native_rsync_feature_compiled());
    }
}
