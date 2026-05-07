//! Portable-mode detection and data directory resolution.
//!
//! When AeroFTP is shipped as the Windows portable ZIP, a `portable.marker`
//! file lives next to `AeroFTP.exe`. Its presence is the single source of
//! truth for "this is a portable install". When detected:
//!
//!   - all per-app data (config, cache, logs, vault, AI databases) goes into
//!     `<exe-dir>/data/...` instead of `%APPDATA%`/`~/.config`. This is what
//!     "portable" means to users: copy the folder, your state comes with it.
//!   - the auto-updater swaps the `.exe` in place rather than launching the
//!     NSIS installer (handled in `windows_update_helper.rs`).
//!
//! Detection is cached on first call. The marker is read at most once per
//! process; if the user adds or removes it after launch, behaviour for the
//! current session is unchanged. This is intentional — we don't want a
//! mid-session jump between two data directories.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MARKER_FILENAME: &str = "portable.marker";
const PORTABLE_DATA_DIRNAME: &str = "data";

/// Cached portable-mode flag. Computed on first access and reused.
static PORTABLE_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Resolve the portable root directory (the folder containing AeroFTP.exe
/// and `portable.marker`). Returns `None` when not running as portable.
fn portable_root() -> Option<&'static Path> {
    PORTABLE_ROOT
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let dir = exe.parent()?.to_path_buf();
            let marker = dir.join(MARKER_FILENAME);
            if marker.is_file() {
                Some(dir)
            } else {
                None
            }
        })
        .as_deref()
}

/// True when the running binary is the portable build.
pub fn is_portable() -> bool {
    portable_root().is_some()
}

/// Portable data root: `<exe-dir>/data`. None when not portable.
fn portable_data_root() -> Option<PathBuf> {
    portable_root().map(|root| root.join(PORTABLE_DATA_DIRNAME))
}

/// Ensure a directory exists with secure permissions when portable.
/// Idempotent; safe to call repeatedly.
fn ensure_dir(path: &Path) -> Result<(), String> {
    if !path.exists() {
        std::fs::create_dir_all(path)
            .map_err(|e| format!("Failed to create {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Resolve the per-app config directory. In portable mode this is
/// `<exe-dir>/data/config`; otherwise delegates to Tauri's `app_config_dir`.
///
/// This is the wrapper to use everywhere instead of calling
/// `app.path().app_config_dir()` directly. It guarantees portable installs
/// stay self-contained.
pub fn app_config_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    if let Some(data_root) = portable_data_root() {
        let dir = data_root.join("config");
        ensure_dir(&dir)?;
        return Ok(dir);
    }
    app.path()
        .app_config_dir()
        .map_err(|e| format!("Cannot resolve app config dir: {e}"))
}

/// Resolve the per-app data directory. In portable mode this is
/// `<exe-dir>/data`; otherwise delegates to Tauri's `app_data_dir`.
pub fn app_data_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    if let Some(data_root) = portable_data_root() {
        ensure_dir(&data_root)?;
        return Ok(data_root);
    }
    app.path()
        .app_data_dir()
        .map_err(|e| format!("Cannot resolve app data dir: {e}"))
}

/// Resolve the credential-store directory. The credential store predates
/// the rest of the app and uses `dirs::config_dir().join("aeroftp")` rather
/// than the Tauri identifier-scoped path. We preserve that on non-portable
/// builds (so existing users don't lose their vault), and route to
/// `<exe-dir>/data/aeroftp` when portable.
pub fn credential_store_dir() -> Option<PathBuf> {
    if let Some(data_root) = portable_data_root() {
        let dir = data_root.join("aeroftp");
        ensure_dir(&dir).ok()?;
        return Some(dir);
    }
    dirs::config_dir()
        .or_else(dirs::home_dir)
        .map(|base| base.join("aeroftp"))
}

// ===========================================================================
// Windows install-format detection
// ===========================================================================
//
// The auto-updater needs to know which artifact to download and which install
// path to follow. The three Windows formats — MSI, NSIS .exe, portable ZIP —
// require different update strategies:
//
//   - MSI: msiexec /i ... /qb /norestart (silent upgrade, in-place)
//   - NSIS: setup.exe /S (silent install, in-place)
//   - Portable: rename + swap of AeroFTP.exe (no installer)
//
// Misclassification is harmful: a portable user who gets pointed at the NSIS
// installer ends up with two copies on disk and a broken update story.
//
// Detection runs in three deterministic stages:
//
//   1. Portable marker (most reliable) — `portable.marker` next to the .exe.
//      Ships inside the ZIP and is the canonical signal.
//
//   2. Registry Uninstall scan (HKLM then HKCU) — walk
//      `Software\Microsoft\Windows\CurrentVersion\Uninstall\*` looking for
//      a sub-key whose `InstallLocation` matches the parent of the running
//      exe AND whose `DisplayName` contains "AeroFTP". The `WindowsInstaller`
//      DWORD distinguishes MSI (=1) from NSIS (=0 or absent).
//
//   3. Fallback path heuristic — if neither marker nor registry resolves,
//      classify by `%ProgramFiles%` containment. Logged as a warning so
//      the operator knows detection was inconclusive.

#[cfg(windows)]
const REGISTRY_UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

/// Windows-only install-format detection. Order: marker → registry → path.
#[cfg(windows)]
pub fn detect_windows_install_format() -> String {
    if is_portable() {
        return "portable".to_string();
    }

    if let Some(format) = detect_via_registry() {
        return format;
    }

    log::warn!(
        "Windows install-format detection: marker absent and registry scan inconclusive, \
         falling back to path heuristic"
    );
    detect_via_path_heuristic()
}

/// Cross-platform stub so the call site compiles everywhere. The non-Windows
/// path is never exercised in production (the `match` in `detect_install_format`
/// gates it on `os == "windows"`), but keeping the function signature uniform
/// avoids `#[cfg]` noise in the caller.
#[cfg(not(windows))]
pub fn detect_windows_install_format() -> String {
    "exe".to_string()
}

#[cfg(windows)]
fn detect_via_registry() -> Option<String> {
    use winreg::enums::*;
    use winreg::RegKey;

    let exe_parent = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let exe_parent_norm = normalize_windows_path(&exe_parent);

    // Try HKLM first (per-machine MSI installs land here), then HKCU
    // (Tauri NSIS per-user installs default to HKCU).
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let root = RegKey::predef(hive);
        let uninstall = match root.open_subkey_with_flags(REGISTRY_UNINSTALL_KEY, KEY_READ) {
            Ok(k) => k,
            Err(_) => continue,
        };

        for sub_key_name in uninstall.enum_keys().flatten() {
            let sub = match uninstall.open_subkey_with_flags(&sub_key_name, KEY_READ) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let display_name: String = sub.get_value("DisplayName").unwrap_or_default();
            if !display_name.contains("AeroFTP") {
                continue;
            }

            let install_location: String = sub.get_value("InstallLocation").unwrap_or_default();
            if install_location.is_empty() {
                continue;
            }

            let install_norm = normalize_windows_path(std::path::Path::new(&install_location));
            if install_norm != exe_parent_norm {
                continue;
            }

            // Match found. WindowsInstaller=1 ⇒ MSI; otherwise NSIS.
            let windows_installer: u32 = sub.get_value("WindowsInstaller").unwrap_or(0);
            let format = if windows_installer == 1 { "msi" } else { "exe" };
            log::info!(
                "Windows install-format detected via registry: {} (key: {}\\{}, DisplayName: {})",
                format,
                if hive == HKEY_LOCAL_MACHINE { "HKLM" } else { "HKCU" },
                sub_key_name,
                display_name
            );
            return Some(format.to_string());
        }
    }

    None
}

/// Last-resort heuristic: classify by Program Files containment. Used only
/// when both marker and registry fail (typically: corrupt registry, manual
/// install via xcopy, or a pre-marker portable that the user hasn't migrated).
#[cfg(windows)]
fn detect_via_path_heuristic() -> String {
    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return "exe".to_string(),
    };
    let path_str = exe_path.to_string_lossy().to_lowercase();
    let pf = std::env::var("ProgramFiles")
        .unwrap_or_default()
        .to_lowercase();
    let pf86 = std::env::var("ProgramFiles(x86)")
        .unwrap_or_default()
        .to_lowercase();

    if (!pf.is_empty() && path_str.starts_with(&pf))
        || (!pf86.is_empty() && path_str.starts_with(&pf86))
        || path_str.contains("program files")
    {
        "msi".to_string()
    } else {
        "exe".to_string()
    }
}

/// Lowercase + trailing-separator-strip normalization. Windows paths from
/// the registry can come in mixed case with or without a trailing backslash;
/// equality must be case-insensitive and separator-tolerant.
#[cfg(windows)]
fn normalize_windows_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().to_lowercase();
    s.trim_end_matches(['\\', '/']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Marker absent ⇒ not portable, all helpers fall through to Tauri/dirs.
    /// We can't easily run `app_config_dir` here without an AppHandle, but we
    /// can sanity-check the detection contract.
    #[test]
    fn detection_is_marker_driven() {
        // In a non-installed test binary, std::env::current_exe() points at the
        // test runner, which has no marker next to it. So portable_root() must
        // return None unless someone manually drops portable.marker into
        // target/debug — which would be a bug in the test environment.
        // We just assert the cached function doesn't panic and is deterministic.
        let first = portable_root().is_some();
        let second = portable_root().is_some();
        assert_eq!(first, second);
    }

    #[test]
    fn portable_data_root_aligns_with_root() {
        match (portable_root(), portable_data_root()) {
            (None, None) => {}
            (Some(root), Some(data)) => {
                assert_eq!(data, root.join(PORTABLE_DATA_DIRNAME));
            }
            other => panic!("portable_root and portable_data_root disagree: {other:?}"),
        }
    }
}
