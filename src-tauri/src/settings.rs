// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

#[cfg(feature = "proto_native_rsync")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "proto_native_rsync")]
use std::fs;
#[cfg(feature = "proto_native_rsync")]
use std::path::PathBuf;
#[cfg(feature = "proto_native_rsync")]
use std::sync::{LazyLock, Mutex};

#[cfg(feature = "proto_native_rsync")]
static SETTINGS_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[cfg(feature = "proto_native_rsync")]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct NativeRsyncSettings {
    #[serde(default)]
    enabled: bool,
}

#[cfg(feature = "proto_native_rsync")]
fn native_rsync_config_path() -> Result<PathBuf, String> {
    let base = dirs::config_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| "Cannot determine config directory".to_string())?;
    Ok(base.join("aeroftp").join("native_rsync.toml"))
}

#[cfg(feature = "proto_native_rsync")]
pub fn load_native_rsync_enabled() -> bool {
    let path = match native_rsync_config_path() {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!("native rsync settings path unavailable: {}", error);
            return false;
        }
    };

    if !path.exists() {
        return false;
    }

    match fs::read_to_string(&path) {
        Ok(content) => match toml::from_str::<NativeRsyncSettings>(&content) {
            Ok(settings) => settings.enabled,
            Err(error) => {
                tracing::warn!(
                    "native rsync settings parse failed ({}): {}",
                    path.display(),
                    error
                );
                false
            }
        },
        Err(error) => {
            tracing::warn!(
                "native rsync settings read failed ({}): {}",
                path.display(),
                error
            );
            false
        }
    }
}

#[cfg(feature = "proto_native_rsync")]
pub fn set_native_rsync_enabled(enabled: bool) -> Result<(), String> {
    let _lock = SETTINGS_WRITE_LOCK
        .lock()
        .map_err(|_| "Native rsync settings write lock poisoned".to_string())?;

    let path = native_rsync_config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    let content = toml::to_string_pretty(&NativeRsyncSettings { enabled })
        .map_err(|e| format!("Failed to serialize native rsync settings: {}", e))?;

    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, content).map_err(|e| format!("Failed to write temp config: {}", e))?;
    fs::rename(&tmp_path, &path).map_err(|e| format!("Failed to rename temp config: {}", e))?;
    Ok(())
}

#[tauri::command]
pub fn native_rsync_feature_compiled() -> bool {
    // Toggle must surface only where the native dispatch branch can
    // actually execute. `SftpProvider::delta_transport()` is
    // `#[cfg(unix)]`, so feature-on Windows / non-Unix builds would
    // otherwise advertise a toggle that has no runtime effect.
    cfg!(all(feature = "proto_native_rsync", unix))
}

#[cfg(feature = "proto_native_rsync")]
#[tauri::command]
pub fn native_rsync_enabled_get() -> bool {
    load_native_rsync_enabled()
}

#[cfg(feature = "proto_native_rsync")]
#[tauri::command]
pub fn native_rsync_enabled_set(enabled: bool) -> Result<(), String> {
    set_native_rsync_enabled(enabled)
}

// =============================================================================
// Tests (U-06): persistence semantics for the native rsync runtime toggle.
// =============================================================================
//
// The tests run the load/set helpers against a scratch config directory
// by overriding the resolver through a temp env var at runtime, so they
// do not poke the real `$XDG_CONFIG_HOME/aeroftp/native_rsync.toml`.
#[cfg(all(test, feature = "proto_native_rsync"))]
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
    fn load_returns_false_when_config_absent() {
        let _g = ScopedXdg::new();
        assert!(!load_native_rsync_enabled());
    }

    #[test]
    fn set_then_load_roundtrips_true() {
        let _g = ScopedXdg::new();
        set_native_rsync_enabled(true).expect("write ok");
        assert!(load_native_rsync_enabled());
    }

    #[test]
    fn set_then_load_roundtrips_false() {
        let _g = ScopedXdg::new();
        set_native_rsync_enabled(true).expect("enable ok");
        set_native_rsync_enabled(false).expect("disable ok");
        assert!(!load_native_rsync_enabled());
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
        // After a successful set, the `.tmp` sibling must not exist —
        // the rename is the atomic commit.
        let tmp = path.with_extension("tmp");
        assert!(!tmp.exists(), "temp file must be renamed away: {tmp:?}");
        assert!(path.exists(), "config file must exist after set");
    }

    #[test]
    fn feature_probe_is_unix_gated() {
        // U-05 pin: the probe must return `true` only on unix feature-on
        // builds. Inside this `#[cfg(feature = "proto_native_rsync")]`
        // module the compile-time answer depends on platform.
        assert_eq!(native_rsync_feature_compiled(), cfg!(unix));
    }
}
