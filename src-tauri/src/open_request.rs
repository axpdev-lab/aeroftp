// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Files handed to the app by the OS: "Open with AeroFTP", a double-click on
//! an associated format, `aeroftp <path>` from a shell, or a macOS open event.
//!
//! Every path is classified once here and parked in a queue the main window
//! drains with `take_open_requests` once it can act on it (vault boot done,
//! app unlocked). The `open-request` event only tells an already running
//! window to drain again. Before this queue, the first launch emitted the
//! path after a fixed 1.5 s sleep: a window still booting, or behind the lock
//! screen, had no listener yet and the file was silently dropped, so an
//! archive opened the browser only when the app was already running.
//!
//! Formats with a dedicated surface keep it (vault panel, keystore import,
//! profiles import, archive browser). Any other file opens in AeroFile on its
//! folder with the file selected, and the window previews it there (#347).

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

/// Window event telling the main window that the queue has new entries.
pub const OPEN_REQUEST_EVENT: &str = "open-request";

/// A long queue only means nobody is draining it; the newest entries win.
const MAX_PENDING: usize = 16;

static PENDING: Mutex<Vec<OpenRequest>> = Mutex::new(Vec::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OpenKind {
    /// `.aerovault` or `.aerozip`: the vault panel.
    Vault,
    /// `.aeroftp-keystore`: Settings, Backup tab, import.
    Keystore,
    /// `.aeroftp`: the server-profiles import.
    Profiles,
    /// A compressed archive the archive browser reads.
    Archive,
    /// Any other file, or a folder: AeroFile.
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    pub kind: OpenKind,
    /// Absolute path of the file or folder.
    pub path: String,
    /// The folder AeroFile shows: the parent of a file, a folder itself.
    pub folder: String,
    /// The entry to select in `folder`; empty for a folder.
    pub name: String,
    pub is_dir: bool,
}

/// The surface for a file name. Matching is case-insensitive, and `.aeroftp`
/// does not match `.aeroftp-keystore` because the whole suffix is compared.
pub fn kind_for_file_name(name: &str) -> OpenKind {
    let lower = name.to_lowercase();
    if lower.ends_with(".aerovault") || lower.ends_with(".aerozip") {
        OpenKind::Vault
    } else if lower.ends_with(".aeroftp-keystore") {
        OpenKind::Keystore
    } else if lower.ends_with(".aeroftp") {
        OpenKind::Profiles
    } else if [
        ".zip", ".7z", ".rar", ".tar", ".tgz", ".tar.gz", ".tar.xz", ".tar.bz2",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
    {
        OpenKind::Archive
    } else {
        OpenKind::Local
    }
}

/// `\\?\C:\x` is how `canonicalize` spells `C:\x` on Windows. It is a valid
/// path for Rust, but the path bar, tabs and breadcrumbs would show the
/// prefix, so it is removed (`\\?\UNC\host\share` becomes `\\host\share`).
fn without_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// The local path an argument names. The Linux packages launch
/// `/usr/bin/aeroftp %U` (the same entry handles `ftp://` links), so a file
/// manager passes `file:///home/me/a%20b.zip` rather than a path: since the
/// dispatcher layout of May 2026 (366d208e5, `%f` became `%U`) every "Open
/// with" from a deb, rpm or AppImage was dropped here. Other URLs are not
/// files and yield `None`; a Windows drive path (`C:\x`) parses as a URL with
/// the scheme `c`, so only the `file` scheme is read as one.
fn local_path(raw: &str) -> Option<PathBuf> {
    if raw
        .get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"))
    {
        return url::Url::parse(raw).ok()?.to_file_path().ok();
    }
    Some(PathBuf::from(raw))
}

/// Classify one argument. `None` for a flag, a URL, or anything that is not
/// an existing regular file or folder once symlinks are resolved.
pub fn classify(raw: &str) -> Option<OpenRequest> {
    if raw.is_empty() || raw.starts_with("--") {
        return None;
    }
    let canonical = without_verbatim_prefix(&std::fs::canonicalize(local_path(raw)?).ok()?);
    let meta = std::fs::metadata(&canonical).ok()?;
    let path = canonical.to_string_lossy().to_string();
    if meta.is_dir() {
        return Some(OpenRequest {
            kind: OpenKind::Local,
            folder: path.clone(),
            path,
            name: String::new(),
            is_dir: true,
        });
    }
    if !meta.is_file() {
        return None;
    }
    let name = canonical.file_name()?.to_string_lossy().to_string();
    let folder = canonical.parent()?.to_string_lossy().to_string();
    Some(OpenRequest {
        kind: kind_for_file_name(&name),
        path,
        folder,
        name,
        is_dir: false,
    })
}

/// The first openable path among the process arguments (`argv[0]` skipped).
/// The extract verbs are handled before this and never reach it.
pub fn from_argv<S: AsRef<str>>(argv: &[S]) -> Option<OpenRequest> {
    argv.iter().skip(1).find_map(|arg| classify(arg.as_ref()))
}

fn push(request: OpenRequest) {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    pending.push(request);
    let overflow = pending.len().saturating_sub(MAX_PENDING);
    pending.drain(..overflow);
}

/// Queue a request and tell the main window, which drains it now if it is
/// ready, or at the end of its boot or unlock otherwise.
pub fn deliver(app: &AppHandle, request: OpenRequest) {
    // The entry name only, a folder's own name for a folder: no full path.
    let shown = Path::new(&request.path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    log::info!("OS open request: {:?} ({shown})", request.kind);
    push(request);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.emit(OPEN_REQUEST_EVENT, ());
    }
}

/// Drain the queue. Called by the main window when it can act on the
/// entries; each entry is returned exactly once.
#[tauri::command]
pub async fn take_open_requests() -> Vec<OpenRequest> {
    std::mem::take(&mut *PENDING.lock().unwrap_or_else(|e| e.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedicated_formats_keep_their_surface_and_the_rest_goes_to_aerofile() {
        assert_eq!(kind_for_file_name("a.aerovault"), OpenKind::Vault);
        assert_eq!(kind_for_file_name("a.AEROZIP"), OpenKind::Vault);
        assert_eq!(kind_for_file_name("a.aeroftp-keystore"), OpenKind::Keystore);
        assert_eq!(kind_for_file_name("a.aeroftp"), OpenKind::Profiles);
        for name in [
            "a.zip",
            "a.7z",
            "a.RAR",
            "a.tar",
            "a.tgz",
            "a.tar.gz",
            "a.tar.xz",
            "a.tar.bz2",
        ] {
            assert_eq!(kind_for_file_name(name), OpenKind::Archive, "{name}");
        }
        // A lone .gz is not a tar the browser can list; AeroFile shows it.
        for name in [
            "photo.png",
            "notes.txt",
            "a.gz",
            "a.aeroftp-script",
            "aeroftp",
        ] {
            assert_eq!(kind_for_file_name(name), OpenKind::Local, "{name}");
        }
    }

    #[test]
    fn a_file_opens_on_its_folder_with_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photo one.png");
        std::fs::write(&file, b"x").unwrap();
        let request = classify(file.to_str().unwrap()).unwrap();
        let folder = without_verbatim_prefix(&std::fs::canonicalize(dir.path()).unwrap());
        assert_eq!(request.kind, OpenKind::Local);
        assert_eq!(request.name, "photo one.png");
        assert_eq!(request.folder, folder.to_string_lossy());
        assert_eq!(Path::new(&request.path), folder.join("photo one.png"));
        assert!(!request.is_dir);
    }

    #[test]
    fn a_folder_opens_on_itself_and_flags_urls_and_missing_paths_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let request = classify(dir.path().to_str().unwrap()).unwrap();
        assert!(request.is_dir);
        assert_eq!(request.folder, request.path);
        assert!(request.name.is_empty());
        let missing = dir.path().join("missing.png");
        let argv = [
            "aeroftp",
            "--autostart",
            "sftp://host/path",
            missing.to_str().unwrap(),
        ];
        assert_eq!(from_argv(&argv), None);
    }

    #[test]
    fn the_first_openable_argument_wins() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        let image = dir.path().join("b.png");
        std::fs::write(&archive, b"x").unwrap();
        std::fs::write(&image, b"x").unwrap();
        let argv = [
            "aeroftp".to_string(),
            "--autostart".to_string(),
            archive.to_string_lossy().to_string(),
            image.to_string_lossy().to_string(),
        ];
        assert_eq!(from_argv(&argv).unwrap().kind, OpenKind::Archive);
    }

    #[test]
    fn a_file_uri_from_the_desktop_entry_is_read_as_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a b.zip");
        std::fs::write(&file, b"x").unwrap();
        let uri = url::Url::from_file_path(&file).unwrap().to_string();
        assert!(uri.contains("a%20b.zip"), "{uri}");
        let request = classify(&uri).unwrap();
        assert_eq!(request.kind, OpenKind::Archive);
        assert_eq!(request.name, "a b.zip");
        // What the 4.2.x packages really pass: argv[0], then the URI.
        assert_eq!(from_argv(&["aeroftp".to_string(), uri]).unwrap(), request);
        assert_eq!(
            local_path("sftp://host/a.zip"),
            Some(PathBuf::from("sftp://host/a.zip"))
        );
        assert_eq!(classify("sftp://host/a.zip"), None);
        // A multi-byte start must not split a character while probing.
        assert_eq!(classify("èèèè.png"), None);
    }

    #[test]
    fn the_verbatim_prefix_is_removed() {
        assert_eq!(
            without_verbatim_prefix(Path::new(r"\\?\C:\Users\a")),
            PathBuf::from(r"C:\Users\a")
        );
        assert_eq!(
            without_verbatim_prefix(Path::new(r"\\?\UNC\host\share\a")),
            PathBuf::from(r"\\host\share\a")
        );
        assert_eq!(
            without_verbatim_prefix(Path::new("/home/a")),
            PathBuf::from("/home/a")
        );
    }

    #[test]
    fn each_entry_is_taken_once_and_the_queue_is_bounded() {
        let entry = |n: usize| OpenRequest {
            kind: OpenKind::Local,
            path: format!("/q/{n}"),
            folder: "/q".into(),
            name: n.to_string(),
            is_dir: false,
        };
        std::mem::take(&mut *PENDING.lock().unwrap());
        for n in 0..MAX_PENDING + 3 {
            push(entry(n));
        }
        let taken = tauri::async_runtime::block_on(take_open_requests());
        assert_eq!(taken.len(), MAX_PENDING);
        assert_eq!(taken[0], entry(3));
        assert!(tauri::async_runtime::block_on(take_open_requests()).is_empty());
    }
}
