//! Proton Drive via the official Proton Drive CLI (`proton-drive`).
//!
//! The user installs the binary and signs in with `proton-drive auth login`
//! (browser). AeroFTP never sees credentials; the session lives in the OS
//! secret store managed by Proton's CLI. Same shape as [`super::mega::MegaCmdProvider`].
//!
//! Install: https://proton.me/download/drive/cli/index.html

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::process::Command;

use super::{
    ProviderError, ProviderType, RemoteEntry, ShareLinkOptions, ShareLinkResult, StorageProvider,
};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const INSTALL_URL: &str = "https://proton.me/download/drive/cli/index.html";
const META_TIMEOUT_SECS: u64 = 120;
const TRANSFER_TIMEOUT_SECS: u64 = 6 * 60 * 60;
const MAX_RETRIES: usize = 2;
const RETRY_DELAY_MS: u64 = 2000;

const MISSING_CLI: &str = "Proton Drive CLI (`proton-drive`) is not installed. Download it from https://proton.me/download/drive/cli/index.html, then run `proton-drive auth login` in a terminal.";
const NEED_LOGIN: &str = "Proton Drive CLI is installed but not signed in. Run `proton-drive auth login` in a terminal, finish in the browser, then connect again.";

#[derive(Debug, Clone)]
pub struct ProtonConfig {
    pub display_name: String,
    pub binary_path: Option<String>,
}

impl ProtonConfig {
    pub fn from_provider_config(config: &super::ProviderConfig) -> Result<Self, ProviderError> {
        let display_name = config
            .username
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                let n = config.name.trim();
                if n.is_empty() {
                    None
                } else {
                    Some(n.to_string())
                }
            })
            .unwrap_or_else(|| "Proton Drive".to_string());
        // The executable is never taken from the profile. Profile options
        // travel in exported `.aeroftp` files and in imports, and every option
        // reaches `extra`, so a path there would make AeroFTP run whatever
        // file an imported profile names. No part of the GUI sets one. A CLI
        // kept outside PATH and the usual locations is named with the
        // AEROFTP_PROTON_CLI environment variable, which a file cannot set.
        let binary_path = std::env::var("AEROFTP_PROTON_CLI")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Ok(Self {
            display_name,
            binary_path,
        })
    }
}

pub struct ProtonCliProvider {
    config: ProtonConfig,
    connected: bool,
    current_path: String,
    account_email: Option<String>,
    binary: String,
}

impl ProtonCliProvider {
    pub fn new(config: ProtonConfig) -> Self {
        let binary = resolve_proton_drive(config.binary_path.as_deref());
        Self {
            config,
            connected: false,
            current_path: "/".to_string(),
            account_email: None,
            binary,
        }
    }

    fn log(&self, msg: &str) {
        tracing::debug!(target: "proton", "{}", msg);
    }

    async fn run_cli(&self, args: &[&str], timeout_secs: u64) -> Result<String, ProviderError> {
        self.log(&format!("[CMD] proton-drive {:?}", redact_cli_args(args)));
        if self.binary.is_empty() || !binary_exists(&self.binary) {
            return Err(ProviderError::InvalidConfig(MISSING_CLI.to_string()));
        }

        let mut last_err = ProviderError::ServerError("No attempts made".to_string());
        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                tracing::debug!(
                    target: "proton",
                    "[RETRY] attempt {}/{} {:?}",
                    attempt,
                    MAX_RETRIES,
                    redact_cli_args(args)
                );
                tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
            }

            let mut cmd = Command::new(&self.binary);
            cmd.args(args);
            cmd.kill_on_drop(true);
            // The CLI needs the user's environment (HOME, XDG_*, the display
            // for the browser login), so it is not cleared, but nothing of
            // AeroFTP's own: AEROFTP_MASTER_PASSWORD and the like stay here.
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("AEROFTP_") {
                    cmd.env_remove(&key);
                }
            }
            #[cfg(windows)]
            {
                cmd.creation_flags(CREATE_NO_WINDOW);
            }

            let output = match tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs),
                cmd.output(),
            )
            .await
            {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => {
                    return Err(ProviderError::ServerError(format!(
                        "Failed to execute proton-drive ({}): {}",
                        self.binary, e
                    )));
                }
                Err(_) => return Err(ProviderError::Timeout),
            };

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if output.status.success() {
                return Ok(stdout);
            }

            let combined = if stderr.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                stderr.trim().to_string()
            };
            if combined.is_empty() {
                last_err = ProviderError::ServerError("Unknown Proton Drive CLI error".to_string());
            } else if is_transient(&combined) && attempt < MAX_RETRIES {
                last_err = classify_cli_error(&combined);
                continue;
            } else {
                return Err(classify_cli_error(&combined));
            }
        }
        Err(last_err)
    }

    fn resolve_path(&self, path: &str) -> String {
        let p = path.trim();
        if p.is_empty() || p == "." {
            return self.current_path.clone();
        }
        if p.starts_with('/') {
            return normalize_abs(p);
        }
        let joined = if p == ".." {
            parent_of(&self.current_path)
        } else if self.current_path == "/" {
            format!("/{}", p.trim_start_matches('/'))
        } else {
            format!(
                "{}/{}",
                self.current_path.trim_end_matches('/'),
                p.trim_start_matches('/')
            )
        };
        normalize_abs(&joined)
    }
}

/// The day passed to `proton-drive sharing set-url --expiration`, which takes
/// a date, not a time. A duration that ends later today would name today, a
/// link dead on arrival or refused, so the earliest day sent is tomorrow; the
/// same string is reported back as the link's expiry.
fn share_expiration_day(now: chrono::DateTime<chrono::Utc>, expires_in_secs: u64) -> String {
    let requested = (now + chrono::Duration::seconds(expires_in_secs as i64)).date_naive();
    let tomorrow = now.date_naive() + chrono::Duration::days(1);
    requested.max(tomorrow).format("%Y-%m-%d").to_string()
}

fn binary_exists(path: &str) -> bool {
    if path.contains('/') || path.contains('\\') {
        return Path::new(path).is_file();
    }
    look_in_path(path).is_some()
}

fn look_in_path(name: &str) -> Option<String> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().to_string());
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe.to_string_lossy().to_string());
            }
        }
    }
    None
}

pub(crate) fn resolve_proton_drive(custom: Option<&str>) -> String {
    if let Some(path) = custom {
        if Path::new(path).is_file() {
            return path.to_string();
        }
    }
    if let Some(found) = look_in_path("proton-drive") {
        return found;
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(&home).join(".local/bin/proton-drive"));
    }
    candidates.push(PathBuf::from("/usr/local/bin/proton-drive"));
    candidates.push(PathBuf::from("/usr/bin/proton-drive"));
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            candidates.push(PathBuf::from(local).join("proton-drive\\proton-drive.exe"));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            candidates.push(PathBuf::from(pf).join("Proton Drive CLI\\proton-drive.exe"));
        }
    }
    for c in candidates {
        if c.is_file() {
            return c.to_string_lossy().to_string();
        }
    }
    "proton-drive".to_string()
}

fn is_transient(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("temporarily unavailable")
        || lower.contains("try again")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection reset")
        || lower.contains("429")
}

fn normalize_local_path_for_cli(path: &str) -> String {
    #[cfg(windows)]
    {
        path.replace('/', "\\")
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

/// Hide the value after `--password` in the line we LOG.
///
/// **It protects the log and nothing else, and that belongs here rather than
/// only where it bites.** The same `args` slice goes to `Command::args`, so
/// while `proton-drive` runs, its password argument sits in the process list.
/// A function named "redact" invites the belief that the secret is handled,
/// and that belief is exactly what would stop someone from looking further.
///
/// **Who can see it is a property of the host, not of this code.** On Linux
/// `/proc/<pid>/cmdline` is world readable unless `/proc` is mounted with
/// `hidepid`, so on an ordinary desktop any local user can read it, not only
/// the one running AeroFTP; with `hidepid=2` they cannot.
///
/// Measured against `cli-drive@0.8.0+06e8c605`: `sharing set-url` takes the
/// password only as `--password PASSWORD`, with no stdin form and no
/// environment variable, and `proton-drive --help` names none either. So
/// there is nothing to switch to today. See `create_share_link`, the one call
/// in this file that carries a secret at all.
fn redact_cli_args(args: &[&str]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut hide_next = false;
    for arg in args {
        if hide_next {
            out.push("***".to_string());
            hide_next = false;
            continue;
        }
        if *arg == "--password" {
            hide_next = true;
        }
        out.push((*arg).to_string());
    }
    out
}

fn classify_cli_error(msg: &str) -> ProviderError {
    let lower = msg.to_lowercase();
    if lower.contains("need to login") || lower.contains("not authenticated") {
        ProviderError::AuthenticationFailed(NEED_LOGIN.to_string())
    } else if lower.contains("not found")
        || lower.contains("no such file")
        || lower.contains("does not exist")
        || lower.contains("could not find")
    {
        ProviderError::NotFound(msg.to_string())
    } else if lower.contains("insufficient quota") || lower.contains("storage quota") {
        ProviderError::ServerError("Storage quota exceeded".to_string())
    } else if lower.contains("permission denied") || lower.contains("access denied") {
        ProviderError::PermissionDenied(msg.to_string())
    } else if lower.contains("already exists") {
        ProviderError::AlreadyExists(msg.to_string())
    } else {
        ProviderError::ServerError(msg.to_string())
    }
}

fn normalize_abs(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    let mut out = String::from("/");
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if let Some(slash) = out.rfind('/') {
                if slash == 0 {
                    out.truncate(1);
                } else {
                    out.truncate(slash);
                }
            }
            continue;
        }
        if out != "/" {
            out.push('/');
        }
        out.push_str(part);
    }
    if out.is_empty() {
        "/".to_string()
    } else {
        out
    }
}

fn parent_of(path: &str) -> String {
    let n = normalize_abs(path);
    if n == "/" {
        return "/".to_string();
    }
    match n.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => n[..i].to_string(),
        None => "/".to_string(),
    }
}

fn basename(path: &str) -> String {
    let n = normalize_abs(path);
    if n == "/" {
        return "/".to_string();
    }
    n.rsplit('/').next().unwrap_or(&n).to_string()
}

fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{}", name.trim_start_matches('/'))
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    }
}

fn first_segment(path: &str) -> &str {
    path.trim_start_matches('/').split('/').next().unwrap_or("")
}

fn is_in_trash_path(path: &str) -> bool {
    matches!(first_segment(path), "trash" | "photos-trash")
}

fn node_uid(value: &Value) -> Option<String> {
    match value.get("uid") {
        Some(v) if v.is_string() => v.as_str().map(str::to_string),
        Some(v) => json_name(v),
        None => None,
    }
}

fn node_modified(value: &Value) -> Option<String> {
    value
        .get("activeRevision")
        .and_then(|r| r.get("claimedModificationTime"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("modificationTime")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
}

/// Stage `local_path` under `dest_name` so `proton-drive filesystem upload`
/// lands on the requested remote basename. The CLI always uses the local
/// basename; a symlink is rejected (`Not a regular file or directory`).
/// Hardlink first, copy when that fails (EXDEV or no hardlink support).
fn stage_local_as_dest(
    local_path: &str,
    dest_name: &str,
) -> Result<(PathBuf, PathBuf), ProviderError> {
    let staging = std::env::temp_dir().join(format!("aeroftp_proton_up_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&staging).map_err(ProviderError::IoError)?;
    let staged = staging.join(dest_name);
    match std::fs::hard_link(local_path, &staged) {
        Ok(()) => Ok((staging, staged)),
        Err(_) => match std::fs::copy(local_path, &staged) {
            Ok(_) => Ok((staging, staged)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                Err(ProviderError::IoError(e))
            }
        },
    }
}

fn json_name(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return Some(s.to_string());
    }
    let obj = value.as_object()?;
    if obj.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        return obj
            .get("value")
            .and_then(|v| v.as_str())
            .map(str::to_string);
    }
    None
}

fn parse_node(value: &Value, listed_path: &str) -> Option<RemoteEntry> {
    if let Some(path) = value.get("path").and_then(|v| v.as_str()) {
        if value.get("type").is_none() && value.get("name").is_none() {
            let name = basename(path);
            return Some(RemoteEntry {
                name,
                path: normalize_abs(path),
                is_dir: true,
                size: 0,
                modified: None,
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                mime_type: None,
                metadata: Default::default(),
            });
        }
    }

    let name = value
        .get("name")
        .and_then(json_name)
        .or_else(|| value.get("path").and_then(|v| v.as_str()).map(basename))?;
    let node_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("file");
    let is_dir = node_type == "folder" || node_type == "album" || node_type == "root";
    let path = value
        .get("path")
        .and_then(|v| v.as_str())
        .map(normalize_abs)
        .unwrap_or_else(|| join_path(listed_path, &name));
    let size = value
        .get("activeRevision")
        .and_then(|r| r.get("claimedSize"))
        .and_then(|v| v.as_u64())
        .or_else(|| value.get("totalStorageSize").and_then(|v| v.as_u64()))
        .or_else(|| {
            value
                .get("activeRevision")
                .and_then(|r| r.get("storageSize"))
                .and_then(|v| v.as_u64())
        })
        .unwrap_or(0);
    let modified = node_modified(value);
    let owner = value
        .get("ownedBy")
        .and_then(|v| v.get("email"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mime_type = value
        .get("mediaType")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut metadata = std::collections::HashMap::new();
    if let Some(uid) = node_uid(value) {
        metadata.insert("uid".to_string(), uid);
    }

    Some(RemoteEntry {
        name,
        path,
        is_dir,
        size,
        modified,
        permissions: None,
        owner,
        group: None,
        is_symlink: false,
        link_target: None,
        mime_type,
        metadata,
    })
}

pub(crate) fn parse_list_json(
    stdout: &str,
    listed_path: &str,
) -> Result<Vec<RemoteEntry>, ProviderError> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| ProviderError::ParseError(format!("Proton CLI list JSON: {e}: {trimmed}")))?;
    match value {
        Value::Array(items) => Ok(items
            .iter()
            .filter_map(|item| parse_node(item, listed_path))
            .collect()),
        Value::Object(_) => Ok(parse_node(&value, listed_path).into_iter().collect()),
        _ => Err(ProviderError::ParseError(
            "Proton CLI list JSON: expected array or object".to_string(),
        )),
    }
}

fn parse_info_json(stdout: &str, listed_path: &str) -> Result<RemoteEntry, ProviderError> {
    let trimmed = stdout.trim();
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| ProviderError::ParseError(format!("Proton CLI info JSON: {e}: {trimmed}")))?;
    parse_node(&value, listed_path)
        .ok_or_else(|| ProviderError::ParseError("Proton CLI info JSON: missing name".to_string()))
}

fn extract_email(stdout: &str) -> Option<String> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    value
        .get("ownedBy")
        .and_then(|v| v.get("email"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| value.get("keyAuthor").and_then(json_name))
}

fn extract_url(stdout: &str) -> Option<String> {
    let trimmed = stdout.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        for key in ["url", "link", "publicUrl", "public_url"] {
            if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
                if s.starts_with("http") {
                    return Some(s.to_string());
                }
            }
        }
        if let Some(s) = value.as_str() {
            if s.starts_with("http") {
                return Some(s.to_string());
            }
        }
    }
    trimmed.lines().find_map(|line| {
        line.split_whitespace()
            .find(|tok| tok.starts_with("https://"))
            .map(|s| s.trim_end_matches(['.', ',']).to_string())
    })
}

#[async_trait]
impl StorageProvider for ProtonCliProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Proton
    }

    fn display_name(&self) -> String {
        self.account_email
            .clone()
            .unwrap_or_else(|| self.config.display_name.clone())
    }

    fn account_email(&self) -> Option<String> {
        self.account_email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        self.binary = resolve_proton_drive(self.config.binary_path.as_deref());
        if !binary_exists(&self.binary) {
            return Err(ProviderError::InvalidConfig(format!(
                "{MISSING_CLI} ({INSTALL_URL})"
            )));
        }

        let listed = self
            .run_cli(&["filesystem", "list", "/", "-j"], META_TIMEOUT_SECS)
            .await?;
        let _roots = parse_list_json(&listed, "/")?;

        if let Ok(info) = self
            .run_cli(
                &["filesystem", "info", "/my-files", "-j"],
                META_TIMEOUT_SECS,
            )
            .await
        {
            if let Some(email) = extract_email(&info) {
                self.account_email = Some(email);
            }
        }

        self.current_path = "/".to_string();
        self.connected = true;
        tracing::info!("[Proton CLI] Connected as {}", self.display_name());
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        // Must not call `auth logout`: that session belongs to the user.
        self.connected = false;
        self.current_path = "/".to_string();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let target = self.resolve_path(path);
        let stdout = self
            .run_cli(&["filesystem", "list", &target, "-j"], META_TIMEOUT_SECS)
            .await
            .map_err(|e| match e {
                ProviderError::NotFound(_) => ProviderError::NotFound(target.clone()),
                other => other,
            })?;
        parse_list_json(&stdout, &target)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = self.resolve_path(path);
        self.run_cli(&["filesystem", "list", &new_path, "-j"], META_TIMEOUT_SECS)
            .await
            .map_err(|e| match e {
                ProviderError::NotFound(_) => {
                    ProviderError::NotFound(format!("Invalid directory: {new_path}"))
                }
                other => other,
            })?;
        self.current_path = new_path;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let remote = self.resolve_path(remote_path);
        if let Some(ref cb) = on_progress {
            cb(0, 0);
        }

        let dest = Path::new(local_path);
        let dest_is_dir = dest.is_dir()
            || local_path.ends_with('/')
            || local_path.ends_with(std::path::MAIN_SEPARATOR);
        let remote_name = basename(&remote);
        let final_path = if dest_is_dir {
            dest.join(&remote_name)
        } else {
            dest.to_path_buf()
        };
        let anchor = if dest_is_dir {
            dest.to_path_buf()
        } else {
            dest.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        };
        tokio::fs::create_dir_all(&anchor)
            .await
            .map_err(ProviderError::IoError)?;
        let temp_dir = anchor.join(format!(".aeroftp-proton-dl-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .map_err(ProviderError::IoError)?;
        let temp_dir_s = temp_dir.to_string_lossy().into_owned();
        let result = async {
            self.run_cli(
                &[
                    "filesystem",
                    "download",
                    "-f",
                    "remove",
                    "-d",
                    "merge",
                    &remote,
                    &temp_dir_s,
                ],
                TRANSFER_TIMEOUT_SECS,
            )
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Download failed: {e}")))?;
            let downloaded = temp_dir.join(&remote_name);
            if let Some(parent) = final_path.parent() {
                if !parent.as_os_str().is_empty() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(ProviderError::IoError)?;
                }
            }
            if final_path.exists() {
                let _ = tokio::fs::remove_file(&final_path).await;
            }
            tokio::fs::rename(&downloaded, &final_path)
                .await
                .map_err(ProviderError::IoError)?;
            Ok::<(), ProviderError>(())
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        result?;

        if let Some(ref cb) = on_progress {
            match std::fs::metadata(&final_path) {
                Ok(meta) => cb(meta.len(), meta.len()),
                Err(_) => cb(1, 1),
            }
        }
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let remote = self.resolve_path(remote_path);
        let file_name = basename(&remote);
        let temp_dir =
            std::env::temp_dir().join(format!("aeroftp_proton_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .map_err(ProviderError::IoError)?;
        let result = async {
            self.download(&remote, &temp_dir.to_string_lossy(), None)
                .await?;
            let temp_file = temp_dir.join(&file_name);
            let limit = super::MAX_DOWNLOAD_TO_BYTES;
            let metadata = tokio::fs::metadata(&temp_file)
                .await
                .map_err(ProviderError::IoError)?;
            if metadata.len() > limit {
                return Err(ProviderError::TransferFailed(format!(
                    "File too large for in-memory download ({:.1} MB). Use streaming download for files over {:.0} MB.",
                    metadata.len() as f64 / 1_048_576.0,
                    limit as f64 / 1_048_576.0,
                )));
            }
            tokio::fs::read(&temp_file)
                .await
                .map_err(ProviderError::IoError)
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        result
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let remote = self.resolve_path(remote_path);
        if let Some(ref cb) = on_progress {
            cb(0, 0);
        }
        let parent = parent_of(&remote);
        let local_cli = normalize_local_path_for_cli(local_path);
        let local_name = basename(&local_cli.replace('\\', "/"));
        let dest_name = basename(&remote);
        let (staging_dir, upload_path) = if local_name == dest_name {
            (None, local_cli)
        } else {
            let (dir, staged) = stage_local_as_dest(&local_cli, &dest_name)?;
            (Some(dir), staged.to_string_lossy().into_owned())
        };
        let result = self
            .run_cli(
                &[
                    "filesystem",
                    "upload",
                    "-f",
                    "create-new-revision",
                    "-d",
                    "merge",
                    &upload_path,
                    &parent,
                ],
                TRANSFER_TIMEOUT_SECS,
            )
            .await;
        if let Some(dir) = staging_dir {
            let _ = std::fs::remove_dir_all(&dir);
        }
        result.map_err(|e| ProviderError::TransferFailed(format!("Upload failed: {e}")))?;

        if let Some(ref cb) = on_progress {
            match std::fs::metadata(local_path) {
                Ok(meta) => cb(meta.len(), meta.len()),
                Err(_) => cb(1, 1),
            }
        }
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let abs = self.resolve_path(path);
        let parent = parent_of(&abs);
        let name = basename(&abs);
        self.run_cli(
            &["filesystem", "create-folder", &parent, "--", &name],
            META_TIMEOUT_SECS,
        )
        .await?;
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let abs = self.resolve_path(path);
        self.run_cli(&["filesystem", "trash", &abs], META_TIMEOUT_SECS)
            .await?;
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        let abs = self.resolve_path(path);
        let in_trash = is_in_trash_path(&abs);
        let trash_root = if first_segment(&abs) == "photos-trash" {
            "/photos-trash"
        } else {
            "/trash"
        };
        let name = basename(&abs);

        let captured_uid = if !in_trash {
            match self
                .run_cli(&["filesystem", "info", &abs, "-j"], META_TIMEOUT_SECS)
                .await
            {
                Ok(stdout) => parse_info_json(&stdout, &parent_of(&abs))
                    .ok()
                    .and_then(|e| e.metadata.get("uid").cloned()),
                Err(ProviderError::NotFound(_)) => return Ok(false),
                Err(e) => return Err(e),
            }
        } else {
            None
        };

        if !in_trash {
            match self
                .run_cli(&["filesystem", "trash", &abs], META_TIMEOUT_SECS)
                .await
            {
                Ok(_) => {}
                Err(ProviderError::NotFound(_)) => return Ok(false),
                Err(e) => return Err(e),
            }
        }

        let listed_stdout = self
            .run_cli(&["filesystem", "list", trash_root, "-j"], META_TIMEOUT_SECS)
            .await?;
        let matches: Vec<RemoteEntry> = parse_list_json(&listed_stdout, trash_root)?
            .into_iter()
            .filter(|e| e.name == name)
            .collect();
        let uid_matches = match captured_uid.as_deref() {
            Some(uid) => matches
                .iter()
                .filter(|e| e.metadata.get("uid").map(String::as_str) == Some(uid))
                .count(),
            None => matches.len(),
        };
        if matches.len() != 1 || uid_matches != 1 {
            return Err(ProviderError::ServerError(format!(
                "Moved to trash, not purged: {} items in trash have this name. Proton Drive CLI 0.8.0 can only address trash by name.",
                matches.len()
            )));
        }

        let purge = join_path(trash_root, &name);
        match self
            .run_cli(&["filesystem", "delete", &purge], META_TIMEOUT_SECS)
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => Err(e),
        }
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from = self.resolve_path(from);
        let to = self.resolve_path(to);
        let from_parent = parent_of(&from);
        let to_parent = parent_of(&to);
        let to_name = basename(&to);
        if from_parent == to_parent {
            self.run_cli(
                &["filesystem", "rename", &from, "--", &to_name],
                META_TIMEOUT_SECS,
            )
            .await?;
        } else {
            self.run_cli(
                &["filesystem", "move", &from, &to_parent],
                META_TIMEOUT_SECS,
            )
            .await?;
            let moved = join_path(&to_parent, &basename(&from));
            if basename(&from) != to_name {
                self.run_cli(
                    &["filesystem", "rename", &moved, "--", &to_name],
                    META_TIMEOUT_SECS,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let abs = self.resolve_path(path);
        if abs == "/" {
            return Ok(RemoteEntry::directory("/".to_string(), "/".to_string()));
        }
        let stdout = self
            .run_cli(&["filesystem", "info", &abs, "-j"], META_TIMEOUT_SECS)
            .await
            .map_err(|e| match e {
                ProviderError::NotFound(_) => ProviderError::NotFound(abs.clone()),
                other => other,
            })?;
        let mut entry = parse_info_json(&stdout, &parent_of(&abs))?;
        entry.path = abs;
        Ok(entry)
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        Ok(self.stat(path).await?.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        let version = self.run_cli(&["version"], META_TIMEOUT_SECS).await?;
        Ok(version.trim().to_string())
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from = self.resolve_path(from);
        let to = self.resolve_path(to);
        let parent = parent_of(&to);
        let name = basename(&to);
        if name == basename(&from) {
            self.run_cli(&["filesystem", "copy", &from, &parent], META_TIMEOUT_SECS)
                .await?;
        } else {
            // `-n -x` is ambiguous (measured). `--name=-x` (or `-n-x`) is the
            // form the CLI documents for an option value that starts with `-`.
            let name_flag = format!("--name={name}");
            self.run_cli(
                &["filesystem", "copy", &name_flag, &from, &parent],
                META_TIMEOUT_SECS,
            )
            .await?;
        }
        Ok(())
    }

    /// What `create_share_link` sends: `--password`, `--expiration` and
    /// `--role viewer|editor`. Existing links cannot be listed or revoked by
    /// id through this provider (removal goes by path).
    fn share_link_capabilities(&self) -> super::ShareLinkCapabilities {
        super::ShareLinkCapabilities {
            supports_expiration: true,
            supports_password: true,
            supports_permissions: true,
            available_permissions: vec!["view".into(), "edit".into()],
            supports_list_links: false,
            supports_revoke: false,
        }
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        let abs = self.resolve_path(path);
        let mut args: Vec<String> = vec!["sharing".into(), "set-url".into()];
        if let Some(ref pw) = options.password {
            // The password travels in the process arguments, and there is no
            // way around it today: `cli-drive@0.8.0+06e8c605` accepts it only
            // as `--password PASSWORD` for `sharing set-url`, with no stdin
            // form and no environment variable. Checked against the binary's
            // own help rather than assumed.
            //
            // **The window is up to three processes, not one.** `run_cli`
            // retries a transient failure, `for attempt in 0..=MAX_RETRIES`
            // with `MAX_RETRIES = 2`, and every attempt spawns the command
            // again with the same arguments.
            //
            // Who can read it is the host's business: on Linux
            // `/proc/<pid>/cmdline` is world readable unless `/proc` carries
            // `hidepid`. `redact_cli_args` covers the log line and not this.
            //
            // The day a CLI release adds a stdin or environment form, this is
            // the site to change, and the version above is what tells a
            // reader whether the check still holds.
            //
            // The warning below carries no arguments on purpose: an alert
            // about a leak that leaks would be the same species as the defect.
            tracing::warn!(
                target: "proton",
                "the share link password is passed to proton-drive as a command-line \
                 argument and is therefore visible in the local process list while the \
                 command runs, up to three times if it is retried. The Proton Drive CLI \
                 offers no other way to supply it."
            );
            args.push("--password".into());
            args.push(pw.clone());
        }
        let mut expires_at = None;
        if let Some(secs) = options.expires_in_secs {
            let day = share_expiration_day(chrono::Utc::now(), secs);
            args.push("--expiration".into());
            args.push(day.clone());
            expires_at = Some(day);
        }
        if let Some(ref perm) = options.permissions {
            let role = if perm == "edit" { "editor" } else { "viewer" };
            args.push("--role".into());
            args.push(role.into());
        }
        args.push(abs);
        args.push("-j".into());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let stdout = self.run_cli(&arg_refs, META_TIMEOUT_SECS).await?;
        let url = extract_url(&stdout).ok_or_else(|| {
            ProviderError::ParseError(format!(
                "Could not parse Proton share URL from: {}",
                stdout.trim()
            ))
        })?;
        Ok(ShareLinkResult {
            url,
            password: options.password,
            expires_at,
        })
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        let abs = self.resolve_path(path);
        self.run_cli(&["sharing", "remove-url", &abs], META_TIMEOUT_SECS)
            .await?;
        Ok(())
    }

    fn clone_for_list(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        Ok(Box::new(ProtonCliProvider {
            config: self.config.clone(),
            connected: self.connected,
            current_path: self.current_path.clone(),
            account_email: self.account_email.clone(),
            binary: self.binary.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_root_list() {
        let json = r#"[{"path":"/my-files"},{"path":"/trash"}]"#;
        let entries = parse_list_json(json, "/").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "my-files");
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].path, "/my-files");
    }

    #[test]
    fn parse_folder_list() {
        let json = r#"[
            {"name":{"ok":true,"value":"Photos"},"type":"folder","modificationTime":"2025-11-30T09:23:40.000Z"},
            {"name":{"ok":true,"value":"notes.txt"},"type":"file","totalStorageSize":1234,"mediaType":"text/plain"}
        ]"#;
        let entries = parse_list_json(json, "/my-files").unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].path, "/my-files/Photos");
        assert!(!entries[1].is_dir);
        assert_eq!(entries[1].size, 1234);
        assert_eq!(entries[1].mime_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn parse_prefers_claimed_size() {
        let json = r#"[{
            "name":{"ok":true,"value":"notes.txt"},
            "type":"file",
            "totalStorageSize":19447,
            "activeRevision":{"storageSize":19447,"claimedSize":12370}
        }]"#;
        let entries = parse_list_json(json, "/my-files").unwrap();
        assert_eq!(entries[0].size, 12370);
    }

    #[test]
    fn parse_prefers_claimed_mtime() {
        let json = r#"[{
            "name":{"ok":true,"value":"notes.txt"},
            "type":"file",
            "modificationTime":"2026-09-18T10:06:22.000Z",
            "activeRevision":{"claimedModificationTime":"2026-09-18T10:06:17.721Z","claimedSize":12}
        }]"#;
        let entries = parse_list_json(json, "/my-files").unwrap();
        assert_eq!(
            entries[0].modified.as_deref(),
            Some("2026-09-18T10:06:17.721Z")
        );
    }

    #[test]
    fn trash_path_uses_first_segment() {
        assert!(is_in_trash_path("/trash/dup.txt"));
        assert!(is_in_trash_path("/photos-trash/x"));
        assert!(!is_in_trash_path("/trashcan/x"));
        assert!(!is_in_trash_path("/my-files/trash"));
    }

    #[test]
    fn a_profile_cannot_choose_the_binary_that_runs() {
        let _env = crate::test_env::lock();
        let mut extra = std::collections::HashMap::new();
        extra.insert("proton_cli_path".to_string(), "/tmp/evil".to_string());
        let config = super::super::ProviderConfig {
            name: "p".to_string(),
            provider_type: super::super::ProviderType::Proton,
            host: "drive.proton.me".to_string(),
            port: None,
            username: None,
            password: None,
            initial_path: None,
            extra,
        };
        let parsed = ProtonConfig::from_provider_config(&config).unwrap();
        assert_eq!(
            parsed.binary_path, None,
            "a profile option picked the executable"
        );
    }

    #[test]
    fn a_share_expiring_later_today_is_sent_as_tomorrow() {
        use chrono::TimeZone;
        let now = chrono::Utc.with_ymd_and_hms(2026, 9, 22, 20, 0, 0).unwrap();
        // One hour from now is still 22 September: a link "expiring" on the
        // day it is made would be dead at once or refused.
        assert_eq!(share_expiration_day(now, 3600), "2026-09-23");
        assert_eq!(share_expiration_day(now, 3 * 86_400), "2026-09-25");
    }

    #[test]
    fn share_link_capabilities_match_what_create_share_link_sends() {
        let p = ProtonCliProvider::new(ProtonConfig {
            display_name: "t".into(),
            binary_path: None,
        });
        let caps = p.share_link_capabilities();
        assert!(caps.supports_password);
        assert!(caps.supports_expiration);
        assert!(caps.supports_permissions);
        assert_eq!(
            caps.available_permissions,
            vec!["view".to_string(), "edit".to_string()]
        );
    }

    #[test]
    fn redacts_share_password() {
        let args = ["sharing", "set-url", "--password", "secret", "/my-files/a"];
        let redacted = redact_cli_args(&args);
        assert_eq!(redacted[3], "***");
        assert!(redacted.iter().all(|s| s != "secret"));
    }

    #[test]
    fn resolve_path_normalizes_dotdot() {
        let mut p = ProtonCliProvider::new(ProtonConfig {
            display_name: "t".into(),
            binary_path: None,
        });
        p.current_path = "/my-files/a".into();
        assert_eq!(p.resolve_path("../b"), "/my-files/b");
        assert_eq!(p.resolve_path("sub/../other"), "/my-files/a/other");
        assert_eq!(p.resolve_path("/my-files/x/../y"), "/my-files/y");
    }

    #[test]
    fn parent_and_join() {
        assert_eq!(parent_of("/my-files/a/b"), "/my-files/a");
        assert_eq!(parent_of("/my-files"), "/");
        assert_eq!(join_path("/my-files", "x"), "/my-files/x");
        assert_eq!(basename("/my-files/x"), "x");
    }

    #[test]
    fn classify_login() {
        match classify_cli_error("You need to login first") {
            ProviderError::AuthenticationFailed(_) => {}
            other => panic!("{other:?}"),
        }
    }
}

/// Shim tests for upload/download/purge argv. Unix-only: the shim is a Python
/// script. These are the sequences Fable measured against the real CLI.
#[cfg(all(test, unix))]
mod cli_sequence_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    /// Put a `proton-drive` stand-in in `dir` and return its path.
    ///
    /// The stand-in is a checked-in executable reached through a symlink, and
    /// the tests must not go back to writing it themselves. Writing a file and
    /// then executing it races with every other thread that spawns a process:
    /// a child forked while the write handle is open inherits that handle until
    /// its own exec, and executing a file that anyone holds open for writing
    /// fails with ETXTBSY ("Text file busy"). Measured on this module before the
    /// change: 2 red runs out of 12 of `cargo test --lib -- proton`, in
    /// different tests each time. A symlink opens no handle on the target.
    fn link_shim(dir: &Path) -> PathBuf {
        let target =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proton_drive_shim.py");
        let mode = std::fs::metadata(&target)
            .unwrap_or_else(|e| panic!("shim fixture missing at {}: {e}", target.display()))
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "shim fixture {} lost its executable bit (mode {mode:o})",
            target.display()
        );
        let shim = dir.join("proton-drive");
        std::os::unix::fs::symlink(&target, &shim).unwrap();
        shim
    }

    fn read_argv(dir: &Path) -> Vec<Vec<String>> {
        let log = std::fs::read_to_string(dir.join("argv.log")).unwrap_or_default();
        log.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    // Synchronous on purpose: the environment lock is a std mutex, and the
    // variable must stay set for the whole child spawn, so the runtime runs
    // inside the lock instead of the lock living across an await.
    #[test]
    fn the_child_receives_no_aeroftp_variables() {
        let _env = crate::test_env::lock();
        std::env::set_var("AEROFTP_TEST_SECRET_FOR_PROTON", "do-not-leak");
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let p = provider(&shim);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _ = runtime.block_on(p.run_cli(&["account", "info", "-j"], 5));
        std::env::remove_var("AEROFTP_TEST_SECRET_FOR_PROTON");
        let log = std::fs::read_to_string(dir.path().join("env.log")).unwrap_or_default();
        assert!(!log.is_empty(), "the shim did not run");
        for line in log.lines() {
            assert_eq!(line, "[]", "AeroFTP variables reached proton-drive: {line}");
        }
    }

    fn provider(shim: &Path) -> ProtonCliProvider {
        let mut p = ProtonCliProvider::new(ProtonConfig {
            display_name: "t".into(),
            binary_path: Some(shim.to_string_lossy().to_string()),
        });
        p.connected = true;
        p
    }

    fn workdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("proton_shim_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn verbs<'a>(argv: &'a [Vec<String>], sub: &str) -> Vec<&'a Vec<String>> {
        argv.iter()
            .filter(|a| {
                a.first().map(|s| s.as_str()) == Some("filesystem")
                    && a.get(1).map(|s| s.as_str()) == Some(sub)
            })
            .collect()
    }

    #[tokio::test]
    async fn upload_stages_dest_basename_instead_of_renaming_remote() {
        let dir = workdir();
        let shim = link_shim(&dir);
        let local = dir.join("notes.txt");
        std::fs::write(&local, "NEW-INCOMING-CONTENT").unwrap();
        let mut p = provider(&shim);
        p.upload(
            local.to_str().unwrap(),
            "/my-files/scratch/notes (1).txt",
            None,
        )
        .await
        .unwrap();
        let argv = read_argv(&dir);
        let uploads = verbs(&argv, "upload");
        assert_eq!(uploads.len(), 1, "expected one upload, got {argv:?}");
        let local_arg = uploads[0].iter().rev().nth(1).expect("upload local path");
        let staged_name = Path::new(local_arg)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        assert_eq!(
            staged_name, "notes (1).txt",
            "must upload a file named as the dest, not the local basename: {uploads:?}"
        );
        assert!(
            verbs(&argv, "trash").is_empty(),
            "must not trash a pre-existing dest: {argv:?}"
        );
        assert!(
            verbs(&argv, "rename").is_empty(),
            "must not rename a just-uploaded local basename: {argv:?}"
        );
        let f_strat = uploads[0]
            .windows(2)
            .find(|w| w[0] == "-f")
            .map(|w| w[1].as_str());
        assert_eq!(
            f_strat,
            Some("create-new-revision"),
            "overwrite must keep the node uid: {uploads:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn download_does_not_remove_local_sibling_matching_remote_name() {
        let dir = workdir();
        let shim = link_shim(&dir);
        let dest_dir = dir.join("dl");
        std::fs::create_dir_all(&dest_dir).unwrap();
        let precious = dest_dir.join("report.txt");
        std::fs::write(&precious, "LOCAL-PRECIOUS-REPORT").unwrap();
        let dest = dest_dir.join("report (1).txt");
        let mut p = provider(&shim);
        p.download("/my-files/scratch/report.txt", dest.to_str().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&precious).unwrap(),
            "LOCAL-PRECIOUS-REPORT",
            "local file matching the remote basename must survive"
        );
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "REMOTE-CONTENT",
            "download must land on the requested dest path"
        );
        let argv = read_argv(&dir);
        let downloads = verbs(&argv, "download");
        assert_eq!(downloads.len(), 1, "{argv:?}");
        let cli_dest = Path::new(downloads[0].last().unwrap());
        assert_ne!(
            cli_dest,
            dest_dir.as_path(),
            "must not download into the user's folder: {downloads:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn delete_permanent_refuses_when_trash_name_is_ambiguous() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("trash.json"),
            r#"[{"name":{"ok":true,"value":"dup.txt"},"uid":"UID-OLD","type":"file"},{"name":{"ok":true,"value":"dup.txt"},"uid":"UID-CAPTURED","type":"file"}]"#,
        )
        .unwrap();
        let mut p = provider(&shim);
        let err = p
            .delete_permanent("/my-files/scratch/dup.txt")
            .await
            .expect_err("ambiguous trash must not report a successful purge");
        let msg = err.to_string();
        assert!(
            msg.contains("not purged") && msg.contains("2"),
            "honest refuse, got: {msg}"
        );
        let argv = read_argv(&dir);
        assert!(
            verbs(&argv, "delete").is_empty(),
            "must not purge by name when trash is ambiguous: {argv:?}"
        );
        assert_eq!(verbs(&argv, "trash").len(), 1, "{argv:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn delete_permanent_purges_when_unique_trash_name_matches_uid() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("trash.json"),
            r#"[{"name":{"ok":true,"value":"dup.txt"},"uid":"UID-CAPTURED","type":"file"}]"#,
        )
        .unwrap();
        let mut p = provider(&shim);
        let purged = p
            .delete_permanent("/my-files/scratch/dup.txt")
            .await
            .unwrap();
        assert!(purged);
        let argv = read_argv(&dir);
        let deletes = verbs(&argv, "delete");
        assert_eq!(deletes.len(), 1, "{argv:?}");
        assert_eq!(
            deletes[0].last().map(|s| s.as_str()),
            Some("/trash/dup.txt")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn mkdir_rename_copy_put_dash_guard_before_bare_names() {
        let dir = workdir();
        let shim = link_shim(&dir);
        let mut p = provider(&shim);
        p.mkdir("/my-files/scratch/-x").await.unwrap();
        p.rename("/my-files/scratch/a.txt", "/my-files/scratch/-y")
            .await
            .unwrap();
        p.server_copy("/my-files/scratch/a.txt", "/my-files/scratch/-z")
            .await
            .unwrap();
        let argv = read_argv(&dir);
        let mkdir = &verbs(&argv, "create-folder")[0];
        assert!(
            mkdir.windows(2).any(|w| w[0] == "--" && w[1] == "-x"),
            "create-folder needs -- before a dash name: {mkdir:?}"
        );
        let rename = &verbs(&argv, "rename")[0];
        assert!(
            rename.windows(2).any(|w| w[0] == "--" && w[1] == "-y"),
            "rename needs -- before a dash name: {rename:?}"
        );
        let copy = &verbs(&argv, "copy")[0];
        assert!(
            copy.iter().any(|a| a == "--name=-z"),
            "copy -n with a dash name needs --name=-z (measured): {copy:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
