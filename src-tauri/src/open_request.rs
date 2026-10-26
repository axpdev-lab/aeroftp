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

/// A multi-selection opened at once arrives as one argument list; past this
/// the queue only means nobody is draining it, and the newest entries win.
const MAX_PENDING: usize = 256;

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

/// `\\?\C:\x` is how `canonicalize` spells `C:\x` on Windows. The path bar,
/// tabs and breadcrumbs would show the prefix, so it is removed from a drive
/// path and from a share (`\\?\UNC\host\share` becomes `\\host\share`).
/// Every consumer of the result is a Rust command, and the standard library
/// adds the prefix back on its own for a path past `MAX_PATH`, so a long path
/// keeps working. Any other verbatim form (`\\?\Volume{...}\`,
/// `\\?\GLOBALROOT\...`) has no plain spelling and is kept whole.
fn without_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        let is_drive = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\';
        if is_drive {
            return PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}

/// The local path an argument names. The Linux packages launch
/// `/usr/bin/aeroftp %U` (the same entry handles `ftp://` links), so a file
/// manager passes `file:///home/me/a%20b.zip` rather than a path: since the
/// dispatcher layout of May 2026 (366d208e5, `%f` became `%U`) every "Open
/// with" from a deb, rpm or AppImage was dropped here. Other URLs are not
/// files and yield `None`; a Windows drive path (`C:\x`) parses as a URL with
/// the scheme `c`, so only the `file` scheme is read as one.
///
/// A relative path is joined to `cwd`, the folder the launching process ran
/// in: a second instance hands its arguments to the running one, whose own
/// folder may differ (`aeroftp ./photo.png` from a shell).
fn local_path(raw: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    if raw
        .get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"))
    {
        return url::Url::parse(raw).ok()?.to_file_path().ok();
    }
    let path = PathBuf::from(raw);
    Some(match cwd {
        Some(cwd) if path.is_relative() => cwd.join(path),
        _ => path,
    })
}

/// Classify one argument, a relative path read against `cwd` (the current
/// folder when `None`). `None` for a flag, a URL, or anything that is not an
/// existing regular file or folder once symlinks are resolved.
pub fn classify(raw: &str, cwd: Option<&Path>) -> Option<OpenRequest> {
    if raw.is_empty() || raw.starts_with("--") {
        return None;
    }
    let canonical = without_verbatim_prefix(&std::fs::canonicalize(local_path(raw, cwd)?).ok()?);
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

/// The app's own options that take a value in the next argument. The value
/// is the app's, never a file the user chose: the Windows portable updater
/// of 4.2.4 relaunches with `--post-update-cleanup <old exe>`, and that old
/// exe still exists while the new one starts, so read as a path it opened
/// its folder in AeroFile. An extract verb whose target is not a file falls
/// through to here too.
const INTERNAL_OPTIONS_WITH_VALUE: &[&str] =
    &["--post-update-cleanup", "--extract-here", "--extract-to"];

/// Every openable path among the process arguments (`argv[0]` skipped), in
/// order: `%U` passes one argument per file of a multi-selection. The
/// extract verbs are handled before this; the app's own options are skipped
/// with their value.
pub fn from_argv<S: AsRef<str>>(argv: &[S], cwd: Option<&Path>) -> Vec<OpenRequest> {
    let mut requests = Vec::new();
    let mut args = argv.iter().skip(1).map(AsRef::as_ref);
    while let Some(arg) = args.next() {
        if INTERNAL_OPTIONS_WITH_VALUE.contains(&arg) {
            args.next();
            continue;
        }
        requests.extend(classify(arg, cwd));
    }
    requests
}

/// Add one delivery to the queue. The bound trims only what was already
/// waiting: every file of the delivery just made is kept, even past
/// MAX_PENDING (a command line bounds it anyway).
fn enqueue(pending: &mut Vec<OpenRequest>, batch: Vec<OpenRequest>) {
    let keep_waiting = MAX_PENDING.saturating_sub(batch.len());
    let dropped = pending.len().saturating_sub(keep_waiting);
    pending.drain(..dropped);
    pending.extend(batch);
}

/// Queue requests and tell the main window once, which drains them now if
/// it is ready, or at the end of its boot or unlock otherwise.
pub fn deliver(app: &AppHandle, requests: Vec<OpenRequest>) {
    if requests.is_empty() {
        return;
    }
    for request in &requests {
        // The entry name only, a folder's own name for a folder: no full path.
        let shown = Path::new(&request.path)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        log::info!("OS open request: {:?} ({shown})", request.kind);
    }
    enqueue(
        &mut PENDING.lock().unwrap_or_else(|e| e.into_inner()),
        requests,
    );
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.emit(OPEN_REQUEST_EVENT, ());
    }
}

/// Bring the main window forward for a file opened while the app runs,
/// including from the tray: what the file opens must be visible. Window
/// calls go through Tauri's main-thread message path, so this is safe from
/// the single-instance D-Bus thread.
pub fn raise_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
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
        let request = classify(file.to_str().unwrap(), None).unwrap();
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
        let request = classify(dir.path().to_str().unwrap(), None).unwrap();
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
        assert!(from_argv(&argv, None).is_empty());
    }

    /// Audit 4.2.5 C01: after a Windows portable update from 4.2.4 the new
    /// exe starts as `AeroFTP.exe --post-update-cleanup <old exe>`, and the
    /// old exe still exists. Its path is the updater's, not a file to open:
    /// no request for it, cold start or second instance (both read argv
    /// through here), while a file the user passes still opens. The same
    /// for an extract verb whose value fell through.
    #[test]
    fn the_apps_own_options_and_their_values_open_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let old_exe = dir.path().join("AeroFTP.exe.old");
        let user_file = dir.path().join("notes.txt");
        std::fs::write(&old_exe, b"MZ").unwrap();
        std::fs::write(&user_file, b"x").unwrap();
        let old = old_exe.to_string_lossy().to_string();
        let user = user_file.to_string_lossy().to_string();
        for flag in ["--post-update-cleanup", "--extract-here", "--extract-to"] {
            let argv = ["AeroFTP.exe", flag, old.as_str()];
            assert!(from_argv(&argv, None).is_empty(), "{flag}");
            // Relative to the launching folder, as a second instance hands it.
            let argv = ["AeroFTP.exe", flag, "AeroFTP.exe.old"];
            assert!(
                from_argv(&argv, Some(dir.path())).is_empty(),
                "{flag} relative"
            );
            let argv = ["AeroFTP.exe", flag, old.as_str(), user.as_str()];
            let names: Vec<String> = from_argv(&argv, None).into_iter().map(|r| r.name).collect();
            assert_eq!(names, ["notes.txt"], "{flag}");
        }
        // A file before the option still opens.
        let argv = [
            "AeroFTP.exe",
            user.as_str(),
            "--post-update-cleanup",
            old.as_str(),
        ];
        assert_eq!(from_argv(&argv, None).len(), 1);
    }

    #[test]
    fn every_file_of_a_multi_selection_is_kept_in_order() {
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
        let kinds: Vec<_> = from_argv(&argv, None).into_iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [OpenKind::Archive, OpenKind::Local]);
    }

    #[test]
    fn a_relative_path_is_read_against_the_launching_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("photo.png"), b"x").unwrap();
        // The running instance's own folder does not hold the file; the
        // second instance's does.
        let request = classify("sub/photo.png", Some(dir.path())).unwrap();
        assert_eq!(request.name, "photo.png");
        assert_eq!(classify("sub/photo.png", Some(Path::new("/"))), None);
        // An absolute path ignores the launching folder.
        let absolute = dir.path().join("sub").join("photo.png");
        assert_eq!(
            classify(absolute.to_str().unwrap(), Some(Path::new("/"))).unwrap(),
            request
        );
    }

    #[test]
    fn a_file_uri_from_the_desktop_entry_is_read_as_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a b.zip");
        std::fs::write(&file, b"x").unwrap();
        let uri = url::Url::from_file_path(&file).unwrap().to_string();
        assert!(uri.contains("a%20b.zip"), "{uri}");
        let request = classify(&uri, None).unwrap();
        assert_eq!(request.kind, OpenKind::Archive);
        assert_eq!(request.name, "a b.zip");
        // What the 4.2.x packages really pass: argv[0], then the URI.
        assert_eq!(from_argv(&["aeroftp".to_string(), uri], None), [request]);
        assert_eq!(
            local_path("sftp://host/a.zip", None),
            Some(PathBuf::from("sftp://host/a.zip"))
        );
        assert_eq!(classify("sftp://host/a.zip", None), None);
        // A multi-byte start must not split a character while probing.
        assert_eq!(classify("èèèè.png", None), None);
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
        // No plain spelling exists for these: stripping would break them.
        for kept in [
            r"\\?\Volume{0b1e2c3d-0000-0000-0000-100000000000}\a",
            r"\\?\GLOBALROOT\Device\HarddiskVolume2\a",
            r"\\?\C:",
        ] {
            assert_eq!(
                without_verbatim_prefix(Path::new(kept)),
                PathBuf::from(kept)
            );
        }
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
        {
            let mut pending = PENDING.lock().unwrap();
            pending.clear();
            for n in 0..MAX_PENDING + 3 {
                enqueue(&mut pending, vec![entry(n)]);
            }
        }
        let taken = tauri::async_runtime::block_on(take_open_requests());
        assert_eq!(taken.len(), MAX_PENDING);
        assert_eq!(taken[0], entry(3));
        assert!(tauri::async_runtime::block_on(take_open_requests()).is_empty());
    }

    #[test]
    fn the_bound_trims_what_waited_never_the_delivery_just_made() {
        let entry = |n: usize| OpenRequest {
            kind: OpenKind::Local,
            path: format!("/q/{n}"),
            folder: "/q".into(),
            name: n.to_string(),
            is_dir: false,
        };
        // A full queue, then a selection larger than the bound.
        let mut pending: Vec<_> = (0..MAX_PENDING).map(entry).collect();
        let batch: Vec<_> = (1000..1000 + MAX_PENDING + 44).map(entry).collect();
        enqueue(&mut pending, batch.clone());
        assert_eq!(pending, batch);
        // Room left: the newest waiting entries stay ahead of the delivery.
        let mut pending: Vec<_> = (0..10).map(entry).collect();
        let batch: Vec<_> = (1000..1000 + MAX_PENDING - 4).map(entry).collect();
        enqueue(&mut pending, batch.clone());
        assert_eq!(pending.len(), MAX_PENDING);
        assert_eq!(pending[..4], [entry(6), entry(7), entry(8), entry(9)]);
        assert_eq!(pending[4..], batch[..]);
    }
}
