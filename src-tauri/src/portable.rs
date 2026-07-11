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
    // The tree we import here is a config tree the user consented to copy, but a
    // symlink inside it can point anywhere: outside the consented tree (dragging
    // in foreign secrets like ~/.ssh) or at an ancestor, forming a cycle that
    // would recurse until path-length exhaustion. So we never follow links, we
    // skip them; skipping also kills the cycle recursion. `symlink_metadata`
    // never follows the link; a metadata error means the path is gone, and the
    // `is_dir`/`is_file` checks below already no-op on a missing path.
    if let Ok(meta) = src.symlink_metadata() {
        if meta.file_type().is_symlink() {
            return Ok(());
        }
    }
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

// ===========================================================================
// Flatpak host-config import (B3)
// ===========================================================================
//
// A Flatpak install redirects XDG_CONFIG_HOME into ~/.var/app/<id>/config, so a
// user moving from the .deb gets a brand-new, empty data root: their saved
// servers and encrypted vault become invisible (not lost, not corrupted). For an
// encrypted-credential app a fresh data root reads as "I lost my passwords",
// which is the trap we must not spring.
//
// With --filesystem=home (granted by the Flatpak manifest for the local pane
// anyway) the sandbox can still read the real ~/.config/aeroftp. We offer a
// one-time, consent-gated import that copies only what is missing and never
// overwrites, rather than a silent copy: relocating an encrypted vault should be
// the user's explicit choice, and a user who deliberately wanted a clean
// sandboxed install should not be surprised. A marker records the decision so
// the offer is made exactly once.

const FLATPAK_IMPORT_DECIDED_MARKER: &str = ".flatpak-host-import-decided";

/// True when running inside a Flatpak sandbox. Mirrors the `FLATPAK_ID` signal
/// used by `detect_install_format()` in `lib.rs`; do not invent a second one.
pub fn is_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

/// Testable core of [`host_config_dir_under_flatpak`]. Kept pure (no env, no
/// implicit filesystem beyond the `is_dir` probe passed in) so the branch logic
/// is unit-tested without a real sandbox.
fn host_config_dir_impl(
    is_flatpak: bool,
    home: Option<PathBuf>,
    leaf: &str,
    current: Option<PathBuf>,
    is_dir: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if !is_flatpak {
        return None;
    }
    let candidate = home?.join(".config").join(leaf);
    // A no-op (candidate == data root) or a non-existent host config is nothing
    // to import; bail so the caller never offers an empty or self-referential
    // migration.
    if current.as_deref() == Some(candidate.as_path()) || !is_dir(&candidate) {
        return None;
    }
    Some(candidate)
}

/// The real host `~/.config/<leaf>` as seen from inside a Flatpak sandbox
/// (visible thanks to `--filesystem=home`). `None` when not under Flatpak, when
/// that directory does not exist, or when it resolves to the current data root.
///
/// `$HOME` inside the sandbox is the real host home, while `dirs::config_dir()`
/// is redirected into the sandbox, so the host path is built from `$HOME`
/// directly rather than from the redirected XDG base.
pub fn host_config_dir_under_flatpak() -> Option<PathBuf> {
    host_config_dir_impl(
        is_flatpak(),
        dirs::home_dir(),
        aeroftp_data_leaf(),
        aeroftp_data_root(),
        |p| p.is_dir(),
    )
}

/// Whether a first-run host-config import should be offered, and the paths.
#[derive(Debug, Clone)]
pub struct FlatpakImportStatus {
    /// True when the offer should be shown: under Flatpak, host config present,
    /// and the user has not already accepted or declined.
    pub available: bool,
    pub source: Option<PathBuf>,
    pub target: Option<PathBuf>,
}

/// Outcome of an import decision.
#[derive(Debug, Clone)]
pub struct FlatpakImportReport {
    pub imported: bool,
    pub source: Option<PathBuf>,
    pub target: Option<PathBuf>,
}

fn flatpak_import_marker_path() -> Option<PathBuf> {
    aeroftp_data_root().map(|d| d.join(FLATPAK_IMPORT_DECIDED_MARKER))
}

fn flatpak_import_decided() -> bool {
    flatpak_import_marker_path()
        .map(|m| m.exists())
        .unwrap_or(false)
}

fn write_flatpak_import_marker() {
    if let Some(marker) = flatpak_import_marker_path() {
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&marker, b"decided\n");
    }
}

/// Should the first-run host-config import prompt be shown, and from/to where.
pub fn flatpak_host_import_status() -> FlatpakImportStatus {
    let source = host_config_dir_under_flatpak();
    let target = aeroftp_data_root();
    FlatpakImportStatus {
        available: source.is_some() && !flatpak_import_decided(),
        source,
        target,
    }
}

/// Apply (`accept = true`) or decline (`accept = false`) the host-config import.
///
/// On accept, copy the host config into the sandbox data root with
/// `copy_missing_tree`, which copies only absent files and never overwrites, so
/// re-running it is safe and a partially set-up sandbox is preserved. Either way
/// the decision is recorded so the prompt is not shown again. The vault is
/// copied as an encrypted blob: it unlocks only with the master password, and
/// the import moves the blob, it does not unlock anything.
pub fn flatpak_host_import_apply(accept: bool) -> Result<FlatpakImportReport, String> {
    let source = host_config_dir_under_flatpak();
    let target = aeroftp_data_root();
    let mut report = FlatpakImportReport {
        imported: false,
        source: source.clone(),
        target: target.clone(),
    };
    if accept {
        match (source.as_ref(), target.as_ref()) {
            (Some(src), Some(dst)) => {
                copy_missing_tree(src, dst).map_err(|e| {
                    format!(
                        "Import host config from {} to {}: {e}",
                        src.display(),
                        dst.display()
                    )
                })?;
                report.imported = true;
            }
            _ => return Err("No host configuration available to import".to_string()),
        }
    }
    write_flatpak_import_marker();
    Ok(report)
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

    // ---- Flatpak host-config import (B3) ----

    /// Outside a Flatpak sandbox there is nothing to import, no matter what the
    /// host looks like. This is the guard that keeps native, portable, Snap and
    /// AppImage installs untouched.
    #[test]
    fn host_config_absent_when_not_flatpak() {
        let got = host_config_dir_impl(
            false,
            Some(PathBuf::from("/home/user")),
            "aeroftp",
            Some(PathBuf::from("/whatever")),
            |_| true,
        );
        assert!(got.is_none());
    }

    /// Under Flatpak with a real host config present, resolve `$HOME/.config/<leaf>`.
    #[test]
    fn host_config_resolved_under_flatpak() {
        let home = PathBuf::from("/home/user");
        let got = host_config_dir_impl(
            true,
            Some(home.clone()),
            "aeroftp",
            Some(PathBuf::from(
                "/home/user/.var/app/com.aeroftp.AeroFTP/config/aeroftp",
            )),
            |p| p == home.join(".config").join("aeroftp"),
        );
        assert_eq!(got, Some(home.join(".config").join("aeroftp")));
    }

    /// A host config that does not exist on disk is not offered.
    #[test]
    fn host_config_skipped_when_dir_missing() {
        let got = host_config_dir_impl(
            true,
            Some(PathBuf::from("/home/user")),
            "aeroftp",
            Some(PathBuf::from("/sandbox/aeroftp")),
            |_| false,
        );
        assert!(got.is_none());
    }

    /// If the resolved host path equals the current data root, importing would be
    /// a self-referential no-op, so it must be refused (guards a misconfigured or
    /// non-redirected environment from copying a tree onto itself).
    #[test]
    fn host_config_skipped_when_equal_to_data_root() {
        let home = PathBuf::from("/home/user");
        let same = home.join(".config").join("aeroftp");
        let got = host_config_dir_impl(true, Some(home), "aeroftp", Some(same), |_| true);
        assert!(got.is_none());
    }

    /// The import copies only what is absent and never overwrites an existing
    /// file in the sandbox, so a partially set-up install is preserved.
    #[test]
    fn copy_missing_tree_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("vault.bin"), b"HOST-VAULT").unwrap();
        std::fs::write(src.join("sub/servers.json"), b"HOST-SERVERS").unwrap();
        // A file the sandbox already has must NOT be clobbered.
        std::fs::write(dst.join("vault.bin"), b"SANDBOX-VAULT").unwrap();

        copy_missing_tree(&src, &dst).unwrap();

        // Existing file preserved, missing file copied over.
        assert_eq!(
            std::fs::read(dst.join("vault.bin")).unwrap(),
            b"SANDBOX-VAULT"
        );
        assert_eq!(
            std::fs::read(dst.join("sub/servers.json")).unwrap(),
            b"HOST-SERVERS"
        );
    }

    /// Recursively check whether any regular file under `dir` contains `needle`.
    /// Uses `symlink_metadata` so the walk itself never chases a link into the
    /// tree it is verifying against.
    #[cfg(unix)]
    fn tree_contains_bytes(dir: &Path, needle: &[u8]) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = path.symlink_metadata() else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                if tree_contains_bytes(&path, needle) {
                    return true;
                }
            } else if let Ok(bytes) = std::fs::read(&path) {
                if bytes.windows(needle.len()).any(|w| w == needle) {
                    return true;
                }
            }
        }
        false
    }

    /// A symlink inside the consented tree can point outside it (foreign secrets
    /// like ~/.ssh) or at an ancestor (a cycle). The import must never follow one:
    /// real files are still copied, but no symlink is materialised and no target
    /// content ever lands under the destination.
    #[cfg(unix)]
    #[test]
    fn copy_missing_tree_skips_symlinks() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = root.join("src");
        let dst = root.join("dst");

        // Foreign content that lives OUTSIDE the consented tree. A followed
        // symlink would drag it into the sandbox; a correct import must not.
        let secret = b"OUTSIDE-SECRET-MUST-NOT-BE-COPIED";
        let outside_dir = root.join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("secret.txt"), secret).unwrap();
        let outside_file = root.join("outside-file.txt");
        std::fs::write(&outside_file, secret).unwrap();

        std::fs::create_dir_all(&src).unwrap();
        // (a) a real file we DO want copied.
        std::fs::write(src.join("real.txt"), b"REAL").unwrap();
        // (b) a symlink to a file outside the tree.
        symlink(&outside_file, src.join("link-to-file")).unwrap();
        // (c) a symlink to a directory outside the tree holding a secret.
        symlink(&outside_dir, src.join("link-to-dir")).unwrap();
        // (d) a self-referencing directory symlink: following it would recurse
        //     until path-length exhaustion.
        symlink(&src, src.join("loop")).unwrap();

        copy_missing_tree(&src, &dst).unwrap();

        // The real file is copied.
        assert_eq!(std::fs::read(dst.join("real.txt")).unwrap(), b"REAL");
        // The symlinks were skipped, not materialised in the destination.
        assert!(!dst.join("link-to-file").exists());
        assert!(!dst.join("link-to-dir").exists());
        assert!(!dst.join("loop").exists());
        // The foreign secret appears nowhere under the destination.
        assert!(!tree_contains_bytes(&dst, secret));
    }
}
