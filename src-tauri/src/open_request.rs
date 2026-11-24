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
use std::collections::BTreeMap;
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
    /// The saved-provider preset a launch URL resolves to (an S3 endpoint's
    /// vendor), so the app applies that preset's region and path style.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Connection options a launch URL sets, in the app's spelling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<BTreeMap<String, serde_json::Value>>,
    /// A sign-in provider (OAuth and the like): `server` names the saved
    /// profile or account to open, and no secret is carried.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sign_in: bool,
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

/// Whether `raw` starts with a launch URL scheme (`launch_url::LAUNCH_SCHEMES`,
/// the table the CLI reads too).
pub fn is_remote_url(raw: &str) -> bool {
    crate::launch_url::is_launch_url(raw)
}

/// The connection options a launch URL sets, in the spelling the app's
/// connection params use (`ProviderOptions`): one field, `bucket`, carries
/// the S3 and B2 bucket, the Azure container, the kDrive id and the
/// Cloudinary cloud name, as the connection form stores them.
fn frontend_options(
    target: &crate::launch_url::LaunchTarget,
) -> BTreeMap<String, serde_json::Value> {
    let mut options = BTreeMap::new();
    let mut fields = target.options.clone();
    fields.extend(target.host_options());
    for (key, value) in fields {
        let (name, value) = match key.as_str() {
            "bucket" | "container" | "drive_id" | "cloud_name" => {
                ("bucket", serde_json::Value::String(value))
            }
            "region" => ("region", serde_json::Value::String(value)),
            "endpoint" => ("endpoint", serde_json::Value::String(value)),
            "path_style" => ("pathStyle", serde_json::Value::Bool(value == "true")),
            "tls_mode" => ("tlsMode", serde_json::Value::String(value)),
            "branch" => ("githubBranch", serde_json::Value::String(value)),
            _ => continue,
        };
        options.insert(name.to_string(), value);
    }
    if target.token() == "sftp" {
        options.insert(
            "allowScpFallback".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    // A WebDAV URL that names its transport keeps it: left to the `auto`
    // rule, an `https://` link on a local host or another port than 443
    // could fall back to plain HTTP and send its password in the clear.
    if target.token() == "webdav" {
        let transport = match target.scheme.scheme {
            "https" | "webdavs" => Some("https"),
            "http" => Some("http"),
            _ => None,
        };
        if let Some(transport) = transport {
            options.insert(
                "webdavScheme".to_string(),
                serde_json::Value::String(transport.to_string()),
            );
        }
    }
    options
}

/// The [`secret_destination`](crate::launch_url::launch_secret::secret_destination)
/// of the connection a launch request describes, built through the same
/// `to_provider_config` the connect runs, from the fields the webview passes
/// back unchanged (the port the way it fills an absent one: the protocol's).
fn launch_destination(
    protocol: &str,
    server: &str,
    port: u16,
    username: &str,
    provider_id: Option<&str>,
    options: &BTreeMap<String, serde_json::Value>,
) -> Result<String, String> {
    let text = |key: &str| options.get(key).and_then(serde_json::Value::as_str);
    let params: crate::provider_commands::ProviderConnectionParams =
        serde_json::from_value(serde_json::json!({
            "protocol": protocol,
            "server": server,
            "port": port,
            "username": username,
            "password": "",
            "provider_id": provider_id,
            "bucket": text("bucket"),
            "region": text("region"),
            "endpoint": text("endpoint"),
            "path_style": options.get("pathStyle").and_then(serde_json::Value::as_bool),
        }))
        .map_err(|err| err.to_string())?;
    crate::launch_url::launch_secret::secret_destination(&params.to_provider_config()?)
}

/// The port a launch URL dials: its own, else the protocol's (the manifest),
/// with plain WebDAV on 80 and FTPS of the `ftps` scheme on 990.
fn default_port(target: &crate::launch_url::LaunchTarget) -> u16 {
    match target.scheme.scheme {
        "webdav" | "http" => 80,
        "webdavs" | "https" => 443,
        _ => crate::connection_manifest::connection_spec(target.token())
            .map_or(443, |spec| spec.default_port),
    }
}

/// Read a launch URL into an OpenRequest. A sign-in provider's request names
/// only the account (`server`, possibly empty) and the folder: the app opens
/// it through a saved, authorized profile, never through the URL.
pub fn parse_remote_url(raw: &str) -> Option<OpenRequest> {
    let target = crate::launch_url::parse(raw).ok()?;
    let protocol = target.token();
    let account = target.scheme.auth == crate::launch_url::LaunchAuth::Account;

    let (initial_path, folder, name, is_dir) = split_remote_path(&target.path);
    // A link that names no folder (`sftp://host`, `dropbox://work`) carries
    // none: the app keeps an open tab where it is and a fresh connect opens
    // the profile's start folder. `dropbox://work/` names the root.
    let initial_path = target.path_given.then_some(initial_path);
    if account {
        return Some(OpenRequest {
            kind: OpenKind::RemoteUrl,
            path: crate::launch_url::redact(raw),
            folder,
            name,
            is_dir,
            protocol: Some(protocol.to_string()),
            server: Some(target.host.clone()),
            initial_path,
            sign_in: true,
            ..Default::default()
        });
    }

    let (server, branch) = match protocol {
        "github" => crate::launch_url::github_repository(&target).ok()?,
        "gitlab" => crate::launch_url::gitlab_project(&target).ok()?,
        _ => (target.connection_host(), None),
    };
    let username = target.connection_username();
    let username = if username.is_empty() && matches!(protocol, "ftp" | "ftps") {
        "anonymous".to_string()
    } else {
        username
    };
    let mut options = frontend_options(&target);
    let mut pending_secret = None;
    let (username, password, provider_id) = if protocol == "s3" {
        let credentials = target.s3_credentials();
        if let Some(region) = credentials.region {
            options.insert("region".to_string(), serde_json::Value::String(region));
        }
        // A secret from the AWS tools' chain (`AWS_*`, `~/.aws`) or from
        // AEROFTP_PASSWORD never enters the request the webview reads: it
        // carries a handle bound to this connection, and provider_connect
        // reads the secret again in Rust (`launch_url::launch_secret`). A
        // URL password travels in the request, as the URL itself does.
        use crate::launch_url::aws::CredentialSource;
        use crate::launch_url::launch_secret::Provenance;
        let provenance = match credentials.secret_source {
            Some(source) if source.is_aws_chain() => Some(Provenance::AwsChain {
                access_key_id: credentials.access_key_id.clone(),
                key_from_url: credentials.key_from_url,
            }),
            Some(CredentialSource::AeroftpEnv) => Some(Provenance::Env("AEROFTP_PASSWORD")),
            _ => None,
        };
        let password = match provenance {
            Some(provenance) => {
                pending_secret = Some(provenance);
                None
            }
            None => credentials.secret_access_key,
        };
        (
            credentials.access_key_id,
            password,
            Some(target.s3_provider_id().to_string()),
        )
    } else if let Some(var) = target
        .env_secret_var()
        .filter(|_| !crate::launch_url::env_secret_in_request(protocol))
    {
        // The environment first, as the CLI reads it; its secret stays in
        // Rust behind a handle bound to this connection.
        pending_secret = Some(crate::launch_url::launch_secret::Provenance::Env(var));
        (username, None, None)
    } else {
        // The environment first, then the URL, as the CLI reads them
        // (docs/CLI-GUIDE.md ranks the URL password last). FTP, SFTP and
        // WebDAV keep an environment secret in the request (Requested 224).
        let password = target.env_secret().or_else(|| {
            target
                .url_secret
                .clone()
                .filter(|secret| !secret.is_empty())
        });
        (username, password, None)
    };
    if let Some(branch) = branch {
        options.insert(
            "githubBranch".to_string(),
            serde_json::Value::String(branch),
        );
    }
    // The handle binds the secret to the address the link's own connection
    // reaches. A link whose connection cannot be described binds none: the
    // connect then asks, or fails on the same missing field.
    // An S3 link's own endpoint carries its port (or none): the host's port,
    // or the default one, is not added to it.
    let port =
        (!target.endpoint_given()).then(|| target.port.unwrap_or_else(|| default_port(&target)));
    if let Some(provenance) = pending_secret {
        // The port the request carries, an absent one filled the way the
        // webview fills it: the binding then sees the address the connect
        // reaches, not the link host's port beside an endpoint of its own.
        match launch_destination(
            protocol,
            &server,
            port.unwrap_or_else(|| default_port(&target)),
            &username,
            provider_id.as_deref(),
            &options,
        ) {
            Ok(destination) => {
                options.insert(
                    "launchSecret".to_string(),
                    serde_json::Value::String(crate::launch_url::launch_secret::register(
                        protocol,
                        &destination,
                        provenance,
                    )),
                );
            }
            Err(err) => tracing::debug!("launch secret not bound: {err}"),
        }
    }
    // A repository URL names no folder: its path is the repository.
    let (initial_path, folder, name, is_dir) = if matches!(protocol, "github" | "gitlab") {
        (None, "/".to_string(), String::new(), true)
    } else {
        (initial_path, folder, name, is_dir)
    };

    Some(OpenRequest {
        kind: OpenKind::RemoteUrl,
        path: crate::launch_url::redact(raw),
        folder,
        name,
        is_dir,
        protocol: Some(protocol.to_string()),
        server: Some(server),
        port,
        username: (!username.is_empty()).then_some(username),
        password,
        initial_path,
        profile_name: None,
        provider_id,
        options: (!options.is_empty()).then_some(options),
        sign_in: false,
    })
}

/// The folder a remote path opens on, and the file to select in it.
fn split_remote_path(decoded_path: &str) -> (String, String, String, bool) {
    if decoded_path.is_empty() || decoded_path == "/" {
        ("/".to_string(), "/".to_string(), String::new(), true)
    } else if decoded_path.ends_with('/') {
        (
            decoded_path.to_string(),
            decoded_path.to_string(),
            String::new(),
            true,
        )
    } else {
        let path_buf = Path::new(decoded_path);
        let file_name = path_buf
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let parent = path_buf
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "/".to_string());
        (parent.clone(), parent, file_name, false)
    }
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
            OpenKind::RemoteUrl => crate::launch_url::redact(&request.path),
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
        // The environment is read before the URL password: hold the lock
        // so a variable another test sets never stands in for it.
        let _guard = crate::test_env::lock();
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
        // No folder named: the app keeps an open tab where it is.
        assert_eq!(req.initial_path, None);
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
            // FTP with explicit TLS on the FTP port (was FTPS on 990).
            ("ftpes://example.com/", "ftp", 21),
            ("webdav://example.com/", "webdav", 80),
            ("webdavs://example.com/", "webdav", 443),
            // The CLI's WebDAV aliases open in the app too (#1534).
            ("http://example.com/", "webdav", 80),
            ("https://example.com/", "webdav", 443),
            ("s3://example.com/?bucket=b", "s3", 443),
            ("immich://example.com/", "immich", 2283),
        ];
        for (raw, expected_proto, expected_port) in cases {
            let req = parse_remote_url(raw).unwrap();
            assert_eq!(req.protocol.as_deref(), Some(expected_proto), "{raw}");
            assert_eq!(req.port, Some(expected_port), "{raw}");
        }
    }

    #[test]
    fn remote_url_percent_encoding_decoding() {
        // The environment is read before the URL password: hold the lock
        // so a variable another test sets never stands in for it.
        let _guard = crate::test_env::lock();
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

    /// The variables an SFTP launch reads its secret from.
    const SFTP_SECRET_VARS: &[&str] = &["SFTP_PASSWORD", "AEROFTP_PASSWORD"];

    #[test]
    fn remote_url_env_var_password_fallback() {
        let _guard = crate::test_env::lock();
        let password = |env: &[(&str, &str)], raw: &str| {
            with_env(SFTP_SECRET_VARS, env, || {
                parse_remote_url(raw).unwrap().password
            })
        };
        assert_eq!(
            password(
                &[("SFTP_PASSWORD", "env_sftp_secret")],
                "sftp://alice@host/"
            )
            .as_deref(),
            Some("env_sftp_secret")
        );
        assert_eq!(
            password(
                &[("AEROFTP_PASSWORD", "env_generic_secret")],
                "sftp://alice@host/"
            )
            .as_deref(),
            Some("env_generic_secret")
        );
        assert_eq!(password(&[], "sftp://alice@host/"), None);
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
    fn the_app_opens_every_scheme_the_launch_table_has() {
        // The environment is read before the URL password: hold the lock
        // so a variable another test sets never stands in for it.
        let _guard = crate::test_env::lock();
        use crate::launch_url::{example_url, LaunchAuth, LAUNCH_SCHEMES};
        for entry in LAUNCH_SCHEMES {
            let url = example_url(entry);
            assert!(is_remote_url(&url), "{url}");
            let request = parse_remote_url(&url).unwrap_or_else(|| panic!("{url}"));
            assert_eq!(request.kind, OpenKind::RemoteUrl, "{url}");
            assert_eq!(request.protocol.as_deref(), Some(entry.token), "{url}");
            assert_eq!(request.sign_in, entry.auth == LaunchAuth::Account, "{url}");
            if request.sign_in {
                assert_eq!(request.password, None, "{url}");
            } else {
                assert!(
                    request.server.as_deref().is_some_and(|s| !s.is_empty()),
                    "{url}"
                );
            }
        }
        assert!(parse_remote_url("gopher://host/").is_none());
    }

    #[test]
    fn launch_urls_carry_the_account_fields_in_the_apps_option_spelling() {
        let s3 = parse_remote_url(
            "s3://AKID@minio.lan:9000/photos/?bucket=b1&region=eu-west-1&path_style=true",
        )
        .unwrap();
        assert_eq!(s3.server.as_deref(), Some("minio.lan"));
        assert_eq!(s3.port, Some(9000));
        assert_eq!(s3.username.as_deref(), Some("AKID"));
        let options = s3.options.unwrap();
        assert_eq!(options["bucket"], "b1");
        assert_eq!(options["region"], "eu-west-1");
        assert_eq!(options["pathStyle"], true);
        assert_eq!(s3.initial_path.as_deref(), Some("/photos/"));

        let b2 = parse_remote_url("b2://KEYID@my-bucket/x/").unwrap();
        assert_eq!(b2.protocol.as_deref(), Some("backblaze"));
        assert_eq!(b2.server.as_deref(), Some("api.backblazeb2.com"));
        assert_eq!(b2.username.as_deref(), Some("KEYID"));
        assert_eq!(b2.options.unwrap()["bucket"], "my-bucket");

        let kdrive = parse_remote_url("kdrive://123456/").unwrap();
        assert_eq!(kdrive.options.unwrap()["bucket"], "123456");

        let azure =
            parse_remote_url("azure://acct@acct.blob.core.windows.net/?container=c1").unwrap();
        assert_eq!(azure.options.unwrap()["bucket"], "c1");

        let immich = parse_remote_url("immich://10.0.0.2:2283/?scheme=http").unwrap();
        assert_eq!(immich.server.as_deref(), Some("http://10.0.0.2:2283"));

        let github = parse_remote_url("github://owner/repo@dev").unwrap();
        assert_eq!(github.server.as_deref(), Some("owner/repo"));
        assert_eq!(github.options.unwrap()["githubBranch"], "dev");

        // The GitLab backend reads the project from the host: the URL's path
        // is the project, never a folder inside it.
        let gitlab = parse_remote_url("gitlab://gitlab.com/group/project@dev").unwrap();
        assert_eq!(gitlab.server.as_deref(), Some("gitlab.com/group/project"));
        assert_eq!(gitlab.initial_path, None);
        assert_eq!(gitlab.options.unwrap()["githubBranch"], "dev");

        let ftpes = parse_remote_url("ftpes://u@h/").unwrap();
        assert_eq!(ftpes.protocol.as_deref(), Some("ftp"));
        assert_eq!(ftpes.port, Some(21));
        assert_eq!(ftpes.options.unwrap()["tlsMode"], "explicit");
    }

    /// Every variable the S3 chain reads: a test clears the ones the
    /// developer's shell holds, so an inherited key never decides it.
    const AWS_CHAIN_VARS: &[&str] = &[
        "AEROFTP_PASSWORD",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
        "AWS_SHARED_CREDENTIALS_FILE",
        "AWS_CONFIG_FILE",
    ];

    /// Run `run` with the AWS tools' files in a temporary folder and only the
    /// variables of `env` set among the chain's, restoring the inherited ones
    /// after. The caller holds the env lock.
    fn with_aws_files<T>(
        credentials: &str,
        config: &str,
        env: &[(&str, &str)],
        run: impl FnOnce() -> T,
    ) -> T {
        let dir = tempfile::tempdir().unwrap();
        let credentials_path = dir.path().join("credentials");
        let config_path = dir.path().join("config");
        std::fs::write(&credentials_path, credentials).unwrap();
        std::fs::write(&config_path, config).unwrap();
        let credentials_path = credentials_path.to_str().unwrap();
        let config_path = config_path.to_str().unwrap();
        let mut set = vec![
            ("AWS_SHARED_CREDENTIALS_FILE", credentials_path),
            ("AWS_CONFIG_FILE", config_path),
        ];
        set.extend_from_slice(env);
        with_env(AWS_CHAIN_VARS, &set, run)
    }

    /// Run `run` with `names` cleared and only `env` set among them,
    /// restoring the inherited values after. The caller holds the env lock.
    fn with_env<T>(names: &[&str], env: &[(&str, &str)], run: impl FnOnce() -> T) -> T {
        let inherited: Vec<(&str, Option<std::ffi::OsString>)> = names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        unsafe {
            for name in names {
                std::env::remove_var(name);
            }
            for (name, value) in env {
                std::env::set_var(name, value);
            }
        }
        let result = run();
        unsafe {
            for (name, value) in inherited {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
        result
    }

    #[test]
    fn an_s3_url_without_keys_reads_the_aws_chain() {
        let _guard = crate::test_env::lock();
        let request = with_aws_files(
            "[default]\naws_access_key_id = FILEKEY\naws_secret_access_key = filesecret\naws_session_token = filetoken\n",
            "[default]\nregion = eu-west-3\n",
            &[],
            || parse_remote_url("s3://minio.lan:9000/?bucket=b1").unwrap(),
        );
        assert_eq!(request.username.as_deref(), Some("FILEKEY"));
        // The chain's secret and token stay in Rust: the request the webview
        // reads carries only the handle provider_connect resolves.
        assert_eq!(request.password, None);
        assert_never_serialized(&request, &["filesecret", "filetoken"]);
        let options = request.options.clone().unwrap();
        assert!(is_handle(&options["launchSecret"]), "{options:?}");
        assert!(!options.contains_key("sessionToken"));
        assert_eq!(options["region"], "eu-west-3");
        assert_eq!(request.provider_id.as_deref(), Some("minio"));

        // The AWS variables win over the files.
        let request = with_aws_files(
            "[default]\naws_access_key_id = FILEKEY\naws_secret_access_key = filesecret\n",
            "",
            &[
                ("AWS_ACCESS_KEY_ID", "ENVKEY"),
                ("AWS_SECRET_ACCESS_KEY", "envsecret"),
            ],
            || parse_remote_url("s3://minio.lan:9000/?bucket=b1").unwrap(),
        );
        assert_eq!(request.username.as_deref(), Some("ENVKEY"));
        assert_eq!(request.password, None);
        assert_never_serialized(&request, &["envsecret", "filesecret"]);
        assert!(is_handle(&request.options.unwrap()["launchSecret"]));

        // A key id from the URL beside a bare AWS secret: the handle keeps
        // that provenance, so the connect pairs the two again.
        let (request, connect) = with_aws_files(
            "[default]\naws_access_key_id = FILEKEY\naws_secret_access_key = filesecret\n",
            "",
            &[("AWS_SECRET_ACCESS_KEY", "envsecret")],
            || {
                let request = parse_remote_url("s3://URLKEY@minio.lan:9000/?bucket=b1").unwrap();
                let connect = webview_params(&request)
                    .to_provider_config()
                    .map(|config| config.password.unwrap_or_default());
                (request, connect)
            },
        );
        assert_eq!(request.username.as_deref(), Some("URLKEY"));
        assert_eq!(request.password, None);
        assert_never_serialized(&request, &["envsecret", "filesecret"]);
        assert_eq!(connect.as_deref(), Ok("envsecret"));
    }

    /// The connect parameters the webview builds from a launch request
    /// (`openRemoteUrlSession` then `buildProviderParams` in App.tsx), for
    /// the fields a connection reads: an absent port is the protocol's, and
    /// an S3 endpoint is the stored one, else a non-AWS host
    /// (`resolveProfileS3Location`).
    fn webview_params(request: &OpenRequest) -> crate::provider_commands::ProviderConnectionParams {
        let options = request.options.clone().unwrap_or_default();
        let text = |key: &str| {
            options
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let protocol = request.protocol.clone().unwrap();
        let server = request.server.clone().unwrap_or_default();
        let endpoint = text("endpoint").or_else(|| {
            (protocol == "s3"
                && crate::bridge_shared::map_s3_provider_from_endpoint(&server) != "amazon-s3")
                .then(|| server.clone())
        });
        let port = request.port.unwrap_or_else(|| {
            crate::connection_manifest::connection_spec(&protocol)
                .map_or(0, |spec| spec.default_port)
        });
        serde_json::from_value(serde_json::json!({
            "protocol": protocol,
            "provider_id": request.provider_id,
            "server": server,
            "port": port,
            "username": request.username.clone().unwrap_or_default(),
            "password": request.password.clone().unwrap_or_default(),
            "launch_secret": text("launchSecret"),
            "bucket": text("bucket"),
            "region": text("region").unwrap_or_else(|| "us-east-1".to_string()),
            "endpoint": endpoint,
            "path_style": options.get("pathStyle").and_then(serde_json::Value::as_bool),
            "verify_cert": true,
            "allow_cleartext_endpoint": false,
            "allow_cleartext_storage": false,
        }))
        .unwrap()
    }

    #[test]
    fn an_s3_endpoint_without_a_port_beside_a_host_port_still_reads_its_secret() {
        // The endpoint names no port and the host names 9000: the request
        // drops the host's port, the webview fills 443, and the binding
        // must see that same address.
        let _guard = crate::test_env::lock();
        let sentinel = "sentinel-endpoint-port-8d02";
        let vars = [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AEROFTP_PASSWORD",
        ];
        for env in [
            &[("AWS_SECRET_ACCESS_KEY", sentinel)][..],
            &[("AEROFTP_PASSWORD", sentinel)][..],
        ] {
            let connected = with_env(&vars, &[], || {
                with_aws_files("", "", env, || {
                    let request = parse_remote_url(
                        "s3://KEY@minio.lan:9000/?bucket=b1&endpoint=https://storage.example.com",
                    )
                    .unwrap();
                    assert_eq!(request.port, None);
                    assert_never_serialized(&request, &[sentinel]);
                    webview_params(&request)
                        .to_provider_config()
                        .map(|config| config.password)
                })
            });
            assert_eq!(connected, Ok(Some(sentinel.to_string())), "{env:?}");
        }
    }

    #[test]
    fn a_launch_secret_reaches_only_the_address_the_link_named() {
        // The webview keeps the link's server and handle but moves what the
        // connection reaches: an S3 endpoint, an AWS region spliced into the
        // host, the bucket a virtual-hosted request puts in the host, the
        // Azure account, a Swift auth host, or the TLS check. Each refuses
        // and never yields the secret; the link's own connection reads it.
        let _guard = crate::test_env::lock();
        let sentinel = "sentinel-launch-destination-41c7";
        type Edit = fn(&mut crate::provider_commands::ProviderConnectionParams);
        type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a [(&'a str, Edit)]);
        let cases: &[Case] = &[
            (
                "s3://minio.lan:9443/?bucket=b1",
                &[
                    ("AWS_ACCESS_KEY_ID", "ENVKEY"),
                    ("AWS_SECRET_ACCESS_KEY", sentinel),
                ],
                &[
                    ("endpoint to another host", |p| {
                        p.endpoint = Some("https://evil.example".into())
                    }),
                    ("endpoint to another port", |p| {
                        p.endpoint = Some("https://minio.lan:9000".into())
                    }),
                    ("endpoint in cleartext", |p| {
                        p.endpoint = Some("http://minio.lan:9443".into())
                    }),
                    ("another bucket", |p| {
                        p.bucket = Some("evil.example#".into())
                    }),
                    ("certificate check off", |p| p.verify_cert = Some(false)),
                    ("cleartext allowed", |p| {
                        p.allow_cleartext_endpoint = Some(true)
                    }),
                ],
            ),
            (
                "s3://s3.amazonaws.com/?bucket=b1&region=eu-west-1",
                &[
                    ("AWS_ACCESS_KEY_ID", "ENVKEY"),
                    ("AWS_SECRET_ACCESS_KEY", sentinel),
                ],
                &[
                    ("a region that is a host", |p| {
                        p.region = Some("evil.example/".into())
                    }),
                    ("an endpoint off AWS", |p| {
                        p.endpoint = Some("s3.amazonaws.com.evil.example".into())
                    }),
                ],
            ),
            (
                "azure://account@account.blob.core.windows.net/?container=c1",
                &[("AZURE_STORAGE_KEY", sentinel)],
                &[("another server", |p| p.server = "evil.example".into())],
            ),
            (
                // The shared service host: the account names the endpoint.
                "azure://account@blob.core.windows.net/?container=c1",
                &[("AZURE_STORAGE_KEY", sentinel)],
                &[("another account", |p| p.username = "evil".into())],
            ),
            (
                "swift://user@keystone.example.com:5000/",
                &[("ST_KEY", sentinel)],
                &[
                    ("another auth host", |p| {
                        p.server = "evil.example:5000".into()
                    }),
                    ("cleartext storage", |p| {
                        p.allow_cleartext_storage = Some(true)
                    }),
                ],
            ),
        ];
        let vars = [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AZURE_STORAGE_KEY",
            "ST_KEY",
            "OS_PASSWORD",
            "AEROFTP_PASSWORD",
        ];
        for (url, env, edits) in cases {
            with_env(&vars, &[], || {
                with_aws_files("", "", env, || {
                    let request = parse_remote_url(url).unwrap();
                    assert_never_serialized(&request, &[sentinel]);
                    let connected = webview_params(&request).to_provider_config();
                    assert_eq!(
                        connected.map(|config| config.password),
                        Ok(Some(sentinel.to_string())),
                        "{url}: the link's own connection"
                    );
                    for (what, edit) in *edits {
                        let mut params = webview_params(&request);
                        edit(&mut params);
                        let outcome = params.to_provider_config();
                        let err = outcome
                            .err()
                            .unwrap_or_else(|| panic!("{url}: {what} was accepted"));
                        assert!(!err.contains(sentinel), "{url}: {what}: {err}");
                    }
                })
            });
        }
    }

    fn is_handle(value: &serde_json::Value) -> bool {
        value.as_str().is_some_and(|handle| {
            handle.len() == 32 && handle.chars().all(|c| c.is_ascii_hexdigit())
        })
    }

    /// The JSON the webview receives through `take_open_requests` holds none
    /// of `secrets`.
    fn assert_never_serialized(request: &OpenRequest, secrets: &[&str]) {
        let json = serde_json::to_string(request).unwrap();
        for secret in secrets {
            assert!(
                !json.contains(secret),
                "{secret} reaches the webview: {json}"
            );
        }
    }

    #[test]
    fn an_s3_url_password_travels_as_written_and_aeroftp_password_stays_in_rust() {
        let _guard = crate::test_env::lock();
        let request = with_aws_files("", "", &[], || {
            parse_remote_url("s3://KEY:urlsecret@minio.lan:9000/?bucket=b1").unwrap()
        });
        assert_eq!(request.password.as_deref(), Some("urlsecret"));
        assert!(!request.options.unwrap().contains_key("launchSecret"));
        let request = with_aws_files("", "", &[("AEROFTP_PASSWORD", "aeroftpsecret")], || {
            parse_remote_url("s3://KEY@minio.lan:9000/?bucket=b1").unwrap()
        });
        assert_eq!(request.password, None);
        assert_never_serialized(&request, &["aeroftpsecret"]);
        assert!(is_handle(&request.options.unwrap()["launchSecret"]));
    }

    #[test]
    fn no_environment_secret_of_a_provider_scheme_reaches_the_webview() {
        // Every scheme with a secret variable, read from the table: a new
        // one is covered when it is added.
        let _guard = crate::test_env::lock();
        let sentinel = "sentinel-provider-secret-5a9e";
        for scheme in crate::launch_url::LAUNCH_SCHEMES
            .iter()
            .filter(|scheme| scheme.auth == crate::launch_url::LaunchAuth::Server)
        {
            for var in scheme
                .secret_env
                .iter()
                .copied()
                .chain(["AEROFTP_PASSWORD"])
            {
                let url = crate::launch_url::example_url(scheme);
                // The outer call puts the variable back as it was.
                let outcome = with_env(&[var], &[], || {
                    with_aws_files("", "", &[(var, sentinel)], || {
                        let request = parse_remote_url(&url)?;
                        // The connect the webview makes from the request.
                        let connected = webview_params(&request)
                            .to_provider_config()
                            .map(|config| config.password.unwrap_or_default());
                        Some((request, connected))
                    })
                });
                let Some((request, connected)) = outcome else {
                    continue;
                };
                if crate::launch_url::env_secret_in_request(scheme.token) {
                    // FTP, SFTP and WebDAV, as before (Requested 224).
                    continue;
                }
                assert_never_serialized(&request, &[sentinel]);
                let options = request.options.clone().unwrap_or_default();
                assert!(
                    options.get("launchSecret").is_some_and(is_handle),
                    "{} with {var}: {options:?}",
                    scheme.scheme
                );
                assert_eq!(
                    connected.as_deref(),
                    Ok(sentinel),
                    "{} with {var}: the link's own connect reads the secret",
                    scheme.scheme
                );
            }
        }
    }

    #[test]
    fn an_inherited_aws_identity_never_decides_a_test() {
        // The developer's shell holds another identity and profile: the
        // helper clears them for the run and puts them back after.
        let _guard = crate::test_env::lock();
        let inherited: Vec<_> = ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_PROFILE"]
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        unsafe {
            std::env::set_var("AWS_ACCESS_KEY_ID", "SHELLKEY");
            std::env::set_var("AWS_SECRET_ACCESS_KEY", "shellsecret");
            std::env::set_var("AWS_PROFILE", "other");
        }
        let request = with_aws_files(
            "[default]\naws_access_key_id = FILEKEY\naws_secret_access_key = filesecret\n",
            "",
            &[],
            || parse_remote_url("s3://minio.lan:9000/?bucket=b1").unwrap(),
        );
        assert_eq!(request.username.as_deref(), Some("FILEKEY"));
        assert_eq!(request.password, None);
        assert_never_serialized(&request, &["filesecret", "shellsecret"]);
        assert_eq!(
            std::env::var("AWS_ACCESS_KEY_ID").as_deref(),
            Ok("SHELLKEY")
        );
        unsafe {
            for (name, value) in inherited {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn an_s3_endpoint_names_its_preset_and_the_region_it_carries() {
        let _guard = crate::test_env::lock();
        let request = with_aws_files("", "", &[], || {
            parse_remote_url(
                "s3://KEY:secret@s3.eu-central-2.wasabisys.com/?bucket=b1&region=us-east-1",
            )
            .unwrap()
        });
        assert_eq!(request.provider_id.as_deref(), Some("wasabi"));
        assert_eq!(request.options.unwrap()["region"], "eu-central-2");

        let generic = with_aws_files("", "", &[], || {
            parse_remote_url("s3://KEY:secret@storage.example.net/?bucket=b1").unwrap()
        });
        assert_eq!(generic.provider_id.as_deref(), Some("custom-s3"));
        assert!(!generic.options.unwrap().contains_key("region"));
    }

    #[test]
    fn a_link_that_names_no_folder_carries_none_and_a_slash_names_the_root() {
        for (bare, root) in [
            ("sftp://u@h.example", "sftp://u@h.example/"),
            ("ftp://h.example", "ftp://h.example/"),
            ("dropbox://work", "dropbox://work/"),
        ] {
            let none = parse_remote_url(bare).unwrap();
            assert_eq!(none.initial_path, None, "{bare}");
            let slash = parse_remote_url(root).unwrap();
            assert_eq!(slash.initial_path.as_deref(), Some("/"), "{root}");
            assert_eq!(none.sign_in, slash.sign_in, "{bare}");
        }
        let empty_host = parse_remote_url("dropbox:///").unwrap();
        assert_eq!(empty_host.initial_path.as_deref(), Some("/"));
    }

    #[test]
    fn a_sign_in_url_names_the_account_and_never_a_secret() {
        let request = parse_remote_url("dropbox://work:hunter2@/Photos/")
            .or_else(|| parse_remote_url("dropbox://work/Photos/"));
        let request = request.unwrap();
        assert!(request.sign_in);
        assert_eq!(request.password, None);
        assert_eq!(request.username, None);
        assert_eq!(request.initial_path.as_deref(), Some("/Photos/"));
        assert!(!request.path.contains("hunter2"));
        let named = parse_remote_url("onedrive://work/Reports").unwrap();
        assert_eq!(named.server.as_deref(), Some("work"));
        // Without the final `/` the last name is a file to select in its
        // folder, which is why the notices write `<profile name>/folder/`.
        assert_eq!(named.initial_path.as_deref(), Some("/"));
        assert_eq!((named.name.as_str(), named.is_dir), ("Reports", false));
        let folder = parse_remote_url("onedrive://work/Reports/").unwrap();
        assert_eq!(folder.initial_path.as_deref(), Some("/Reports/"));
        assert!(folder.is_dir);
    }

    #[test]
    fn the_environment_secret_wins_over_the_url_password_as_in_the_cli() {
        let _guard = crate::test_env::lock();
        let password = |env: &[(&str, &str)]| {
            with_env(SFTP_SECRET_VARS, env, || {
                parse_remote_url("sftp://alice:url_secret@host/")
                    .unwrap()
                    .password
            })
        };
        assert_eq!(
            password(&[("SFTP_PASSWORD", "env_sftp_secret")]).as_deref(),
            Some("env_sftp_secret")
        );
        assert_eq!(
            password(&[("AEROFTP_PASSWORD", "env_generic_secret")]).as_deref(),
            Some("env_generic_secret")
        );
        // With neither variable set, whatever the developer's shell holds,
        // the URL's own password is the one read.
        assert_eq!(password(&[]).as_deref(), Some("url_secret"));
    }

    #[test]
    fn a_webdav_url_keeps_the_transport_it_names() {
        let options = |raw: &str| parse_remote_url(raw).unwrap().options.unwrap_or_default();
        assert_eq!(options("https://localhost:8080/")["webdavScheme"], "https");
        assert_eq!(
            options("webdavs://dav.example.com:8443/")["webdavScheme"],
            "https"
        );
        assert_eq!(options("http://nas.lan/")["webdavScheme"], "http");
        assert!(!options("webdav://dav.example.com/").contains_key("webdavScheme"));
    }

    #[test]
    fn redact_url_for_log_masks_passwords() {
        assert_eq!(
            crate::launch_url::redact(
                "sftp://admin:VerySecret123@files.corp.internal:2222/var/log"
            ),
            "sftp://admin:***@files.corp.internal:2222/var/log"
        );
        assert_eq!(
            crate::launch_url::redact("ftp://guest@example.com/pub"),
            "ftp://guest@example.com/pub"
        );
        assert_eq!(crate::launch_url::redact("not-a-url"), "not-a-url");
    }

    #[test]
    fn malformed_and_unsupported_urls_return_none() {
        assert_eq!(parse_remote_url("sftp://"), None);
        assert_eq!(parse_remote_url("sftp:///path"), None);
        assert_eq!(parse_remote_url("gopher://host/path"), None);
        assert_eq!(classify("gopher://host/path", None), None);
    }
}
