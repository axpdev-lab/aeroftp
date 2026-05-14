// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#[cfg(feature = "aerorsync")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "aerorsync")]
use std::fs;
#[cfg(feature = "aerorsync")]
use std::path::PathBuf;
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
    let base = dirs::config_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| "Cannot determine config directory".to_string())?;
    Ok(base.join("aeroftp").join("native_rsync.toml"))
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
    !matches!(load_native_rsync_mode(), NativeRsyncMode::Classic)
}

#[cfg(feature = "aerorsync")]
pub fn load_native_rsync_mode() -> NativeRsyncMode {
    let path = match native_rsync_config_path() {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!("native rsync settings path unavailable: {}", error);
            return NativeRsyncMode::Classic;
        }
    };

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

    match fs::read_to_string(&path) {
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
    let _lock = SETTINGS_WRITE_LOCK
        .lock()
        .map_err(|_| "Native rsync settings write lock poisoned".to_string())?;

    let path = native_rsync_config_path()?;
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
    fs::rename(&tmp_path, &path).map_err(|e| format!("Failed to rename temp config: {}", e))?;
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

#[cfg(feature = "aerorsync")]
#[tauri::command]
pub fn native_rsync_enabled_get() -> bool {
    load_native_rsync_enabled()
}

#[cfg(feature = "aerorsync")]
#[tauri::command]
pub fn native_rsync_enabled_set(enabled: bool) -> Result<(), String> {
    set_native_rsync_enabled(enabled)
}

#[cfg(feature = "aerorsync")]
#[tauri::command]
pub fn native_rsync_mode_get() -> String {
    match load_native_rsync_mode() {
        NativeRsyncMode::Auto => "auto",
        NativeRsyncMode::Classic => "classic",
        NativeRsyncMode::Native => "native",
    }
    .to_string()
}

#[cfg(feature = "aerorsync")]
#[tauri::command]
pub fn native_rsync_mode_set(mode: String) -> Result<(), String> {
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
    set_native_rsync_mode(mode)
}

// =============================================================================
// Tests (U-06): persistence semantics for the native rsync runtime toggle.
// =============================================================================
//
// The tests run the load/set helpers against a scratch config directory
// by overriding the resolver through a temp env var at runtime, so they
// do not poke the real `$XDG_CONFIG_HOME/aeroftp/native_rsync.toml`.
#[cfg(all(test, feature = "aerorsync"))]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialise tests that touch the process-wide env var used to
    // redirect `dirs::config_dir` via `XDG_CONFIG_HOME`. `cargo test`
    // otherwise races and flakes.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct ScopedXdg {
        _guard: std::sync::MutexGuard<'static, ()>,
        _tempdir: tempfile::TempDir,
        prior: Option<std::ffi::OsString>,
    }

    impl ScopedXdg {
        fn new() -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let tempdir = tempfile::tempdir().expect("tempdir");
            let prior = std::env::var_os("XDG_CONFIG_HOME");
            std::env::set_var("XDG_CONFIG_HOME", tempdir.path());
            Self {
                _guard: guard,
                _tempdir: tempdir,
                prior,
            }
        }
    }

    impl Drop for ScopedXdg {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    #[test]
    fn load_returns_true_when_config_absent() {
        // Z.1.5 (2026-05-12): fresh-install default flipped to ON after the
        // host-key algorithm negotiation asymmetry was fixed.
        let _g = ScopedXdg::new();
        assert!(load_native_rsync_enabled());
        assert_eq!(load_native_rsync_mode(), NativeRsyncMode::Auto);
    }

    #[test]
    fn set_then_load_roundtrips_true() {
        let _g = ScopedXdg::new();
        set_native_rsync_enabled(true).expect("write ok");
        assert!(load_native_rsync_enabled());
        assert_eq!(load_native_rsync_mode(), NativeRsyncMode::Auto);
    }

    #[test]
    fn set_then_load_roundtrips_false() {
        let _g = ScopedXdg::new();
        set_native_rsync_enabled(true).expect("enable ok");
        set_native_rsync_enabled(false).expect("disable ok");
        assert!(!load_native_rsync_enabled());
        assert_eq!(load_native_rsync_mode(), NativeRsyncMode::Classic);
    }

    #[test]
    fn set_then_load_roundtrips_native_mode() {
        let _g = ScopedXdg::new();
        set_native_rsync_mode(NativeRsyncMode::Native).expect("native ok");
        assert!(load_native_rsync_enabled());
        assert_eq!(load_native_rsync_mode(), NativeRsyncMode::Native);
    }

    #[test]
    fn legacy_enabled_toml_maps_to_auto() {
        let _g = ScopedXdg::new();
        let path = native_rsync_config_path().expect("path");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"enabled = true\n").unwrap();
        assert_eq!(load_native_rsync_mode(), NativeRsyncMode::Auto);
    }

    #[test]
    fn legacy_disabled_toml_maps_to_classic() {
        let _g = ScopedXdg::new();
        let path = native_rsync_config_path().expect("path");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"enabled = false\n").unwrap();
        assert_eq!(load_native_rsync_mode(), NativeRsyncMode::Classic);
    }

    #[test]
    fn malformed_config_falls_back_to_disabled_and_does_not_panic() {
        let _g = ScopedXdg::new();
        // Write garbage directly to the target file, simulating a
        // partial write or a user mistake.
        let path = native_rsync_config_path().expect("path");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"this is <<not toml>>").unwrap();
        assert!(
            !load_native_rsync_enabled(),
            "malformed config must be treated as disabled (opt-in by user action only)"
        );
    }

    #[test]
    fn set_uses_atomic_temp_rename() {
        let _g = ScopedXdg::new();
        let path = native_rsync_config_path().expect("path");
        set_native_rsync_enabled(true).unwrap();
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
