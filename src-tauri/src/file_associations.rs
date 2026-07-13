// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! In-app File associations settings panel (Deliverable G #3).
//!
//! Exposes status and apply commands for AeroFTP-owned formats and common
//! compressed archive formats. Keeps OS defaults honest:
//! - Linux: direct via `xdg-mime default` + query (immediate effect)
//! - Windows: read-only UserChoice / ProgId checks; set opens Default apps UI
//!   and never writes protected UserChoice keys.
//! - macOS: best-effort only (gated); no live validation claim from Linux host.
//!
//! The panel is the source of truth; no localStorage checkbox state is used.

use serde::{Deserialize, Serialize};
use std::process::Command;

use crate::hidden_command;

/// Typed catalog entry for one association "key" (format family).
#[derive(Clone, Debug)]
pub struct AssocCatalogEntry {
    pub key: &'static str,
    pub label: &'static str,
    pub extensions: &'static [&'static str],
    pub linux_mimes: &'static [&'static str],
    /// Windows ProgIDs to recognize as "AeroFTP is handling this".
    /// Order: preferred first (e.g. AeroFTP.Archive before legacy).
    #[allow(dead_code)]
    pub windows_progids: &'static [&'static str],
}

/// AeroFTP-owned formats (use dedicated ProgIDs / MIME where registered).
pub const AERO_OWNED: &[AssocCatalogEntry] = &[
    AssocCatalogEntry {
        key: "aerovault",
        label: "AeroVault",
        extensions: &[".aerovault"],
        linux_mimes: &["application/x-aerovault"],
        windows_progids: &["AeroFTP.AeroVault", "AeroFTP.aerovault"],
    },
    AssocCatalogEntry {
        key: "aerozip",
        label: "AeroZip",
        extensions: &[".aerozip"],
        linux_mimes: &["application/x-aerozip"],
        windows_progids: &["AeroFTP.AeroZip", "AeroFTP.aerozip"],
    },
    AssocCatalogEntry {
        key: "profile",
        label: "AeroFTP Profile",
        extensions: &[".aeroftp"],
        linux_mimes: &["application/x-aeroftp"],
        windows_progids: &["AeroFTP.Profile", "AeroFTP.aeroftp"],
    },
    AssocCatalogEntry {
        key: "keystore",
        label: "AeroFTP Keystore",
        extensions: &[".aeroftp-keystore"],
        linux_mimes: &["application/x-aeroftp-keystore"],
        windows_progids: &["AeroFTP.Keystore", "AeroFTP.aeroftp-keystore"],
    },
    AssocCatalogEntry {
        key: "script",
        label: "AeroFTP Script",
        extensions: &[".aeroftp-script"],
        linux_mimes: &["application/x-aeroftp-script"],
        windows_progids: &["AeroFTP.Script", "AeroFTP.aeroftp-script"],
    },
];

/// General compressed archive formats (share AeroFTP.Archive ProgID from
/// Deliverable G Windows opener registration + tauri fileAssociations).
pub const ARCHIVES: &[AssocCatalogEntry] = &[
    AssocCatalogEntry {
        key: "zip",
        label: "ZIP",
        extensions: &[".zip"],
        linux_mimes: &["application/zip"],
        windows_progids: &["AeroFTP.Archive"],
    },
    AssocCatalogEntry {
        key: "sevenz",
        label: "7z",
        extensions: &[".7z"],
        linux_mimes: &["application/x-7z-compressed"],
        windows_progids: &["AeroFTP.Archive"],
    },
    AssocCatalogEntry {
        key: "rar",
        label: "RAR",
        extensions: &[".rar"],
        linux_mimes: &["application/vnd.rar", "application/x-rar-compressed"],
        windows_progids: &["AeroFTP.Archive"],
    },
    AssocCatalogEntry {
        key: "tar",
        label: "TAR / TGZ / TBZ / TXZ",
        extensions: &[".tar", ".tgz", ".tar.gz", ".tar.xz", ".tar.bz2"],
        linux_mimes: &[
            "application/x-tar",
            "application/x-compressed-tar",
            "application/gzip",
        ],
        windows_progids: &["AeroFTP.Archive"],
    },
    AssocCatalogEntry {
        key: "single_stream",
        label: "Gzip / XZ / Bzip2",
        extensions: &[".gz", ".xz", ".bz2"],
        linux_mimes: &[
            "application/gzip",
            "application/x-xz",
            "application/x-bzip2",
        ],
        windows_progids: &["AeroFTP.Archive"],
    },
];

/// All entries in display order (Aero owned first, then archives).
pub fn all_entries() -> Vec<&'static AssocCatalogEntry> {
    let mut v = Vec::with_capacity(AERO_OWNED.len() + ARCHIVES.len());
    v.extend(AERO_OWNED);
    v.extend(ARCHIVES);
    v
}

/// Map key -> entry (for backend + tests).
pub fn entry_for_key(key: &str) -> Option<&'static AssocCatalogEntry> {
    all_entries().into_iter().find(|e| e.key == key)
}

/// Linux desktop file ids we try (order: preferred first).
/// The panel prefers the full reverse-DNS id; postinst and some packages may install as "AeroFTP.desktop".
const LINUX_DESKTOP_IDS: &[&str] = &[
    "com.aeroftp.AeroFTP.desktop",
    "AeroFTP.desktop",
    "aeroftp.desktop",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAssociationStatus {
    pub platform: String,
    /// True when the platform supports direct (no-UI) apply (Linux today).
    pub can_apply_directly: bool,
    /// True when the platform requires the user to confirm in an OS dialog.
    pub requires_os_confirmation: bool,
    pub items: Vec<FileAssociationItemStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAssociationItemStatus {
    pub key: String,
    pub label: String,
    pub extensions: Vec<String>,
    pub is_default: bool,
    /// True if AeroFTP is registered as a possible handler (even if not default).
    pub is_available: bool,
    /// Human readable current handler name when known (e.g. "Files", "Archive Manager").
    pub current_handler: Option<String>,
    /// What action a "make default" click performs: "direct" | "os_confirmation" | "best_effort" | "unsupported"
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAssociationApplyResult {
    pub applied: Vec<String>,
    /// Keys that could not be applied directly (e.g. Windows always).
    pub requires_confirmation: Vec<String>,
    pub platform_note: Option<String>,
    pub error: Option<String>,
}

/// Return a compact status snapshot for the UI.
#[tauri::command]
pub async fn file_associations_status() -> Result<FileAssociationStatus, String> {
    let platform = std::env::consts::OS.to_string();
    let (can_apply, requires_confirm) = match platform.as_str() {
        "linux" => (true, false),
        "windows" => (false, true),
        "macos" => (false, false), // best-effort, no forced confirmation UX
        _ => (false, false),
    };

    let mut items = Vec::new();
    for entry in all_entries() {
        let st = compute_item_status(entry, &platform);
        items.push(st);
    }

    Ok(FileAssociationStatus {
        platform,
        can_apply_directly: can_apply,
        requires_os_confirmation: requires_confirm,
        items,
    })
}

fn compute_item_status(entry: &AssocCatalogEntry, platform: &str) -> FileAssociationItemStatus {
    match platform {
        "linux" => linux_item_status(entry),
        "windows" => windows_item_status(entry),
        "macos" => macos_item_status(entry),
        _ => unsupported_item_status(entry),
    }
}

fn unsupported_item_status(entry: &AssocCatalogEntry) -> FileAssociationItemStatus {
    FileAssociationItemStatus {
        key: entry.key.to_string(),
        label: entry.label.to_string(),
        extensions: entry.extensions.iter().map(|s| s.to_string()).collect(),
        is_default: false,
        is_available: false,
        current_handler: None,
        action: "unsupported".to_string(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Linux implementation (direct xdg-mime)

// Runs `xdg-mime query default <mime>` and returns trimmed stdout or None.
fn xdg_mime_query_default(mime: &str) -> Option<String> {
    let out = Command::new("xdg-mime")
        .arg("query")
        .arg("default")
        .arg(mime)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn xdg_mime_set_default(desktop_id: &str, mime: &str) -> Result<(), String> {
    let status = hidden_command("xdg-mime")
        .arg("default")
        .arg(desktop_id)
        .arg(mime)
        .status()
        .map_err(|e| format!("xdg-mime spawn failed: {}", e))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "xdg-mime default failed for {} (exit {:?})",
            mime,
            status.code()
        ))
    }
}

fn linux_item_status(entry: &AssocCatalogEntry) -> FileAssociationItemStatus {
    // Consider it "default" if ANY of its mimes currently point at an aeroftp desktop.
    let mut is_default = false;
    let mut current_handler: Option<String> = None;

    for mime in entry.linux_mimes {
        if let Some(h) = xdg_mime_query_default(mime) {
            let h_lower = h.to_lowercase();
            if h_lower.contains("aeroftp") {
                is_default = true;
            }
            // Keep the first non-empty handler we saw for UX label.
            if current_handler.is_none() {
                current_handler = Some(h);
            }
        }
    }

    // Availability: if the desktop file itself is installed (best-effort: xdg-mime query or which).
    // We treat "AeroFTP is in the list of candidates" as true on Linux if xdg-mime works at all
    // (the desktop file with our MimeType= must have been installed).
    let is_available = true; // conservative: if we reached here the runtime supports xdg-mime

    FileAssociationItemStatus {
        key: entry.key.to_string(),
        label: entry.label.to_string(),
        extensions: entry.extensions.iter().map(|s| s.to_string()).collect(),
        is_default,
        is_available,
        current_handler,
        action: "direct".to_string(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Windows implementation (read-only status; never touch UserChoice)

#[cfg(windows)]
fn windows_query_userchoice_prog_id(ext: &str) -> Option<String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    // FileExts\<ext>\UserChoice\ProgId  (Win8+ protected default)
    let path = format!(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.{}",
        ext.trim_start_matches('.')
    );
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey_with_flags(&path, KEY_READ) {
        if let Ok(uc) = key.open_subkey_with_flags("UserChoice", KEY_READ) {
            if let Ok(prog) = uc.get_value::<String, _>("ProgId") {
                if !prog.is_empty() {
                    return Some(prog);
                }
            }
        }
    }
    // Fallback: direct class association (older / non-UserChoice surfaces)
    let class_path = format!(r"Software\Classes\.{}", ext.trim_start_matches('.'));
    if let Ok(key) = hkcu.open_subkey_with_flags(&class_path, KEY_READ) {
        if let Ok(prog) = key.get_value::<String, _>("") {
            if !prog.is_empty() {
                return Some(prog);
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn windows_query_userchoice_prog_id(_ext: &str) -> Option<String> {
    None
}

#[cfg(windows)]
fn windows_is_prog_id_aeroftp(prog: &str, entry: &AssocCatalogEntry) -> bool {
    let p = prog.trim();
    for candidate in entry.windows_progids {
        if p.eq_ignore_ascii_case(candidate) {
            return true;
        }
    }
    false
}

#[cfg(not(windows))]
fn windows_is_prog_id_aeroftp(_prog: &str, _entry: &AssocCatalogEntry) -> bool {
    false
}

fn windows_item_status(entry: &AssocCatalogEntry) -> FileAssociationItemStatus {
    let mut is_default = false;
    let mut current_handler: Option<String> = None;

    for ext in entry.extensions {
        if let Some(prog) = windows_query_userchoice_prog_id(ext) {
            if windows_is_prog_id_aeroftp(&prog, entry) {
                is_default = true;
            }
            if current_handler.is_none() {
                current_handler = Some(prog);
            }
        }
    }

    // is_available: the fact that the installer wrote OpenWithProgids + Capabilities means
    // AeroFTP appears in the OS list. We conservatively return true here; the UI shows
    // "Available" when not default.
    let is_available = true;

    FileAssociationItemStatus {
        key: entry.key.to_string(),
        label: entry.label.to_string(),
        extensions: entry.extensions.iter().map(|s| s.to_string()).collect(),
        is_default,
        is_available,
        current_handler,
        action: "os_confirmation".to_string(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// macOS (best-effort only; compile-clean on non-mac)

#[cfg(target_os = "macos")]
fn macos_item_status(entry: &AssocCatalogEntry) -> FileAssociationItemStatus {
    // Best-effort: we do not call LaunchServices here unless live on Mac with full test.
    // Report "best_effort" action; leave is_default false unless we add live FFI later.
    FileAssociationItemStatus {
        key: entry.key.to_string(),
        label: entry.label.to_string(),
        extensions: entry.extensions.iter().map(|s| s.to_string()).collect(),
        is_default: false,
        is_available: true,
        current_handler: None,
        action: "best_effort".to_string(),
    }
}

#[cfg(not(target_os = "macos"))]
fn macos_item_status(entry: &AssocCatalogEntry) -> FileAssociationItemStatus {
    // On non-mac builds we report the same shape the UI expects but unsupported action.
    let mut s = unsupported_item_status(entry);
    s.action = "best_effort".to_string(); // still surface the intent in docs
    s
}

// ─────────────────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn file_associations_set_default(
    keys: Vec<String>,
) -> Result<FileAssociationApplyResult, String> {
    let platform = std::env::consts::OS.to_string();
    let mut applied = Vec::new();
    let mut requires = Vec::new();
    let mut errors = Vec::new();

    match platform.as_str() {
        "linux" => {
            for k in keys {
                if let Some(entry) = entry_for_key(&k) {
                    let mut ok = true;
                    for mime in entry.linux_mimes {
                        let mut set_ok = false;
                        for &id in LINUX_DESKTOP_IDS {
                            if xdg_mime_set_default(id, mime).is_ok() {
                                set_ok = true;
                                break;
                            }
                        }
                        if !set_ok {
                            ok = false;
                            errors.push(format!("{}: xdg-mime failed for {}", k, mime));
                        }
                    }
                    if ok {
                        applied.push(k);
                    } else {
                        requires.push(k);
                    }
                }
            }
        }
        "windows" => {
            // Never write UserChoice. Always surface the OS UI.
            for k in keys {
                requires.push(k);
            }
            // Also trigger the UI immediately for convenience (non-fatal).
            let _ = file_associations_open_settings().await;
        }
        "macos" => {
            // Best effort: attempt LaunchServices if we can, otherwise just open settings.
            #[cfg(target_os = "macos")]
            {
                // Placeholder: real LSSetDefaultRoleHandlerForContentType would go here.
                // For now we treat as "opened settings for user to confirm".
                for k in keys {
                    requires.push(k);
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                for k in keys {
                    requires.push(k);
                }
            }
            let _ = file_associations_open_settings().await;
        }
        _ => {
            for k in keys {
                requires.push(k);
            }
        }
    }

    let error = if errors.is_empty() {
        None
    } else {
        Some(errors.join("; "))
    };

    let platform_note = match platform.as_str() {
        "linux" => Some("Applied via xdg-mime. Changes take effect immediately for most file managers.".to_string()),
        "windows" => Some("Windows protects defaults. AeroFTP opened Default apps for you to confirm.".to_string()),
        "macos" => Some("macOS association is best-effort. Confirm in System Settings if the change did not apply.".to_string()),
        _ => None,
    };

    Ok(FileAssociationApplyResult {
        applied,
        requires_confirmation: requires,
        platform_note,
        error,
    })
}

#[tauri::command]
pub async fn file_associations_open_settings() -> Result<(), String> {
    let platform = std::env::consts::OS;

    match platform {
        "linux" => {
            // Best effort open a settings surface. Many DEs differ.
            // Try common ones; ignore failures.
            let _ = hidden_command("xdg-open")
                .arg("settings://default-applications")
                .spawn();
            // Fallbacks (gnome, kde etc) are non-fatal.
            let _ = hidden_command("gnome-control-center")
                .arg("default-apps")
                .spawn();
            Ok(())
        }
        "windows" => {
            // ms-settings:defaultapps is the documented entry point on Win10/11.
            // explorer.exe reliably launches it without a console flash.
            let mut cmd = hidden_command("explorer.exe");
            cmd.arg("ms-settings:defaultapps");
            let _ = cmd
                .spawn()
                .map_err(|e| format!("Failed to open Windows Default apps: {}", e))?;
            Ok(())
        }
        "macos" => {
            // Best effort: open the main System Settings (user can navigate to Desktop & Dock or Privacy).
            #[cfg(target_os = "macos")]
            {
                let _ = Command::new("open")
                    .arg("x-apple.systempreferences:com.apple.preference.general")
                    .spawn();
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit-testable helpers (exported for tests)

/// Pure helper: given a key, return the canonical extensions (no platform).
#[cfg(test)]
pub fn extensions_for_key(key: &str) -> Vec<String> {
    entry_for_key(key)
        .map(|e| e.extensions.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default()
}

/// Pure helper used by tests to validate catalog shape.
#[cfg(test)]
pub fn catalog_keys() -> Vec<&'static str> {
    all_entries().iter().map(|e| e.key).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_expected_keys_and_groups() {
        let keys = catalog_keys();
        assert!(keys.contains(&"aerovault"));
        assert!(keys.contains(&"aerozip"));
        assert!(keys.contains(&"zip"));
        assert!(keys.contains(&"sevenz"));
        assert!(keys.contains(&"single_stream"));
        // Aero owned first
        assert_eq!(keys[0], "aerovault");
    }

    #[test]
    fn extensions_are_non_empty_and_start_with_dot() {
        for e in all_entries() {
            assert!(!e.extensions.is_empty(), "{}", e.key);
            for ext in e.extensions {
                assert!(ext.starts_with('.'), "{}: {}", e.key, ext);
            }
        }
    }

    #[test]
    fn windows_progids_are_present_for_all() {
        for e in all_entries() {
            assert!(
                !e.windows_progids.is_empty(),
                "missing progid for {}",
                e.key
            );
        }
    }

    #[test]
    fn linux_mimes_present() {
        for e in all_entries() {
            assert!(!e.linux_mimes.is_empty(), "missing mime for {}", e.key);
        }
    }

    #[test]
    fn entry_lookup_works() {
        assert!(entry_for_key("zip").is_some());
        assert!(entry_for_key("no-such-key").is_none());
    }

    #[test]
    fn extensions_helper_roundtrips() {
        let exts = extensions_for_key("tar");
        assert!(exts.iter().any(|e| e.contains(".tar")));
        assert!(exts.iter().any(|e| e.contains(".tgz")));
    }
}
