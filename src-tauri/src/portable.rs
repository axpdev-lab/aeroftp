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
const AEROFTP_DATA_RELEASE_LEAF: &str = "aeroftp";
const AEROFTP_DATA_DEBUG_LEAF: &str = "aeroftp-dev";

/// Cached portable-mode flag. Computed on first access and reused.
static PORTABLE_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
static LEGACY_APP_CONFIG_MIGRATED: OnceLock<()> = OnceLock::new();

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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Failed to secure {}: {e}", path.display()))?;
    }
    Ok(())
}

pub fn aeroftp_data_leaf_for_debug(debug: bool) -> &'static str {
    if debug {
        AEROFTP_DATA_DEBUG_LEAF
    } else {
        AEROFTP_DATA_RELEASE_LEAF
    }
}

fn aeroftp_data_leaf() -> &'static str {
    aeroftp_data_leaf_for_debug(cfg!(debug_assertions))
}

/// Single source of truth for AeroFTP's file-backed app state.
///
/// Release builds preserve the historical `aeroftp` leaf byte-for-byte. Debug
/// builds use the sibling `aeroftp-dev` leaf so `tauri dev` / `cargo run`
/// cannot read or mutate an installed release vault, sync journal, settings, or
/// SQLite history. Portable builds keep the same self-contained `<exe>/data`
/// root and apply the same release/debug leaf underneath it.
pub fn aeroftp_data_root() -> Option<PathBuf> {
    let leaf = aeroftp_data_leaf();
    if let Some(data_root) = portable_data_root() {
        let dir = data_root.join(leaf);
        ensure_dir(&dir).ok()?;
        return Some(dir);
    }
    let dir = dirs::config_dir()
        .or_else(dirs::home_dir)
        .map(|base| base.join(leaf))?;
    ensure_dir(&dir).ok()?;
    Some(dir)
}

/// Resolve the legacy Tauri identifier-scoped config directory used before the
/// unified data-root migration. This is read only as a release-build migration
/// source; debug builds intentionally do not copy release state into
/// `aeroftp-dev`.
fn legacy_app_config_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri::Manager;
    app.path().app_config_dir().ok()
}

fn legacy_cli_app_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|base| base.join(TAURI_APP_IDENTIFIER))
}

fn copy_missing_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o700));
        }
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            copy_missing_tree(&entry.path(), &dst.join(name))?;
        }
    } else if src.is_file() && !dst.exists() {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ = std::fs::copy(src, dst)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o600));
        }
    }
    Ok(())
}

fn migrate_legacy_app_config_dir(legacy_dir: Option<PathBuf>, new_dir: &Path) {
    if cfg!(debug_assertions) || is_portable() {
        return;
    }
    if LEGACY_APP_CONFIG_MIGRATED.get().is_some() {
        return;
    }
    let Some(legacy_dir) = legacy_dir else {
        return;
    };
    if !legacy_dir.is_dir() || legacy_dir == new_dir {
        return;
    }
    match copy_missing_tree(&legacy_dir, new_dir) {
        Ok(()) => tracing::info!(
            "Migrated legacy AeroFTP app config from {} to {}",
            legacy_dir.display(),
            new_dir.display()
        ),
        Err(e) => tracing::warn!(
            "Failed to migrate legacy AeroFTP app config from {} to {}: {}",
            legacy_dir.display(),
            new_dir.display(),
            e
        ),
    }
    let _ = LEGACY_APP_CONFIG_MIGRATED.set(());
}

/// Resolve the per-app config directory. In portable mode this is
/// `<exe-dir>/data/aeroftp` (or `aeroftp-dev` in debug); otherwise it is the
/// canonical AeroFTP data root.
///
/// This is the wrapper to use everywhere instead of calling
/// `app.path().app_config_dir()` directly. It keeps portable installs
/// self-contained and keeps debug builds isolated from release data.
pub fn app_config_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = aeroftp_data_root().ok_or_else(|| "Cannot resolve AeroFTP data root".to_string())?;
    migrate_legacy_app_config_dir(legacy_app_config_dir(app), &dir);
    Ok(dir)
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

/// Resolve the credential-store directory. Kept as a compatibility wrapper
/// because diagnostics and credential-store code already call this name.
pub fn credential_store_dir() -> Option<PathBuf> {
    aeroftp_data_root()
}

/// CLI-friendly resolution of the per-app config directory, mirroring
/// [`app_config_dir`] but without an `AppHandle`. Standalone binaries
/// (`aeroftp-cli`, `aerorsync_serve`) use this so a full keystore
/// export/import sees the same SQLite databases + plugin trees that
/// the GUI runtime would.
///
/// Returns `None` when no plausible config root exists (no `$HOME`,
/// no `%APPDATA%`). Callers should fall back to "vault only" mode in
/// that case rather than silently writing into the working directory.
pub fn cli_app_config_dir() -> Option<PathBuf> {
    let dir = aeroftp_data_root()?;
    migrate_legacy_app_config_dir(legacy_cli_app_config_dir(), &dir);
    Some(dir)
}

/// The Tauri identifier hard-coded in `tauri.conf.json`. Kept for legacy
/// migration from the old identifier-scoped config directory.
pub const TAURI_APP_IDENTIFIER: &str = "com.aeroftp.AeroFTP";

/// Resolve the WebView2 / WebKitGTK per-window data directory.
///
/// In portable mode this is `<exe-dir>/data/webview`. Two portable
/// installations of AeroFTP in different folders MUST NOT share WebView
/// state (localStorage, IndexedDB, cookies, cache) otherwise deleting a
/// saved server in one folder propagates to the other through the
/// identifier-scoped default folder Windows picks for WebView2.
///
/// Returns `None` when not running as portable: in that case the default
/// `WebviewWindowBuilder` behaviour (identifier-scoped folder under
/// `%LOCALAPPDATA%` / `~/.local/share`) is preserved for installed
/// builds so existing MSI/NSIS/.deb/.rpm users see no migration.
pub fn webview_data_dir() -> Option<PathBuf> {
    let data_root = portable_data_root()?;
    let dir = data_root.join("webview");
    ensure_dir(&dir).ok()?;
    Some(dir)
}

/// True when an `EBWebView` folder exists under the shared, identifier-scoped
/// `%LOCALAPPDATA%\com.aeroftp.AeroFTP` directory. Used by the portable
/// migration wizard to detect "there is legacy state from a previous
/// non-portable (or pre-isolation portable) install on this machine".
///
/// Only meaningful when running portable: an installed build expects that
/// directory to exist (it's its own state).
#[cfg(windows)]
pub fn shared_webview_data_present() -> bool {
    if !is_portable() {
        return false;
    }
    let Some(local_appdata) = dirs::data_local_dir() else {
        return false;
    };
    let candidate = local_appdata.join("com.aeroftp.AeroFTP").join("EBWebView");
    candidate.is_dir()
}

#[cfg(not(windows))]
pub fn shared_webview_data_present() -> bool {
    false
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
                if hive == HKEY_LOCAL_MACHINE {
                    "HKLM"
                } else {
                    "HKCU"
                },
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

    #[test]
    fn data_root_leaf_is_sibling_safe() {
        assert_eq!(aeroftp_data_leaf_for_debug(false), "aeroftp");
        assert_eq!(aeroftp_data_leaf_for_debug(true), "aeroftp-dev");
    }

    #[test]
    fn current_profile_data_root_uses_expected_leaf() {
        let Some(root) = aeroftp_data_root() else {
            return;
        };
        let expected = aeroftp_data_leaf_for_debug(cfg!(debug_assertions));
        assert_eq!(root.file_name().and_then(|s| s.to_str()), Some(expected));
    }
}
