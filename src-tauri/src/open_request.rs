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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
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
    #[default]
    Local,
    /// A remote server connection requested via URL (sftp://, scp://, ftp://, etc.).
    RemoteUrl,
    /// A saved profile requested by name via `--profile <NAME>`.
    Profile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    pub kind: OpenKind,
    /// Absolute path of the file or folder (or synthetic URL/profile URI).
    pub path: String,
    /// The folder AeroFile shows: the parent of a file, a folder itself.
    pub folder: String,
    /// The entry to select in `folder`; empty for a folder.
    pub name: String,
    pub is_dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
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

/// Schemes recognized as remote server connection requests.
const REMOTE_SCHEMES: &[&str] = &[
    "sftp://",
    "scp://",
    "ftp://",
    "ftps://",
    "ftpes://",
    "webdav://",
    "webdavs://",
    "ssh://",
];

/// Whether `raw` starts with a supported remote URL scheme.
pub fn is_remote_url(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    REMOTE_SCHEMES
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// Redact credentials in a URL so it is safe to log or serialize.
pub fn redact_url_for_log(raw: &str) -> String {
    if let Ok(mut u) = url::Url::parse(raw) {
        if u.password().is_some() {
            let _ = u.set_password(Some("***"));
        }
        u.to_string()
    } else {
        if let Some(scheme_end) = raw.find("://") {
            let after_scheme = &raw[scheme_end + 3..];
            if let Some(at_pos) = after_scheme.find('@') {
                let user_info = &after_scheme[..at_pos];
                if let Some(colon_pos) = user_info.find(':') {
                    let user = &user_info[..colon_pos];
                    let rest = &after_scheme[at_pos..];
                    return format!("{}{user}:***{rest}", &raw[..scheme_end + 3]);
                }
            }
        }
        raw.to_string()
    }
}

/// Look up a password in environment variables for the given protocol.
fn resolve_env_password(protocol: &str) -> Option<String> {
    let specific_var = match protocol {
        "sftp" | "scp" => "SFTP_PASSWORD",
        "ftp" | "ftps" => "FTP_PASSWORD",
        "webdav" => "WEBDAV_PASSWORD",
        _ => "",
    };
    if !specific_var.is_empty() {
        if let Ok(val) = std::env::var(specific_var) {
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    if let Ok(val) = std::env::var("AEROFTP_PASSWORD") {
        if !val.is_empty() {
            return Some(val);
        }
    }
    None
}

/// Parse a remote connection URL into an OpenRequest.
pub fn parse_remote_url(raw: &str) -> Option<OpenRequest> {
    if !is_remote_url(raw) {
        return None;
    }
    let u = url::Url::parse(raw).ok()?;
    let scheme = u.scheme().to_ascii_lowercase();
    let protocol = match scheme.as_str() {
        "sftp" | "ssh" => "sftp",
        "scp" => "scp",
        "ftp" => "ftp",
        "ftps" | "ftpes" => "ftps",
        "webdav" | "webdavs" => "webdav",
        _ => return None,
    };
    let host = u.host_str()?;
    if host.is_empty() {
        return None;
    }
    let server = host.to_string();
    let port = u.port().unwrap_or(match scheme.as_str() {
        "sftp" | "scp" | "ssh" => 22,
        "ftp" => 21,
        "ftps" | "ftpes" => 990,
        "webdavs" => 443,
        "webdav" => 80,
        _ => 22,
    });

    let raw_user = u.username();
    let user_decoded = urlencoding::decode(raw_user)
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| raw_user.to_string());
    let username = if !user_decoded.is_empty() {
        Some(user_decoded)
    } else if protocol == "ftp" || protocol == "ftps" {
        Some("anonymous".to_string())
    } else {
        None
    };

    let password = if let Some(raw_pass) = u.password() {
        let pass_decoded = urlencoding::decode(raw_pass)
            .map(|s| s.into_owned())
            .unwrap_or_else(|_| raw_pass.to_string());
        if !pass_decoded.is_empty() {
            Some(pass_decoded)
        } else {
            resolve_env_password(protocol)
        }
    } else {
        resolve_env_password(protocol)
    };

    let raw_path = u.path();
    let decoded_path = urlencoding::decode(raw_path)
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| raw_path.to_string());

    let (initial_path, folder, name, is_dir) = if decoded_path.is_empty() || decoded_path == "/" {
        (Some("/".to_string()), "/".to_string(), String::new(), true)
    } else if decoded_path.ends_with('/') {
        (
            Some(decoded_path.clone()),
            decoded_path,
            String::new(),
            true,
        )
    } else {
        let path_buf = Path::new(&decoded_path);
        let file_name = path_buf
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let parent = path_buf
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "/".to_string());
        (Some(parent.clone()), parent, file_name, false)
    };

    Some(OpenRequest {
        kind: OpenKind::RemoteUrl,
        path: redact_url_for_log(raw),
        folder,
        name,
        is_dir,
        protocol: Some(protocol.to_string()),
        server: Some(server),
        port: Some(port),
        username,
        password,
        initial_path,
        profile_name: None,
    })
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
    if raw.is_empty() {
        return None;
    }
    if is_remote_url(raw) {
        return parse_remote_url(raw);
    }
    if raw.starts_with("--") {
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
            ..Default::default()
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
        ..Default::default()
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
        if arg == "--profile" {
            if let Some(name) = args.next() {
                if !name.is_empty() {
                    requests.push(OpenRequest {
                        kind: OpenKind::Profile,
                        path: format!("profile://{name}"),
                        folder: String::new(),
                        name: name.to_string(),
                        is_dir: false,
                        profile_name: Some(name.to_string()),
                        ..Default::default()
                    });
                }
            }
            continue;
        } else if let Some(name) = arg.strip_prefix("--profile=") {
            if !name.is_empty() {
                requests.push(OpenRequest {
                    kind: OpenKind::Profile,
                    path: format!("profile://{name}"),
                    folder: String::new(),
                    name: name.to_string(),
                    is_dir: false,
                    profile_name: Some(name.to_string()),
                    ..Default::default()
                });
            }
            continue;
        }
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
        let shown = match request.kind {
            OpenKind::RemoteUrl => redact_url_for_log(&request.path),
            OpenKind::Profile => request
                .profile_name
                .as_deref()
                .unwrap_or(&request.name)
                .to_string(),
            _ => Path::new(&request.path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default(),
        };
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
            "gopher://host/path",
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
        assert_eq!(classify("gopher://host/a.zip", None), None);
        assert_eq!(
            classify("sftp://host/a.zip", None).unwrap().kind,
            OpenKind::RemoteUrl
        );
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
            ..Default::default()
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
            ..Default::default()
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

    #[test]
    fn remote_url_with_credentials_and_port() {
        let req = parse_remote_url("sftp://bob:secret@example.com:2222/var/www/file.txt").unwrap();
        assert_eq!(req.kind, OpenKind::RemoteUrl);
        assert_eq!(req.protocol.as_deref(), Some("sftp"));
        assert_eq!(req.server.as_deref(), Some("example.com"));
        assert_eq!(req.port, Some(2222));
        assert_eq!(req.username.as_deref(), Some("bob"));
        assert_eq!(req.password.as_deref(), Some("secret"));
        assert_eq!(req.initial_path.as_deref(), Some("/var/www"));
        assert_eq!(req.folder, "/var/www");
        assert_eq!(req.name, "file.txt");
        assert!(!req.is_dir);
        // Ensure path is redacted
        assert_eq!(req.path, "sftp://bob:***@example.com:2222/var/www/file.txt");
    }

    #[test]
    fn remote_url_trailing_slash_folder() {
        let req = parse_remote_url("sftp://bob:secret@example.com:2222/var/www/").unwrap();
        assert_eq!(req.kind, OpenKind::RemoteUrl);
        assert_eq!(req.initial_path.as_deref(), Some("/var/www/"));
        assert_eq!(req.folder, "/var/www/");
        assert_eq!(req.name, "");
        assert!(req.is_dir);
    }

    #[test]
    fn remote_url_root_path_defaulting() {
        let req = parse_remote_url("ftp://ftp.example.com").unwrap();
        assert_eq!(req.protocol.as_deref(), Some("ftp"));
        assert_eq!(req.server.as_deref(), Some("ftp.example.com"));
        assert_eq!(req.port, Some(21));
        assert_eq!(req.username.as_deref(), Some("anonymous"));
        assert_eq!(req.initial_path.as_deref(), Some("/"));
        assert_eq!(req.folder, "/");
        assert_eq!(req.name, "");
        assert!(req.is_dir);
    }

    #[test]
    fn remote_url_schemes_and_default_ports() {
        let cases = [
            ("sftp://example.com/", "sftp", 22),
            ("scp://example.com/", "scp", 22),
            ("ssh://example.com/", "sftp", 22),
            ("ftp://example.com/", "ftp", 21),
            ("ftps://example.com/", "ftps", 990),
            ("ftpes://example.com/", "ftps", 990),
            ("webdav://example.com/", "webdav", 80),
            ("webdavs://example.com/", "webdav", 443),
        ];
        for (raw, expected_proto, expected_port) in cases {
            let req = parse_remote_url(raw).unwrap();
            assert_eq!(req.protocol.as_deref(), Some(expected_proto), "{raw}");
            assert_eq!(req.port, Some(expected_port), "{raw}");
        }
    }

    #[test]
    fn remote_url_percent_encoding_decoding() {
        let req = parse_remote_url(
            "sftp://user%40work.com:p%40ss%23123@host:22/my%20folder/file%201.txt",
        )
        .unwrap();
        assert_eq!(req.username.as_deref(), Some("user@work.com"));
        assert_eq!(req.password.as_deref(), Some("p@ss#123"));
        assert_eq!(req.folder, "/my folder");
        assert_eq!(req.name, "file 1.txt");
        assert_eq!(req.initial_path.as_deref(), Some("/my folder"));
    }

    #[test]
    fn remote_url_env_var_password_fallback() {
        unsafe {
            std::env::set_var("SFTP_PASSWORD", "env_sftp_secret");
        }
        let req = parse_remote_url("sftp://alice@host/").unwrap();
        assert_eq!(req.password.as_deref(), Some("env_sftp_secret"));
        unsafe {
            std::env::remove_var("SFTP_PASSWORD");
            std::env::set_var("AEROFTP_PASSWORD", "env_generic_secret");
        }
        let req2 = parse_remote_url("sftp://alice@host/").unwrap();
        assert_eq!(req2.password.as_deref(), Some("env_generic_secret"));
        unsafe {
            std::env::remove_var("AEROFTP_PASSWORD");
        }
    }

    #[test]
    fn profile_flag_parsing() {
        let argv1 = ["aeroftp", "--profile", "Production NAS"];
        let reqs1 = from_argv(&argv1, None);
        assert_eq!(reqs1.len(), 1);
        assert_eq!(reqs1[0].kind, OpenKind::Profile);
        assert_eq!(reqs1[0].profile_name.as_deref(), Some("Production NAS"));
        assert_eq!(reqs1[0].name, "Production NAS");
        assert_eq!(reqs1[0].path, "profile://Production NAS");

        let argv2 = ["aeroftp", "--profile=Home Lab"];
        let reqs2 = from_argv(&argv2, None);
        assert_eq!(reqs2.len(), 1);
        assert_eq!(reqs2[0].kind, OpenKind::Profile);
        assert_eq!(reqs2[0].profile_name.as_deref(), Some("Home Lab"));

        // Empty profile name should be ignored
        let argv3 = ["aeroftp", "--profile", "", "--profile="];
        let reqs3 = from_argv(&argv3, None);
        assert!(reqs3.is_empty());
    }

    #[test]
    fn interleaved_profile_and_remote_url() {
        let argv = ["aeroftp", "--profile", "Dev Box", "sftp://bob@host/data/"];
        let reqs = from_argv(&argv, None);
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].kind, OpenKind::Profile);
        assert_eq!(reqs[0].profile_name.as_deref(), Some("Dev Box"));
        assert_eq!(reqs[1].kind, OpenKind::RemoteUrl);
        assert_eq!(reqs[1].server.as_deref(), Some("host"));
    }

    #[test]
    fn redact_url_for_log_masks_passwords() {
        assert_eq!(
            redact_url_for_log("sftp://admin:VerySecret123@files.corp.internal:2222/var/log"),
            "sftp://admin:***@files.corp.internal:2222/var/log"
        );
        assert_eq!(
            redact_url_for_log("ftp://guest@example.com/pub"),
            "ftp://guest@example.com/pub"
        );
        assert_eq!(redact_url_for_log("not-a-url"), "not-a-url");
    }

    #[test]
    fn malformed_and_unsupported_urls_return_none() {
        assert_eq!(parse_remote_url("sftp://"), None);
        assert_eq!(parse_remote_url("sftp:///path"), None);
        assert_eq!(parse_remote_url("gopher://host/path"), None);
        assert_eq!(parse_remote_url("https://example.com/"), None);
        assert_eq!(classify("gopher://host/path", None), None);
    }
}
