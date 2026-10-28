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

use super::bulk::{
    command_arg_bytes, plain_bulk_name, BulkProgress, BulkTransferItem, BulkTransferOutcome,
    BULK_COMMAND_RESERVE_BYTES, MAX_BULK_BYTES, MAX_BULK_FILES, MAX_BULK_PATH_BYTES,
};
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

/// Which client a Proton Drive profile connects through, from the profile
/// option `proton_mode`. A profile without it is a CLI profile: every profile
/// saved before the native client existed keeps connecting as it did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProtonConnectionMode {
    /// Proton's official `proton-drive` CLI ([`ProtonCliProvider`]).
    #[default]
    Cli,
    /// AeroFTP's native client (`proton_native`, cargo feature
    /// `proton-native`).
    Native,
}

#[derive(Debug, Clone)]
pub struct ProtonConfig {
    pub display_name: String,
    pub binary_path: Option<String>,
    pub connection_mode: ProtonConnectionMode,
    /// The saved profile the native client keeps its session with.
    pub profile_id: Option<String>,
    /// The account the profile names (its username field), if any.
    pub username: Option<String>,
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
        // Unlike MEGA, a missing mode is never read as native: a profile
        // gets the native client only when it asks for it.
        let connection_mode = match config.extra.get("proton_mode").map(String::as_str) {
            None | Some("cli") => ProtonConnectionMode::Cli,
            Some("native") => ProtonConnectionMode::Native,
            Some(other) => {
                return Err(ProviderError::InvalidConfig(format!(
                    "Invalid Proton Drive connection mode: {other}"
                )));
            }
        };
        let profile_id = config
            .extra
            .get("profile_id")
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty());
        let username = config
            .username
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        Ok(Self {
            display_name,
            binary_path,
            connection_mode,
            profile_id,
            username,
        })
    }
}

pub struct ProtonCliProvider {
    config: ProtonConfig,
    connected: bool,
    current_path: String,
    account_email: Option<String>,
    binary: String,
    /// Claimed sizes, from the fresh listing of the latest bulk download, of
    /// the files that bulk handed back to the single-file path, by absolute
    /// remote path. The single-file `download` holds such a file to the same
    /// size; an entry stays for every retry, goes once the file is
    /// published, and expires after [`BULK_CLAIMED_SIZE_TTL`].
    bulk_claimed_sizes: std::collections::HashMap<String, (u64, std::time::Instant)>,
    trash_listing: Option<(String, Vec<RemoteEntry>)>,
}

/// How long a claimed size from a bulk's listing still describes the file a
/// single-file retry downloads.
const BULK_CLAIMED_SIZE_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// A staged download whose size is not the one its uploader claimed.
fn claimed_size_mismatch(name: &str, claimed: u64, actual: u64) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "{name}: downloaded {actual} bytes, but its uploader claimed {claimed}; not published"
        ),
    )
}

impl ProtonCliProvider {
    /// The CLI walks/decrypts a parent listing for every path spelled by name,
    /// serially, before starting its download queue. A fresh listing binds only
    /// the selected names to UIDs; a /my-files/UID lookup skips those repeated
    /// walks without recursively reading unselected siblings. Other sections
    /// retain their established path resolution. The same listing gives the
    /// size each selected file's uploader claimed, when there is one, for the
    /// publication check.
    async fn bulk_download_sources(
        &mut self,
        remotes: &[String],
        destination_bytes: usize,
    ) -> (Vec<String>, std::collections::HashMap<String, u64>) {
        let mut claimed = std::collections::HashMap::new();
        let Some(first) = remotes.first() else {
            return (Vec::new(), claimed);
        };
        let parent = parent_of(first);
        if first_segment(first) != "my-files" || remotes.iter().any(|r| parent_of(r) != parent) {
            return (remotes.to_vec(), claimed);
        }
        let Ok(entries) = self.list(&parent).await else {
            return (remotes.to_vec(), claimed);
        };
        let mut by_name = std::collections::HashMap::new();
        let mut duplicates = std::collections::HashSet::new();
        for entry in entries {
            if entry.is_dir {
                continue;
            }
            if by_name.contains_key(&entry.name) {
                duplicates.insert(entry.name.clone());
            }
            if let Some(size) = entry
                .metadata
                .get("claimed_size")
                .and_then(|size| size.parse::<u64>().ok())
            {
                claimed.insert(entry.name.clone(), size);
            }
            by_name.insert(entry.name, entry.metadata.get("uid").cloned());
        }
        claimed.retain(|name, _| !duplicates.contains(name));
        let candidate: Vec<_> = remotes
            .iter()
            .map(|remote| {
                let name = basename(remote);
                if duplicates.contains(&name) {
                    return remote.clone();
                }
                by_name
                    .get(&name)
                    .and_then(|uid| uid.as_deref())
                    .filter(|uid| valid_cli_node_uid(uid))
                    .map(|uid| format!("/my-files/{uid}"))
                    .unwrap_or_else(|| remote.clone())
            })
            .collect();
        // Old Proton volume/link IDs can be over 200 bytes. Never let an
        // expanded UID list exceed the conservative command-line budget.
        if candidate.iter().map(|p| p.len() + 3).sum::<usize>() + destination_bytes + 512
            > MAX_BULK_PATH_BYTES
        {
            (remotes.to_vec(), claimed)
        } else {
            (candidate, claimed)
        }
    }

    /// Unlike `run_cli`, a mutating multi-file call is NEVER replayed. An
    /// interrupted process may have committed an arbitrary subset of revisions.
    async fn run_bulk_once(&self, args: &[String]) -> Result<std::process::Output, ProviderError> {
        if self.binary.is_empty() || !binary_exists(&self.binary) {
            return Err(ProviderError::InvalidConfig(MISSING_CLI.to_string()));
        }
        let mut cmd = Command::new(&self.binary);
        cmd.args(args).kill_on_drop(true);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("AEROFTP_") {
                cmd.env_remove(&key);
            }
        }
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        match tokio::time::timeout(
            std::time::Duration::from_secs(TRANSFER_TIMEOUT_SECS),
            cmd.output(),
        )
        .await
        {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(ProviderError::IoError(error)),
            Err(_) => Err(ProviderError::Timeout),
        }
    }

    async fn transfer_bulk(
        &mut self,
        items: &[BulkTransferItem],
        upload: bool,
        progress: Option<BulkProgress>,
    ) -> Result<Vec<BulkTransferOutcome>, ProviderError> {
        let mut claimed = std::collections::HashMap::new();
        let outcomes = self
            .transfer_bulk_once(items, upload, progress, &mut claimed)
            .await?;
        if !upload {
            // Only what goes back to the single-file path is held to the
            // listing's claimed size there: a file this bulk published, or
            // failed for good, keeps the ordinary single-file download.
            let now = std::time::Instant::now();
            self.bulk_claimed_sizes = items
                .iter()
                .zip(&outcomes)
                .filter(|(_, outcome)| super::bulk::goes_back_to_single_file(outcome))
                .filter_map(|(item, _)| {
                    let remote = self.resolve_path(&item.remote_path);
                    let size = *claimed.get(&remote)?;
                    Some((remote, (size, now)))
                })
                .collect();
        }
        Ok(outcomes)
    }

    /// One bulk; `claimed` receives the listed claimed sizes of its items by
    /// absolute remote path.
    async fn transfer_bulk_once(
        &mut self,
        items: &[BulkTransferItem],
        upload: bool,
        progress: Option<BulkProgress>,
        claimed: &mut std::collections::HashMap<String, u64>,
    ) -> Result<Vec<BulkTransferOutcome>, ProviderError> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        if self.binary.is_empty() || !binary_exists(&self.binary) {
            return Err(ProviderError::InvalidConfig(MISSING_CLI.to_string()));
        }
        // Admission. An item that cannot join this native call is handed back
        // as NotAttempted, so the caller's single-file path gives it the same
        // treatment it had before bulks existed (a renamed or changed source,
        // a name the CLI would map, a sibling past the bounds). Nothing here
        // has reached the wire.
        let mut outcomes: Vec<BulkTransferOutcome> = items
            .iter()
            .map(|_| BulkTransferOutcome::NotAttempted("Not admitted to the native bulk".into()))
            .collect();
        let mut bytes = 0u64;
        let mut arg_bytes = BULK_COMMAND_RESERVE_BYTES;
        let mut names = std::collections::HashSet::new();
        let mut remotes = Vec::with_capacity(items.len());
        let mut locals = Vec::with_capacity(items.len());
        let mut valid_indices = Vec::with_capacity(items.len());
        let mut group_parent: Option<String> = None;
        for (index, item) in items.iter().enumerate() {
            let remote = self.resolve_path(&item.remote_path);
            let Some(name) = plain_bulk_name(&remote) else {
                continue;
            };
            let local = Path::new(&item.local_path);
            if local.file_name().and_then(|s| s.to_str()) != Some(name)
                || names.contains(&name.to_lowercase())
            {
                continue;
            }
            let parent = if upload {
                parent_of(&remote)
            } else {
                local
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_string_lossy()
                    .into_owned()
            };
            let item_args = command_arg_bytes(&item.local_path, &remote);
            if group_parent.as_ref().is_some_and(|p| p != &parent)
                || valid_indices.len() >= MAX_BULK_FILES
                || bytes.saturating_add(item.size) > MAX_BULK_BYTES
                || arg_bytes.saturating_add(item_args) > MAX_BULK_PATH_BYTES
            {
                continue;
            }
            if upload {
                // A source that changed since planning, or is not a plain
                // file, keeps the single-file path, which sends what is there
                // now (or reports exactly why it cannot).
                let source = (|| {
                    ensure_parent_writable(&parent_of(&remote))?;
                    let metadata =
                        std::fs::symlink_metadata(local).map_err(ProviderError::IoError)?;
                    if !metadata.is_file() || metadata.len() != item.size {
                        return Err(ProviderError::InvalidPath(
                            "Source changed or is not a regular file".into(),
                        ));
                    }
                    literal_local_path_for_cli(&normalize_local_path_for_cli(&item.local_path))
                })();
                match source {
                    Ok(path) => locals.push(path),
                    Err(_) => continue,
                }
            }
            names.insert(name.to_lowercase());
            group_parent = Some(parent);
            bytes = bytes.saturating_add(item.size);
            arg_bytes = arg_bytes.saturating_add(item_args);
            valid_indices.push(index);
            remotes.push(remote);
        }
        if valid_indices.is_empty() {
            return Ok(outcomes);
        }
        let parent = group_parent.expect("non-empty bulk has parent");
        // TempDir owns cleanup even when cancellation drops this future. A
        // download shares its destination device so publication is a rename.
        let staging = if upload {
            None
        } else {
            tokio::fs::create_dir_all(&parent)
                .await
                .map_err(ProviderError::IoError)?;
            Some(
                tempfile::Builder::new()
                    .prefix(".aeroftp-proton-bulk-")
                    .tempdir_in(&parent)
                    .map_err(ProviderError::IoError)?,
            )
        };
        if let Some(dir) = staging.as_ref() {
            // Names this filesystem folds together (case, Unicode form) would
            // overwrite one another in the staging folder: those go back to
            // the single-file path, the rest stay in the bulk.
            let aliased = reserve_bulk_download_names(dir.path(), &remotes)?;
            (valid_indices, remotes) = valid_indices
                .into_iter()
                .zip(std::mem::take(&mut remotes))
                .zip(aliased)
                .filter_map(|(kept, aliased)| (!aliased).then_some(kept))
                .unzip();
            if remotes.is_empty() {
                return Ok(outcomes);
            }
        }
        let mut args: Vec<String> = [
            "filesystem",
            if upload { "upload" } else { "download" },
            "-j",
            "-f",
            if upload {
                "create-new-revision"
            } else {
                "remove"
            },
            "-d",
            "merge",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let mut claimed_sizes = std::collections::HashMap::new();
        args.extend(if upload {
            locals
        } else {
            let (sources, claimed_by_name) = self
                .bulk_download_sources(
                    &remotes,
                    staging
                        .as_ref()
                        .expect("download has staging")
                        .path()
                        .as_os_str()
                        .len(),
                )
                .await;
            let listed_parent = parent_of(&remotes[0]);
            for item in items {
                let remote = self.resolve_path(&item.remote_path);
                if parent_of(&remote) == listed_parent {
                    if let Some(&size) = claimed_by_name.get(&basename(&remote)) {
                        claimed.insert(remote, size);
                    }
                }
            }
            claimed_sizes = claimed_by_name;
            sources
        });
        args.push(if let Some(dir) = staging.as_ref() {
            literal_local_path_for_cli(&dir.path().to_string_lossy())?
        } else {
            parent
        });
        if args.iter().map(|arg| arg.len() + 3).sum::<usize>() > MAX_BULK_PATH_BYTES {
            return Ok(outcomes);
        }
        for &index in &valid_indices {
            if let Some(ref cb) = progress {
                cb(index, 0, items[index].size);
            }
        }
        // From here on the native program may have written: a download is
        // still only published from private staging, an upload may be on
        // the server whatever the process says.
        let output = match self.run_bulk_once(&args).await {
            Ok(output) => output,
            Err(error) => {
                for &index in &valid_indices {
                    outcomes[index] = unproven_bulk_item(
                        upload,
                        format!("Bulk interrupted; remote state unknown: {error}"),
                    );
                }
                return Ok(outcomes);
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut results = bulk_summary_outcomes(&stdout, &remotes, output.status.success(), upload);
        for (position, result) in results.iter_mut().enumerate() {
            let index = valid_indices[position];
            if !matches!(result, BulkTransferOutcome::Completed) {
                continue;
            }
            if let Some(dir) = staging.as_ref() {
                let name = basename(&remotes[position]);
                let staged = dir.path().join(&name);
                // The SDK behind the CLI checks the SHA-256 of every block and
                // the manifest signature over them. On top of that, a file
                // must have the size its uploader claimed when the fresh
                // listing has one. The planned size is not compared: without
                // a claimed size Proton lists the encrypted size, which a
                // correct download never matches.
                let publish = match tokio::fs::symlink_metadata(&staged).await {
                    Ok(metadata)
                        if metadata.is_file()
                            && claimed_sizes
                                .get(&name)
                                .is_some_and(|&claimed| claimed != metadata.len()) =>
                    {
                        Err(claimed_size_mismatch(
                            &name,
                            claimed_sizes[&name],
                            metadata.len(),
                        ))
                    }
                    Ok(metadata) if metadata.is_file() => {
                        tokio::fs::rename(&staged, &items[index].local_path).await
                    }
                    Ok(_) => Err(std::io::Error::other(
                        "The native CLI wrote something other than a regular file",
                    )),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "The native CLI skipped this file and wrote nothing (Proton Docs and Sheets are not downloadable as files)",
                        ))
                    }
                    Err(error) => Err(error),
                };
                if let Err(error) = publish {
                    *result = BulkTransferOutcome::Failed(ProviderError::IoError(error));
                    continue;
                }
            }
            if let Some(ref cb) = progress {
                cb(index, items[index].size, items[index].size);
            }
        }
        for (index, result) in valid_indices.into_iter().zip(results) {
            outcomes[index] = result;
        }
        Ok(outcomes)
    }

    pub fn new(config: ProtonConfig) -> Self {
        let binary = resolve_proton_drive(config.binary_path.as_deref());
        Self {
            config,
            connected: false,
            current_path: START_DIR.to_string(),
            account_email: None,
            binary,
            bulk_claimed_sizes: std::collections::HashMap::new(),
            trash_listing: None,
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
            // A transfer that failed for some of its items says only "N
            // item(s) failed to upload" on stderr; why each one failed is in
            // the summary on stdout. Classify from those reasons. A full
            // drive is not transient and returns here, so it is not retried
            // as an unknown server error. A timeout or a 429 on an item uses
            // the same retry limit as any other transient CLI error, and the
            // last attempt still returns the classified summary.
            if let Some(failures) = transfer_failures(&stdout) {
                let classified = classify_transfer_failures(&failures);
                if is_transient(&failure_reasons(&failures)) && attempt < MAX_RETRIES {
                    last_err = classified;
                    continue;
                }
                return Err(classified);
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

    /// `filesystem move` of `path` into the folder `folder`, under its name.
    async fn move_into(&self, path: &str, folder: &str) -> Result<(), ProviderError> {
        self.run_cli(&["filesystem", "move", path, folder], META_TIMEOUT_SECS)
            .await
            .map(|_| ())
    }

    /// `filesystem rename` of `path` to `name`, in its own folder.
    async fn rename_in_place(&self, path: &str, name: &str) -> Result<(), ProviderError> {
        self.run_cli(
            &["filesystem", "rename", path, "--", name],
            META_TIMEOUT_SECS,
        )
        .await
        .map(|_| ())
    }

    fn resolve_path(&self, path: &str) -> String {
        // A path is an exact name, including whitespace-only leaves.
        let p = path;
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

/// Budget for the status probe of the connection form. The CLI starts a new
/// process per command, so this bounds a slow start, not a network transfer.
const STATUS_PROBE_TIMEOUT_SECS: u64 = 20;

/// Whether the Proton Drive CLI is installed and signed in, for the status
/// banner of the connection form: `(installed, signed_in)`. Resolves the
/// binary the same way a connection does, and asks for the root listing, the
/// same first call `connect` makes, so "signed in" means a connect would pass
/// that step.
pub async fn cli_session_status() -> (bool, bool) {
    let custom = std::env::var("AEROFTP_PROTON_CLI")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let binary = resolve_proton_drive(custom.as_deref());
    if !binary_exists(&binary) {
        return (false, false);
    }
    let probe = ProtonCliProvider::new(ProtonConfig {
        display_name: "status".into(),
        binary_path: Some(binary),
        connection_mode: ProtonConnectionMode::Cli,
        profile_id: None,
        username: None,
    });
    let signed_in = probe
        .run_cli(
            &["filesystem", "list", "/", "-j"],
            STATUS_PROBE_TIMEOUT_SECS,
        )
        .await
        .is_ok();
    (true, signed_in)
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

/// proton-drive expands a local path holding `*`, `?`, `[` or `{` as a glob
/// (`cli/src/commands/fileSystem/localPath.ts`), for the upload source and the
/// download folder alike, so `a [1].txt` matched nothing ("No paths
/// matched"). Each of those, and `(` (an extglob opener once the path is a
/// glob), goes inside a one-character class, which matches it literally on
/// every platform; a path without them is passed as it is. Remote paths are
/// never globbed. On Unix a backslash in a name has no escape the CLI's glob
/// accepts, so such a path is refused with the reason instead of failing on
/// a pattern.
fn literal_local_path_for_cli(path: &str) -> Result<String, ProviderError> {
    if !path.contains(['*', '?', '[', '{']) {
        return Ok(path.to_string());
    }
    #[cfg(not(windows))]
    if path.contains('\\') {
        return Err(ProviderError::InvalidPath(format!(
            "{path}: proton-drive reads a local path holding * ? [ or {{ as a pattern, and a \
             backslash in it cannot be escaped; rename the file or use the API mode"
        )));
    }
    let mut literal = String::with_capacity(path.len() + 8);
    for c in path.chars() {
        if matches!(c, '*' | '?' | '[' | '{' | '(') {
            literal.push('[');
            literal.push(c);
            literal.push(']');
        } else {
            literal.push(c);
        }
    }
    Ok(literal)
}

/// The name proton-drive gives a downloaded file
/// (`downloadPathValidation.ts::sanitizePathSegmentForLocalFilesystem`):
/// control characters and `< > : " | ? * \ /` become `_` on every platform,
/// `.` and `..` become `_` and `__`, and on Windows a reserved device name
/// gets a leading `_` and trailing dots and spaces become `_`.
fn cli_local_name(name: &str) -> String {
    let mut segment: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_control()
                || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/')
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    match segment.as_str() {
        "" | "." => return "_".to_string(),
        ".." => return "__".to_string(),
        _ => {}
    }
    if cfg!(windows) {
        let stem = segment.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && matches!(stem.as_bytes()[3], b'1'..=b'9'));
        if reserved {
            segment.insert(0, '_');
        }
        let kept = segment.trim_end_matches(['.', ' ']).len();
        let trailing = segment.len() - kept;
        segment.truncate(kept);
        segment.push_str(&"_".repeat(trailing));
    }
    segment
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
        if arg.starts_with("--password=") {
            out.push("--password=***".to_string());
            continue;
        }
        out.push((*arg).to_string());
    }
    out
}

/// The failed items of a `filesystem upload|download -j` summary, as (name,
/// reason) pairs: `{"failedItems":N,"failures":[{"name","error"}]}`, where
/// `error` is `<SDK error class>: <API message>`. `None` when `stdout` is not
/// such a summary, or lists no failure.
fn transfer_failures(stdout: &str) -> Option<Vec<(String, String)>> {
    let summary: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let failures: Vec<(String, String)> = summary
        .get("failures")?
        .as_array()?
        .iter()
        .map(|failure| {
            let field = |key: &str| {
                failure
                    .get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            (field("name"), field("error"))
        })
        .collect();
    (!failures.is_empty()).then_some(failures)
}

fn valid_cli_node_uid(uid: &str) -> bool {
    let valid_part = |part: &str| {
        let old = (88..=108).contains(&part.len());
        (part.len() == 22 || old)
            && part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || (old && c == b'='))
    };
    uid.split_once('~')
        .is_some_and(|(volume, link)| valid_part(volume) && valid_part(link))
}

/// Ask the actual destination filesystem, including its case/normalization
/// rules, which staged names it keeps apart. A name it folds into an earlier
/// one is flagged; the native transfer must not overwrite one selected sibling
/// with another. The probe files are removed again, so whatever the CLI then
/// reports as written is what it wrote.
fn reserve_bulk_download_names(dir: &Path, remotes: &[String]) -> Result<Vec<bool>, ProviderError> {
    let mut aliased = vec![false; remotes.len()];
    let mut created = Vec::with_capacity(remotes.len());
    let mut probe = || -> Result<(), ProviderError> {
        for (position, remote) in remotes.iter().enumerate() {
            let path = dir.join(basename(remote));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    drop(file);
                    created.push(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    aliased[position] = true;
                }
                Err(error) => return Err(ProviderError::IoError(error)),
            }
        }
        Ok(())
    };
    let probed = probe();
    for path in &created {
        let _ = std::fs::remove_file(path);
    }
    probed.map(|()| aliased)
}

/// What an item whose process outcome is unknown becomes. A download was only
/// ever written to private staging, so its single-file path may run; an upload
/// may already be a revision on the server and is never replayed.
fn unproven_bulk_item(upload: bool, message: String) -> BulkTransferOutcome {
    if upload {
        BulkTransferOutcome::Uncertain(message)
    } else {
        BulkTransferOutcome::NotAttempted(message)
    }
}

/// Only a complete, unambiguous JSON summary can attest successes. Failure
/// names are basenames in Proton's TransferQueue, hence the planner's unique
/// basename rule. Missing/malformed counters never turn into blanket success.
fn bulk_summary_outcomes(
    stdout: &str,
    remotes: &[String],
    exit_success: bool,
    upload: bool,
) -> Vec<BulkTransferOutcome> {
    let parse = || -> Option<Vec<BulkTransferOutcome>> {
        let summary: Value = serde_json::from_str(stdout.trim()).ok()?;
        let transferred = summary.get("transferredItems")?.as_u64()?;
        let skipped = summary.get("skippedItems")?.as_u64()?;
        let failed = summary.get("failedItems")?.as_u64()?;
        let failures = summary.get("failures")?.as_array()?;
        if transferred.checked_add(skipped)?.checked_add(failed)? != remotes.len() as u64
            || failed != failures.len() as u64
            || (!exit_success && failed == 0)
        {
            return None;
        }
        let mut by_name = std::collections::HashMap::new();
        for failure in failures {
            let name = failure.get("name")?.as_str()?;
            let error = failure.get("error")?.as_str()?;
            if !remotes.iter().any(|remote| basename(remote) == name)
                || by_name.insert(name, error).is_some()
            {
                return None;
            }
        }
        Some(
            remotes
                .iter()
                .map(|remote| match by_name.get(basename(remote).as_str()) {
                    Some(error) => BulkTransferOutcome::Failed(classify_transfer_failures(&[(
                        basename(remote),
                        (*error).to_string(),
                    )])),
                    None => BulkTransferOutcome::Completed,
                })
                .collect(),
        )
    };
    parse().unwrap_or_else(|| {
        remotes
            .iter()
            .map(|_| {
                unproven_bulk_item(
                    upload,
                    "Native CLI returned no complete per-file summary; remote state unknown"
                        .to_string(),
                )
            })
            .collect()
    })
}

/// `name: error` for each failed item, joined for the message the user reads.
fn failure_detail(failures: &[(String, String)]) -> String {
    failures
        .iter()
        .map(|(name, error)| format!("{name}: {error}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The error texts alone, which is what is classified and what is retried.
fn failure_reasons(failures: &[(String, String)]) -> String {
    failures
        .iter()
        .map(|(_, error)| error.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

/// The classified text, then the item list. The list stays in the message and
/// is not classified: `TransferError::from_provider` reads the whole string,
/// and a file called `Storage quota exceeded.txt` would otherwise be a full
/// drive that is never retried.
fn show_with_items(classified: &str, detail: &str) -> String {
    format!(
        "{classified}{}{detail}",
        super::types::UNCLASSIFIED_ITEM_LIST
    )
}

/// Classify a partly failed transfer by its reasons only: the item names stay
/// out of the match, so a file called `quota.txt` that failed for another
/// reason is not a full drive. The JSON carries no error code (the CLI prints
/// only the class and the API message), so a full drive is told by the
/// message, and only on the class the SDK gives an API refusal of input
/// (`ValidationError`): a local failure (`Error: ENOSPC ... <path>`) carries
/// the local path, where any word can appear.
fn classify_transfer_failures(failures: &[(String, String)]) -> ProviderError {
    let detail = failure_detail(failures);
    let reasons = failure_reasons(failures);
    if failures.iter().any(|(_, error)| is_storage_full(error)) {
        return ProviderError::ServerError(show_with_items(
            &format!("Storage quota exceeded ({reasons})"),
            &detail,
        ));
    }
    let shown = show_with_items(&reasons, &detail);
    match classify_cli_error(&reasons) {
        ProviderError::NotFound(_) => ProviderError::NotFound(shown),
        ProviderError::PermissionDenied(_) => ProviderError::PermissionDenied(shown),
        ProviderError::AlreadyExists(_) => ProviderError::AlreadyExists(shown),
        ProviderError::ServerError(_) => ProviderError::ServerError(shown),
        other => other,
    }
}

/// A Proton refusal for a full drive: API codes 200001, 200002, 200100 and
/// 200101, which the SDK raises as a plain `ValidationError` and the CLI's
/// JSON reduces to `ValidationError: <API message>`.
fn is_storage_full(error: &str) -> bool {
    let Some(message) = error.strip_prefix("ValidationError:") else {
        return false;
    };
    let lower = message.to_lowercase();
    lower.contains("quota")
        || lower.contains("insufficient space")
        || lower.contains("not enough space")
        || lower.contains("out of storage")
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

/// The CLI's `/` is not a folder: it lists the account sections (my-files,
/// devices, photos, trash, the shared views...). Nothing can be created in it,
/// and the sections themselves are fixed.
fn is_root_or_section(abs: &str) -> bool {
    parent_of(abs) == "/"
}

/// Refuses a write whose destination folder is the root, before the CLI runs
/// and answers with a bare `Path "/" is not supported`.
fn ensure_parent_writable(parent: &str) -> Result<(), ProviderError> {
    if parent == "/" {
        return Err(ProviderError::PermissionDenied(
            "The Proton Drive root only holds the account sections (my-files, devices, photos, trash and the shared views). Open my-files, or another section, to add files or folders.".to_string(),
        ));
    }
    Ok(())
}

/// Refuses renaming, moving, copying or deleting the root or a section.
fn ensure_not_section(abs: &str) -> Result<(), ProviderError> {
    if is_root_or_section(abs) {
        return Err(ProviderError::PermissionDenied(format!(
            "\"{}\" is a Proton Drive section: it cannot be renamed, moved, copied or deleted.",
            basename(abs)
        )));
    }
    Ok(())
}

/// Where a session starts and returns to: My Files, the folder a native
/// profile opens in, so that both modes open on the same files. The root of
/// this mode stays the list of sections, one `cd ..` away (Requested 79).
pub const START_DIR: &str = "/my-files";

/// Every section the CLI shows in the root. They always exist and none can be
/// created, renamed or removed.
const ROOT_SECTIONS: &[&str] = &[
    "/my-files",
    "/devices",
    "/photos",
    "/albums",
    "/trash",
    "/photos-trash",
    "/shared-by-me",
    "/shared-with-me",
    "/photos-shared-by-me",
    "/photos-shared-with-me",
];

/// Sections that hold the account's own storage, the ones a used-storage scan
/// of the root counts. Left out: `shared-by-me` and `photos-shared-by-me`
/// repeat items already under my-files and photos, `shared-with-me` and
/// `photos-shared-with-me` are other people's files, `albums` groups photos
/// already under photos.
const OWN_STORAGE_SECTIONS: &[&str] = &[
    "/my-files",
    "/devices",
    "/photos",
    "/trash",
    "/photos-trash",
];

/// A section the CLI lists in the root but cannot open: measured with 0.8.0,
/// `albums` and `photos` answer `Path type ... is not supported` and
/// `photos-shared-by-me` fails with `TypeError: Invalid photo node type`.
fn explain_unlistable(path: &str, err: ProviderError) -> ProviderError {
    let text = err.to_string().to_lowercase();
    if text.contains("is not supported") || text.contains("invalid photo node type") {
        return ProviderError::NotSupported(format!(
            "The Proton Drive CLI cannot open \"{}\" yet. Use the Proton Drive web or mobile app for photos and albums.",
            first_segment(path)
        ));
    }
    err
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
    stage_local_as_dest_in(
        local_path,
        dest_name,
        &crate::util::scratch::scratch_dir_path(),
    )
}

fn stage_local_as_dest_in(
    local_path: &str,
    dest_name: &str,
    scratch_root: &Path,
) -> Result<(PathBuf, PathBuf), ProviderError> {
    let len = std::fs::metadata(local_path)
        .map_err(ProviderError::IoError)?
        .len();
    std::fs::create_dir_all(scratch_root).map_err(ProviderError::IoError)?;
    crate::util::scratch::protect_scratch_process(scratch_root).map_err(ProviderError::IoError)?;
    let staging = scratch_root.join(format!("aeroftp_proton_up_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&staging).map_err(ProviderError::IoError)?;
    let staged = staging.join(dest_name);
    match std::fs::hard_link(local_path, &staged) {
        Ok(()) => Ok((staging, staged)),
        Err(_) => {
            // A link needs no second payload. Only the copy fallback needs
            // enough free space for the file and the scratch headroom.
            let copy = crate::util::scratch::scratch_dir_with_room(&staging, len)
                .map_err(ProviderError::TransferFailed)
                .and_then(|_| std::fs::copy(local_path, &staged).map_err(ProviderError::IoError));
            match copy {
                Ok(_) => Ok((staging, staged)),
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&staging);
                    Err(e)
                }
            }
        }
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
    // The size the uploader claimed, apart from the storage-size fallback
    // above: only this one can be compared with a downloaded file.
    if let Some(claimed) = value
        .get("activeRevision")
        .and_then(|r| r.get("claimedSize"))
        .and_then(|v| v.as_u64())
    {
        metadata.insert("claimed_size".to_string(), claimed.to_string());
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
        // Official CLI 0.9.0 returns the link alongside its access policy.
        if let Some(url) = value.pointer("/urlAccess/url").and_then(Value::as_str) {
            if url.starts_with("https://") {
                return Some(url.to_string());
            }
        }
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

impl ProtonCliProvider {
    async fn capture_uid(&mut self, abs: &str) -> Result<String, ProviderError> {
        let stdout = self
            .run_cli(&["filesystem", "info", abs, "-j"], META_TIMEOUT_SECS)
            .await?;
        let entry = parse_info_json(&stdout, &parent_of(abs))?;
        entry_uid(&entry)
    }

    async fn purge_trash_by_name(
        &mut self,
        trash_root: &str,
        name: &str,
        captured_uid: Option<&str>,
        use_snapshot: bool,
    ) -> Result<bool, ProviderError> {
        if use_snapshot && !matches!(&self.trash_listing, Some((root, _)) if root == trash_root) {
            let listed = self
                .run_cli(&["filesystem", "list", trash_root, "-j"], META_TIMEOUT_SECS)
                .await?;
            self.trash_listing = Some((
                trash_root.to_string(),
                parse_list_json(&listed, trash_root)?,
            ));
        }
        let entries = match (&self.trash_listing, use_snapshot) {
            (Some((_, entries)), true) => entries.clone(),
            _ => {
                let listed = self
                    .run_cli(&["filesystem", "list", trash_root, "-j"], META_TIMEOUT_SECS)
                    .await?;
                parse_list_json(&listed, trash_root)?
            }
        };
        let matches: Vec<RemoteEntry> = entries
            .into_iter()
            .filter(|entry| entry.name == name)
            .collect();
        let uid_matches = match captured_uid {
            Some(uid) => matches
                .iter()
                .filter(|entry| entry.metadata.get("uid").map(String::as_str) == Some(uid))
                .count(),
            None => matches.len(),
        };
        if matches.len() != 1 || uid_matches != 1 {
            return Err(ProviderError::ServerError(format!(
                "Moved to trash, not purged: {} items in trash have this name and {} match the captured UID. Proton Drive CLI 0.9.0 resolves trash paths by name only; refusing an ambiguous purge",
                matches.len(),
                uid_matches
            )));
        }

        let purged_uid = matches[0].metadata.get("uid").cloned();
        let purge = join_path(trash_root, name);
        self.run_cli(&["filesystem", "delete", &purge], META_TIMEOUT_SECS)
            .await?;
        if use_snapshot {
            if let Some((_, entries)) = &mut self.trash_listing {
                entries.retain(|entry| entry.metadata.get("uid") != purged_uid.as_ref());
            }
        }
        Ok(true)
    }
}

fn entry_uid(entry: &RemoteEntry) -> Result<String, ProviderError> {
    let uid = entry
        .metadata
        .get("uid")
        .ok_or_else(|| ProviderError::ParseError("Proton CLI node metadata has no UID".into()))?;
    if !valid_cli_node_uid(uid) {
        return Err(ProviderError::ParseError(
            "Proton CLI node metadata has an invalid UID".into(),
        ));
    }
    Ok(uid.clone())
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

    fn used_scan_roots(&self, root: &str) -> Vec<String> {
        if normalize_abs(root) == "/" {
            OWN_STORAGE_SECTIONS.iter().map(|s| s.to_string()).collect()
        } else {
            vec![normalize_abs(root)]
        }
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

        self.current_path = START_DIR.to_string();
        self.connected = true;
        tracing::info!("[Proton CLI] Connected as {}", self.display_name());
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        // Must not call `auth logout`: that session belongs to the user.
        self.connected = false;
        self.current_path = START_DIR.to_string();
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
                other => explain_unlistable(&target, other),
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
                other => explain_unlistable(&new_path, other),
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
        // No opening report: the CLI says nothing while it downloads, and the
        // `(0, 0)` sent here read as a transfer already done, even when the
        // download then failed. Opening at `(0, size)` instead would cost one
        // more CLI run per file to read the size (`filesystem info`, 0.75 to
        // 1.6 s on 0.8.0) for a total the callers already show from their own
        // listing. The only report is the completed file, below.
        let remote = self.resolve_path(remote_path);

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
        // A file a bulk handed back is held to the size its fresh listing
        // claimed, as the bulk would have held it.
        let claimed = self
            .bulk_claimed_sizes
            .get(&remote)
            .filter(|(_, listed)| listed.elapsed() < BULK_CLAIMED_SIZE_TTL)
            .map(|(size, _)| *size);
        let temp_dir = anchor.join(format!(".aeroftp-proton-dl-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .map_err(ProviderError::IoError)?;
        let temp_dir_s = match literal_local_path_for_cli(&temp_dir.to_string_lossy()) {
            Ok(path) => path,
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&temp_dir).await;
                return Err(error);
            }
        };
        let result = async {
            self.run_cli(
                &[
                    "filesystem",
                    "download",
                    // JSON: a summary with a reason per failed item, and no
                    // interactive prompt.
                    "-j",
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
            // proton-drive writes the file under a name safe on every
            // platform, `"` or `?` turned into `_` on Linux too.
            let downloaded = temp_dir.join(cli_local_name(&remote_name));
            if let Some(parent) = final_path.parent() {
                if !parent.as_os_str().is_empty() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(ProviderError::IoError)?;
                }
            }
            if let Some(claimed) = claimed {
                let actual = tokio::fs::metadata(&downloaded)
                    .await
                    .map_err(ProviderError::IoError)?
                    .len();
                if actual != claimed {
                    return Err(ProviderError::IoError(claimed_size_mismatch(
                        &remote_name,
                        claimed,
                        actual,
                    )));
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
        // Published: the bulk's expectation for this file is spent.
        self.bulk_claimed_sizes.remove(&remote);

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
        let parent = parent_of(&remote);
        ensure_parent_writable(&parent)?;
        // The official CLI writes no progress outside its debug log, which is
        // not a stable interface and carries the blocks' upload tokens, so the
        // bar opens at 0 and reaches 100 when the CLI has returned. It used to
        // open with `(0, 0)`, which reads as an empty upload already done.
        let size = std::fs::metadata(local_path)
            .map_err(ProviderError::IoError)?
            .len();
        let progress = super::upload_progress::UploadProgress::new(on_progress, size);
        progress.start();
        let local_cli = normalize_local_path_for_cli(local_path);
        let local_name = basename(&local_cli.replace('\\', "/"));
        let dest_name = basename(&remote);
        let (staging_dir, upload_path) = if local_name == dest_name {
            (None, local_cli)
        } else {
            let (dir, staged) = stage_local_as_dest(&local_cli, &dest_name)?;
            (Some(dir), staged.to_string_lossy().into_owned())
        };
        let upload_path = match literal_local_path_for_cli(&upload_path) {
            Ok(path) => path,
            Err(error) => {
                if let Some(dir) = staging_dir {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                return Err(error);
            }
        };
        let result = self
            .run_cli(
                &[
                    "filesystem",
                    "upload",
                    // Without JSON, proton-drive 0.9.0 opens its upgrade page
                    // in the browser after every upload that fails on a full
                    // drive (`storageQuotaUpsell.ts`): one tab per file of a
                    // recursive upload. JSON also gives a reason per item.
                    "-j",
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

        progress.complete();
        Ok(())
    }

    fn supports_bulk_upload(&self) -> bool {
        true
    }
    fn supports_bulk_download(&self) -> bool {
        true
    }

    async fn upload_bulk(
        &mut self,
        items: &[BulkTransferItem],
        progress: Option<BulkProgress>,
    ) -> Result<Vec<BulkTransferOutcome>, ProviderError> {
        self.transfer_bulk(items, true, progress).await
    }

    async fn download_bulk(
        &mut self,
        items: &[BulkTransferItem],
        progress: Option<BulkProgress>,
    ) -> Result<Vec<BulkTransferOutcome>, ProviderError> {
        self.transfer_bulk(items, false, progress).await
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let abs = self.resolve_path(path);
        // The root and its sections always exist. Saying so lets an ancestor
        // walk (sync conflict uploads, recursive mkdir) pass through
        // `/my-files` instead of stopping on a refusal.
        if abs == "/" || ROOT_SECTIONS.contains(&abs.as_str()) {
            return Err(ProviderError::AlreadyExists(abs));
        }
        let parent = parent_of(&abs);
        ensure_parent_writable(&parent)?;
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
        ensure_not_section(&abs)?;
        self.run_cli(&["filesystem", "trash", &abs], META_TIMEOUT_SECS)
            .await?;
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // Round 2 of the 4.2.1 review: the API's delete takes a folder's
        // content along, so a folder that lists anything is refused here and
        // only one that listed empty reaches it.
        self.refuse_non_empty_dir(path).await?;
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    fn purges_live_path(&self) -> bool {
        true
    }

    async fn capture_trash_identities(
        &mut self,
        paths: &[String],
    ) -> Vec<Result<Option<String>, ProviderError>> {
        self.trash_listing = None;
        let absolute: Vec<String> = paths.iter().map(|path| self.resolve_path(path)).collect();
        let Some(first) = absolute.first() else {
            return Vec::new();
        };
        let parent = parent_of(first);
        let one_parent = absolute.iter().all(|path| parent_of(path) == parent);
        let listed = if one_parent {
            match self.list(&parent).await {
                Ok(entries) => Some(entries),
                Err(error) => {
                    let message = error.to_string();
                    return absolute
                        .iter()
                        .map(|_| Err(ProviderError::Other(message.clone())))
                        .collect();
                }
            }
        } else {
            None
        };
        let mut by_name = std::collections::HashMap::new();
        for entry in listed.into_iter().flatten() {
            let name = entry.name.clone();
            by_name
                .entry(name)
                .and_modify(|uid| *uid = None)
                .or_insert_with(|| entry_uid(&entry).ok());
        }
        let mut captured = Vec::with_capacity(absolute.len());
        for abs in &absolute {
            let uid = match by_name.get(&basename(abs)) {
                Some(Some(uid)) => Ok(uid.clone()),
                _ => self.capture_uid(abs).await,
            };
            captured.push(uid.map(Some));
        }
        captured
    }

    async fn delete_permanent_with_identity(
        &mut self,
        path: &str,
        identity: Option<&str>,
    ) -> Result<bool, ProviderError> {
        let Some(uid) = identity else {
            return self.delete_permanent(path).await;
        };
        let abs = self.resolve_path(path);
        ensure_not_section(&abs)?;
        let trash_root = if first_segment(&abs) == "photos-trash" {
            "/photos-trash"
        } else {
            "/trash"
        };
        self.purge_trash_by_name(trash_root, &basename(&abs), Some(uid), true)
            .await
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        self.trash_listing = None;
        let abs = self.resolve_path(path);
        ensure_not_section(&abs)?;
        let in_trash = is_in_trash_path(&abs);
        let trash_root = if first_segment(&abs) == "photos-trash" {
            "/photos-trash"
        } else {
            "/trash"
        };
        let name = basename(&abs);

        let captured_uid = if !in_trash {
            match self.capture_uid(&abs).await {
                Ok(uid) => Some(uid),
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

        self.purge_trash_by_name(trash_root, &name, captured_uid.as_deref(), false)
            .await
    }

    /// A rename in place, or a move to the new folder (which keeps the name)
    /// and then a rename. Proton refuses a taken name, but at the rename
    /// after the move the source had already moved, so the destination is
    /// looked up first and a taken one refused as AlreadyExists before any
    /// command. When the destination folder already holds the old name the
    /// rename goes first, in the source folder; when the source folder also
    /// holds the new name, nothing is changed.
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from = self.resolve_path(from);
        let to = self.resolve_path(to);
        ensure_not_section(&from)?;
        let from_parent = parent_of(&from);
        let to_parent = parent_of(&to);
        ensure_parent_writable(&to_parent)?;
        if from == to {
            return Ok(());
        }
        super::refuse_occupied_destination(self, &from, &to).await?;
        let (from_name, to_name) = (basename(&from), basename(&to));
        if from_parent == to_parent {
            return self.rename_in_place(&from, &to_name).await;
        }
        let old_name_at_destination = join_path(&to_parent, &from_name);
        let rename_first = from_name != to_name && self.exists(&old_name_at_destination).await?;
        if rename_first {
            let new_name_at_source = join_path(&from_parent, &to_name);
            if self.exists(&new_name_at_source).await? {
                return Err(ProviderError::Other(format!(
                    "Cannot move {from} to {to} in two steps without two items sharing a name: \
                     {old_name_at_destination} and {new_name_at_source} both exist"
                )));
            }
            self.rename_in_place(&from, &to_name).await?;
            // If the move fails, the rename is undone; if that fails too,
            // the error says where the item is.
            if let Err(e) = self.move_into(&new_name_at_source, &to_parent).await {
                let undone = self.rename_in_place(&new_name_at_source, &from_name).await;
                return Err(super::second_step_failed(
                    &from,
                    &to,
                    &new_name_at_source,
                    e,
                    undone,
                ));
            }
            return Ok(());
        }
        self.move_into(&from, &to_parent).await?;
        if from_name != to_name {
            if let Err(e) = self
                .rename_in_place(&old_name_at_destination, &to_name)
                .await
            {
                let undone = self.move_into(&old_name_at_destination, &from_parent).await;
                return Err(super::second_step_failed(
                    &from,
                    &to,
                    &old_name_at_destination,
                    e,
                    undone,
                ));
            }
        }
        Ok(())
    }

    /// No: the Proton Drive CLI renames and moves without an overwrite, so
    /// there is no one-step replace, and the callers that need one refuse
    /// before they write anything.
    async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
        Ok(false)
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
        ensure_not_section(&from)?;
        let parent = parent_of(&to);
        ensure_parent_writable(&parent)?;
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
            // as an option value of `sharing set-url`, with no stdin form and
            // no environment variable. Checked against the binary's own help
            // rather than assumed.
            //
            // One token, `--password=VALUE`: as two tokens a password that
            // starts with `-` is read as an option, the ambiguity measured on
            // `copy -n -x`, and `--name=-x` is the form the CLI documents for
            // a value that starts with `-`. `redact_cli_args` hides the value
            // in this form too.
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
            args.push(format!("--password={pw}"));
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
        // The response also contains customPassword and link secrets. Never
        // embed it in diagnostics when a future CLI changes its schema.
        let url = extract_url(&stdout).ok_or_else(|| {
            ProviderError::ParseError("Could not parse Proton share URL from CLI response".into())
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
            // A listing clone downloads nothing.
            bulk_claimed_sizes: std::collections::HashMap::new(),
            trash_listing: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Proton Drive CLI renames and moves without an overwrite, so there is
    /// no one-step replace. The answer is no, so the callers that need one (CLI
    /// `edit`, MCP `remote_edit`, the crypt marker paths) refuse before they
    /// write.
    #[tokio::test]
    async fn proton_does_not_claim_an_atomic_replace() {
        let mut p = ProtonCliProvider::new(ProtonConfig {
            display_name: "t".into(),
            binary_path: None,
            connection_mode: ProtonConnectionMode::Cli,
            profile_id: None,
            username: None,
        });
        assert!(!p.supports_atomic_replace().await.unwrap());
        assert!(!p.replace_sets_aside());
    }

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

    fn proton_profile(extra: &[(&str, &str)]) -> super::super::ProviderConfig {
        super::super::ProviderConfig {
            name: "p".to_string(),
            provider_type: super::super::ProviderType::Proton,
            host: "drive.proton.me".to_string(),
            port: None,
            username: None,
            password: None,
            initial_path: None,
            extra: extra
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// Saved profiles have no `proton_mode`: they stay CLI profiles. Only an
    /// explicit `native` selects the native client, and a value nobody wrote
    /// is refused instead of guessed.
    #[test]
    fn the_connection_mode_is_cli_unless_the_profile_asks_for_native() {
        let _env = crate::test_env::lock();
        let mode = |extra: &[(&str, &str)]| {
            ProtonConfig::from_provider_config(&proton_profile(extra)).map(|c| c.connection_mode)
        };
        assert_eq!(mode(&[]).unwrap(), ProtonConnectionMode::Cli);
        assert_eq!(
            mode(&[("proton_mode", "cli")]).unwrap(),
            ProtonConnectionMode::Cli
        );
        assert_eq!(
            mode(&[("proton_mode", "native")]).unwrap(),
            ProtonConnectionMode::Native
        );
        assert!(matches!(
            mode(&[("proton_mode", "Native ")]),
            Err(ProviderError::InvalidConfig(_))
        ));
        assert!(matches!(
            mode(&[("proton_mode", "megacmd")]),
            Err(ProviderError::InvalidConfig(_))
        ));
    }

    /// The factory builds what the mode names and never falls back to the
    /// CLI: a build without the native client refuses a native profile.
    #[test]
    fn the_factory_builds_the_client_the_mode_names() {
        let _env = crate::test_env::lock();
        let mut cli = super::super::ProviderFactory::create(&proton_profile(&[]))
            .unwrap_or_else(|e| panic!("cli profile refused: {e}"));
        assert!(cli
            .as_any_mut()
            .downcast_mut::<ProtonCliProvider>()
            .is_some());

        let native = super::super::ProviderFactory::create(&proton_profile(&[
            ("proton_mode", "native"),
            ("profile_id", "srv_1"),
        ]));
        #[cfg(feature = "proton-native")]
        {
            let mut native = native.unwrap_or_else(|e| panic!("native profile refused: {e}"));
            assert!(!native.is_connected());
            assert_eq!(native.provider_type(), ProviderType::Proton);
            assert!(native
                .as_any_mut()
                .downcast_mut::<crate::providers::proton_native::provider::ProtonNativeProvider>()
                .is_some());
        }
        #[cfg(not(feature = "proton-native"))]
        match native {
            Err(ProviderError::InvalidConfig(text)) => {
                assert_eq!(text, super::super::NATIVE_PROTON_UNAVAILABLE)
            }
            Err(other) => panic!("wrong refusal: {other}"),
            Ok(_) => panic!("a build without the native client built one"),
        }
    }

    #[test]
    fn the_profile_id_is_read_and_a_blank_one_is_none() {
        let _env = crate::test_env::lock();
        let id = |extra: &[(&str, &str)]| {
            ProtonConfig::from_provider_config(&proton_profile(extra))
                .unwrap()
                .profile_id
        };
        assert_eq!(id(&[("profile_id", "srv_1")]).as_deref(), Some("srv_1"));
        assert_eq!(id(&[("profile_id", "  ")]), None);
        assert_eq!(id(&[]), None);
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
            connection_mode: ProtonConnectionMode::Cli,
            profile_id: None,
            username: None,
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

    /// The password goes out as one `--password=VALUE` token, and the log
    /// line hides the value in that form too.
    #[test]
    fn redacts_a_share_password_given_in_one_token() {
        let args = ["sharing", "set-url", "--password=-secret", "/my-files/a"];
        let redacted = redact_cli_args(&args);
        assert_eq!(redacted[2], "--password=***");
        assert!(redacted.iter().all(|s| !s.contains("secret")));
    }

    #[test]
    fn resolve_path_normalizes_dotdot() {
        let mut p = ProtonCliProvider::new(ProtonConfig {
            display_name: "t".into(),
            binary_path: None,
            connection_mode: ProtonConnectionMode::Cli,
            profile_id: None,
            username: None,
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

    #[test]
    fn only_a_transfer_summary_that_lists_failures_is_read_as_one() {
        assert_eq!(transfer_failures(""), None);
        assert_eq!(transfer_failures("[]"), None);
        assert_eq!(transfer_failures("Transfer summary:"), None);
        assert_eq!(
            transfer_failures(r#"{"failedItems":0,"failures":[]}"#),
            None
        );
        assert_eq!(
            transfer_failures(r#"{"failedItems":1,"failures":[{"name":"a","error":"E: m"}]}"#),
            Some(vec![("a".to_string(), "E: m".to_string())])
        );
    }

    /// A full drive is read from the API's refusal, never from a name or a
    /// local path: either can hold any word.
    #[test]
    fn a_full_drive_is_read_from_the_refusal_never_from_a_name() {
        let classify = |name: &str, error: &str| {
            classify_transfer_failures(&[(name.to_string(), error.to_string())])
        };
        let is_quota = |error: &ProviderError| error.to_string().contains("Storage quota exceeded");
        assert!(is_quota(&classify(
            "a.txt",
            "ValidationError: Insufficient storage quota"
        )));
        assert!(!is_quota(&classify(
            "quota.txt",
            "APICodeError: Something went wrong"
        )));
        assert!(!is_quota(&classify(
            "x",
            "Error: EACCES: permission denied, open '/home/u/quota/x'"
        )));
        assert!(matches!(
            classify("gone.txt", "NotFoundAPIError: Node not found"),
            ProviderError::NotFound(ref t) if t.contains("gone.txt")
        ));
    }

    /// The upload path wraps the summary as `TransferFailed`, and that string
    /// is classified again. The file name is in the message the user reads and
    /// not in the class: `Storage quota exceeded.txt` failing for another
    /// reason is not a full drive, which would never be retried.
    #[test]
    fn a_file_named_like_a_quota_error_is_not_a_full_drive() {
        let classified = |name: &str, error: &str| {
            let failure = classify_transfer_failures(&[(name.to_string(), error.to_string())]);
            ProviderError::TransferFailed(format!("Upload failed: {failure}"))
        };
        let refused = crate::transfer_dag::TransferError::from_provider(&classified(
            "Storage quota exceeded.txt",
            "APICodeError: Something went wrong",
        ));
        assert!(
            refused.message.contains("Storage quota exceeded.txt"),
            "{}",
            refused.message
        );
        assert_ne!(
            refused.kind,
            crate::transfer_dag::TransferErrorKind::QuotaExceeded,
            "{}",
            refused.message
        );
        assert_ne!(
            refused.retry,
            crate::transfer_dag::RetryDirective::Never,
            "{}",
            refused.message
        );

        let full = crate::transfer_dag::TransferError::from_provider(&classified(
            "Storage quota exceeded.txt",
            "ValidationError: Insufficient storage quota",
        ));
        assert_eq!(
            full.kind,
            crate::transfer_dag::TransferErrorKind::QuotaExceeded,
            "{}",
            full.message
        );
        assert_eq!(full.retry, crate::transfer_dag::RetryDirective::Never);
        assert!(
            full.message.contains("Storage quota exceeded.txt"),
            "{}",
            full.message
        );
    }
}

/// Shim tests for upload/download/purge argv. Unix-only: the shim is a Python
/// script. These are the sequences Fable measured against the real CLI.
#[cfg(all(test, unix))]
mod cli_sequence_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    fn bulk_upload_items(dir: &Path, count: usize) -> Vec<BulkTransferItem> {
        (0..count)
            .map(|i| {
                let name = format!("file-{i:03}.txt");
                let path = dir.join(&name);
                std::fs::write(&path, b"DATA").unwrap();
                BulkTransferItem {
                    local_path: path.to_string_lossy().into_owned(),
                    remote_path: format!("/my-files/bulk/{name}"),
                    size: 4,
                }
            })
            .collect()
    }

    #[tokio::test]
    async fn bulk_download_binds_fresh_uids_to_only_the_selected_names() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let uid_a = format!("{}~{}", "A".repeat(22), "B".repeat(22));
        let uid_b = format!("{}~{}", "A".repeat(22), "C".repeat(22));
        let uid_other = format!("{}~{}", "A".repeat(22), "D".repeat(22));
        std::fs::write(
            dir.path().join("my-files_bulk.json"),
            serde_json::to_vec(&serde_json::json!([
                {"name":{"ok":true,"value":"a.txt"},"uid":uid_a,"type":"file"},
                {"name":{"ok":true,"value":"b.txt"},"uid":uid_b,"type":"file"},
                {"name":{"ok":true,"value":"not-selected.txt"},"uid":uid_other,"type":"file"}
            ]))
            .unwrap(),
        )
        .unwrap();
        let map: std::collections::HashMap<_, _> =
            [(uid_a.clone(), "a.txt"), (uid_b.clone(), "b.txt")]
                .into_iter()
                .collect();
        std::fs::write(
            dir.path().join("uid-name-map.json"),
            serde_json::to_vec(&map).unwrap(),
        )
        .unwrap();
        let items: Vec<_> = ["a.txt", "b.txt"]
            .iter()
            .map(|name| BulkTransferItem {
                local_path: dir.path().join(name).to_string_lossy().into_owned(),
                remote_path: format!("/my-files/bulk/{name}"),
                size: 14,
            })
            .collect();
        let results = provider(&shim).download_bulk(&items, None).await.unwrap();
        assert!(results
            .iter()
            .all(|r| matches!(r, BulkTransferOutcome::Completed)));
        let argv = read_argv(dir.path());
        let commands = verbs(&argv, "download");
        assert_eq!(commands.len(), 1);
        assert!(commands[0].contains(&format!("/my-files/{uid_a}")));
        assert!(commands[0].contains(&format!("/my-files/{uid_b}")));
        assert!(!commands[0].contains(&format!("/my-files/{uid_other}")));
        assert!(!dir.path().join("not-selected.txt").exists());
        assert_eq!(verbs(&argv, "list").len(), 1);
    }

    #[test]
    fn bulk_uid_shape_matches_only_the_cli_uid_grammar() {
        assert!(valid_cli_node_uid(&format!(
            "{}~{}",
            "A".repeat(22),
            "b".repeat(22)
        )));
        for uid in [
            "../../elsewhere",
            "AA~BB",
            "-j",
            "/my-files/folder",
            "bad~uid~extra",
        ] {
            assert!(!valid_cli_node_uid(uid));
        }
        assert!(!valid_cli_node_uid(&format!(
            "{}~{}",
            "=".repeat(22),
            "B".repeat(22)
        )));
    }

    #[test]
    fn bulk_staging_flags_aliasing_names_and_leaves_no_probe_behind() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![
            "/one/a.txt".to_string(),
            "/two/a.txt".to_string(),
            "/one/b.txt".to_string(),
        ];
        assert_eq!(
            reserve_bulk_download_names(dir.path(), &paths).unwrap(),
            vec![false, true, false]
        );
        // The CLI must find an empty folder: what it reports as written is
        // what it wrote, never a probe left from the reservation.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    /// Without `claimedSize` Proton lists the encrypted size. v4.2.4 refused
    /// to publish such a download ("size differs from the planned file") on
    /// every run, although the per-file path delivered it.
    #[tokio::test]
    async fn bulk_download_publishes_a_file_whose_listed_size_was_the_storage_size() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let items: Vec<_> = ["a.txt", "b.txt"]
            .iter()
            .map(|name| BulkTransferItem {
                local_path: dir.path().join(name).to_string_lossy().into_owned(),
                remote_path: format!("/my-files/bulk/{name}"),
                size: 1234,
            })
            .collect();
        let results = provider(&shim).download_bulk(&items, None).await.unwrap();
        assert!(results
            .iter()
            .all(|r| matches!(r, BulkTransferOutcome::Completed)));
        for item in &items {
            assert_eq!(
                std::fs::read_to_string(&item.local_path).unwrap(),
                "REMOTE-CONTENT"
            );
        }
    }

    /// A claimed size in the fresh listing is a promise about the plaintext:
    /// a staged file of another size is not published. Without one, the
    /// listed size is the storage size and only the SDK's block and manifest
    /// checks stand behind the file.
    #[tokio::test]
    async fn bulk_download_holds_a_file_to_the_size_its_uploader_claimed() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let uid = |c: char| format!("{}~{}", "A".repeat(22), c.to_string().repeat(22));
        std::fs::write(
            dir.path().join("my-files_bulk.json"),
            serde_json::to_vec(&serde_json::json!([
                {"name":{"ok":true,"value":"same.txt"},"uid":uid('B'),"type":"file","activeRevision":{"claimedSize":14}},
                {"name":{"ok":true,"value":"other.txt"},"uid":uid('C'),"type":"file","activeRevision":{"claimedSize":999}},
                {"name":{"ok":true,"value":"unclaimed.txt"},"uid":uid('D'),"type":"file","totalStorageSize":1234}
            ]))
            .unwrap(),
        )
        .unwrap();
        let map: std::collections::HashMap<_, _> = [
            (uid('B'), "same.txt"),
            (uid('C'), "other.txt"),
            (uid('D'), "unclaimed.txt"),
        ]
        .into_iter()
        .collect();
        std::fs::write(
            dir.path().join("uid-name-map.json"),
            serde_json::to_vec(&map).unwrap(),
        )
        .unwrap();
        let items: Vec<_> = ["same.txt", "other.txt", "unclaimed.txt"]
            .iter()
            .map(|name| BulkTransferItem {
                local_path: dir.path().join(name).to_string_lossy().into_owned(),
                remote_path: format!("/my-files/bulk/{name}"),
                size: 1234,
            })
            .collect();
        let results = provider(&shim).download_bulk(&items, None).await.unwrap();
        assert!(matches!(&results[0], BulkTransferOutcome::Completed));
        assert!(matches!(&results[1], BulkTransferOutcome::Failed(_)));
        assert!(matches!(&results[2], BulkTransferOutcome::Completed));
        assert!(Path::new(&items[0].local_path).exists());
        assert!(!Path::new(&items[1].local_path).exists());
        assert!(Path::new(&items[2].local_path).exists());
    }

    /// Writes the listing of `/my-files/bulk` and the UID map the shim needs:
    /// `same.txt` claims the 14 bytes the shim writes, `other.txt` claims 999.
    fn claimed_listing(dir: &Path) {
        let uid = |c: char| format!("{}~{}", "A".repeat(22), c.to_string().repeat(22));
        std::fs::write(
            dir.join("my-files_bulk.json"),
            serde_json::to_vec(&serde_json::json!([
                {"name":{"ok":true,"value":"same.txt"},"uid":uid('B'),"type":"file","activeRevision":{"claimedSize":14}},
                {"name":{"ok":true,"value":"other.txt"},"uid":uid('C'),"type":"file","activeRevision":{"claimedSize":999}}
            ]))
            .unwrap(),
        )
        .unwrap();
        let map: std::collections::HashMap<_, _> =
            [(uid('B'), "same.txt"), (uid('C'), "other.txt")]
                .into_iter()
                .collect();
        std::fs::write(
            dir.join("uid-name-map.json"),
            serde_json::to_vec(&map).unwrap(),
        )
        .unwrap();
    }

    /// CodeRabbit on #1334: a claimed-size mismatch was a retryable local IO
    /// error, so the generic layer sent the file to the single-file path,
    /// which published it unchecked. It is final in the bulk now.
    #[tokio::test]
    async fn bulk_claimed_size_mismatch_is_final_and_never_reaches_the_single_file_path() {
        use crate::provider_transfer_executor::{
            resolve_provider_transfer_runtime, ProviderDownloadExecutor,
        };
        use crate::transfer_domain::{TransferBatchConfig, TransferDirection, TransferEntry};
        use crate::transfer_event_sink::{NoopTransferSink, TransferEventSink};
        use crate::transfer_orchestrator::{execute_batch, TransferBatch};
        use std::sync::{atomic::AtomicBool, Arc};
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        claimed_listing(dir.path());
        let out = dir.path().join("out");
        let provider: Arc<tokio::sync::Mutex<Option<Box<dyn StorageProvider>>>> =
            Arc::new(tokio::sync::Mutex::new(Some(Box::new(provider(&shim)))));
        let (settings, model, caps) =
            resolve_provider_transfer_runtime(&provider, Default::default()).await;
        let sink: Arc<dyn TransferEventSink> = Arc::new(NoopTransferSink);
        let result = execute_batch(
            Arc::clone(&sink),
            TransferBatch {
                id: "bulk-claimed".into(),
                display_name: "Bulk".into(),
                direction: TransferDirection::Download,
                config: TransferBatchConfig {
                    max_concurrent: 1,
                    ..Default::default()
                },
                entries: ["same.txt", "other.txt"]
                    .iter()
                    .map(|name| TransferEntry {
                        id: name.to_string(),
                        display_name: name.to_string(),
                        local_path: out.join(name).to_string_lossy().into_owned(),
                        remote_path: format!("/my-files/bulk/{name}"),
                        size: 1234,
                        modified: None,
                    })
                    .collect(),
            },
            Arc::new(ProviderDownloadExecutor::new(
                sink,
                provider,
                settings,
                tokio_util::sync::CancellationToken::new(),
                model,
                caps,
            )),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!((result.completed, result.failed), (1, 1));
        assert_eq!(verbs(&read_argv(dir.path()), "download").len(), 1);
        assert!(out.join("same.txt").exists());
        assert!(!out.join("other.txt").exists());
    }

    /// A file a bulk handed back (here: a run that proved nothing, zero
    /// counters and exit 1) is downloaded alone and held to the claimed size
    /// of the bulk's listing, on every attempt, until it is published.
    #[tokio::test]
    async fn single_file_download_holds_a_handed_back_file_to_its_claimed_size() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        claimed_listing(dir.path());
        std::fs::write(
            dir.path().join("bulk-summary-once.json"),
            r#"{"transferredItems":0,"skippedItems":0,"failedItems":0,"failures":[]}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("transfer-exit-1"), "").unwrap();
        let out = dir.path().join("out");
        let item = |name: &str| BulkTransferItem {
            local_path: out.join(name).to_string_lossy().into_owned(),
            remote_path: format!("/my-files/bulk/{name}"),
            size: 1234,
        };
        let mut p = provider(&shim);
        let results = p
            .download_bulk(&[item("same.txt"), item("other.txt")], None)
            .await
            .unwrap();
        assert!(results
            .iter()
            .all(|r| matches!(r, BulkTransferOutcome::NotAttempted(_))));
        let other = out.join("other.txt");
        for _attempt in 0..2 {
            assert!(p
                .download("/my-files/bulk/other.txt", other.to_str().unwrap(), None)
                .await
                .is_err());
            assert!(!other.exists());
        }
        let same = out.join("same.txt");
        p.download("/my-files/bulk/same.txt", same.to_str().unwrap(), None)
            .await
            .unwrap();
        assert!(same.exists());
        assert!(!p.bulk_claimed_sizes.contains_key("/my-files/bulk/same.txt"));
        assert!(p
            .bulk_claimed_sizes
            .contains_key("/my-files/bulk/other.txt"));
    }

    /// Only what a bulk hands back is held to its listing. A file it
    /// published, or failed for good, keeps the ordinary single-file
    /// download, which compares nothing: the listing may be stale by then.
    #[tokio::test]
    async fn single_file_download_of_a_file_the_bulk_settled_is_not_compared() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        claimed_listing(dir.path());
        let out = dir.path().join("out");
        let item = |name: &str| BulkTransferItem {
            local_path: out.join(name).to_string_lossy().into_owned(),
            remote_path: format!("/my-files/bulk/{name}"),
            size: 1234,
        };
        let mut p = provider(&shim);
        let results = p
            .download_bulk(&[item("same.txt"), item("other.txt")], None)
            .await
            .unwrap();
        assert!(matches!(results[0], BulkTransferOutcome::Completed));
        assert!(matches!(results[1], BulkTransferOutcome::Failed(_)));
        assert!(p.bulk_claimed_sizes.is_empty());
        for name in ["same.txt", "other.txt"] {
            let target = dir.path().join("again").join(name);
            p.download(
                &format!("/my-files/bulk/{name}"),
                target.to_str().unwrap(),
                None,
            )
            .await
            .unwrap();
            assert!(target.exists());
        }
    }

    /// The CLI resolves every source before it transfers anything; one that
    /// vanished since the listing ends the run with zero counters. Nothing
    /// was published, so each file goes back to the single-file path instead
    /// of all of them being reported as unknown.
    #[tokio::test]
    async fn bulk_download_resolution_failure_hands_every_file_back_unattempted() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        std::fs::write(
            dir.path().join("bulk-summary.json"),
            r#"{"transferredItems":0,"skippedItems":0,"failedItems":0,"failures":[]}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("transfer-exit-1"), "").unwrap();
        let items: Vec<_> = ["a.txt", "b.txt"]
            .iter()
            .map(|name| BulkTransferItem {
                local_path: dir
                    .path()
                    .join("out")
                    .join(name)
                    .to_string_lossy()
                    .into_owned(),
                remote_path: format!("/my-files/bulk/{name}"),
                size: 14,
            })
            .collect();
        let results = provider(&shim).download_bulk(&items, None).await.unwrap();
        assert!(results
            .iter()
            .all(|r| matches!(r, BulkTransferOutcome::NotAttempted(_))));
        assert!(!dir.path().join("out").join("a.txt").exists());
    }

    #[tokio::test]
    async fn mkdir_on_a_section_says_it_exists_and_root_folders_stay_refused() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let mut p = provider(&shim);
        assert!(matches!(
            p.mkdir("/my-files").await,
            Err(ProviderError::AlreadyExists(_))
        ));
        assert!(matches!(
            p.mkdir("/new-root-folder").await,
            Err(ProviderError::PermissionDenied(_))
        ));
        assert!(verbs(&read_argv(dir.path()), "create-folder").is_empty());
    }

    #[tokio::test]
    async fn bulk_sync_uses_one_native_upload_and_accounts_all_files() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let input = dir.path().join("input");
        std::fs::create_dir(&input).unwrap();
        bulk_upload_items(&input, 200);
        let mut p: Box<dyn StorageProvider> = Box::new(provider(&shim));
        let options = crate::sync::SyncOptions {
            delta_policy: crate::sync::DeltaPolicy::SizeOnly,
            ..Default::default()
        };
        let report = crate::transfer_dag_sync::execute_sync_dag(
            &mut p,
            input.to_str().unwrap(),
            "/my-files/bulk",
            &options,
            &mut crate::sync::NoopProgressSink,
        )
        .await;
        assert_eq!(report.uploaded, 200);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(verbs(&read_argv(dir.path()), "upload").len(), 1);
    }

    /// Explicitly opt-in live measurement. Creates and permanently removes
    /// ONLY its own UUID-named test tree; never uses a saved profile or prints
    /// authentication data. All readbacks are byte-checked against generated
    /// payloads. Run with AEROFTP_BULK_LIVE=1 and --ignored --exact.
    #[tokio::test]
    #[ignore = "requires an authenticated official CLI and an exclusive benchmark window"]
    async fn bulk_live_benchmark() {
        use crate::provider_transfer_executor::{
            resolve_provider_transfer_runtime, ProviderDownloadExecutor, ProviderUploadExecutor,
        };
        use crate::transfer_domain::{TransferBatchConfig, TransferDirection, TransferEntry};
        use crate::transfer_event_sink::{NoopTransferSink, TransferEventSink};
        use crate::transfer_orchestrator::{execute_batch, TransferBatch};
        use std::sync::{atomic::AtomicBool, Arc};
        assert_eq!(std::env::var("AEROFTP_BULK_LIVE").as_deref(), Ok("1"));
        fn live_provider() -> ProtonCliProvider {
            ProtonCliProvider::new(ProtonConfig {
                display_name: "Bulk benchmark".into(),
                binary_path: None,
                connection_mode: ProtonConnectionMode::Cli,
                profile_id: None,
                username: None,
            })
        }
        fn ensure(ok: bool, message: &str) -> Result<(), ProviderError> {
            if ok {
                Ok(())
            } else {
                Err(ProviderError::TransferFailed(message.into()))
            }
        }
        async fn dag(items: &[BulkTransferItem], upload: bool) -> Result<(), ProviderError> {
            let provider: Arc<tokio::sync::Mutex<Option<Box<dyn StorageProvider>>>> =
                Arc::new(tokio::sync::Mutex::new(Some(Box::new(live_provider()))));
            let (settings, model, caps) =
                resolve_provider_transfer_runtime(&provider, Default::default()).await;
            let sink: Arc<dyn TransferEventSink> = Arc::new(NoopTransferSink);
            let batch = TransferBatch {
                id: "live-bulk".into(),
                display_name: "Live bulk".into(),
                direction: if upload {
                    TransferDirection::Upload
                } else {
                    TransferDirection::Download
                },
                config: TransferBatchConfig {
                    max_concurrent: 1,
                    ..Default::default()
                },
                entries: items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| TransferEntry {
                        id: i.to_string(),
                        display_name: basename(&item.remote_path),
                        local_path: item.local_path.clone(),
                        remote_path: item.remote_path.clone(),
                        size: item.size,
                        modified: None,
                    })
                    .collect(),
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let token = tokio_util::sync::CancellationToken::new();
            let result = if upload {
                execute_batch(
                    Arc::clone(&sink),
                    batch,
                    Arc::new(ProviderUploadExecutor::new(
                        sink, provider, settings, None, token, model, caps,
                    )),
                    cancel,
                    None,
                )
                .await
            } else {
                execute_batch(
                    Arc::clone(&sink),
                    batch,
                    Arc::new(ProviderDownloadExecutor::new(
                        sink, provider, settings, token, model, caps,
                    )),
                    cancel,
                    None,
                )
                .await
            };
            ensure(
                result.completed as usize == items.len() && result.failed == 0,
                &format!("Live DAG results: {result:?}"),
            )
        }
        async fn per_file(items: &[BulkTransferItem], upload: bool) -> Result<(), ProviderError> {
            let mut jobs = tokio::task::JoinSet::new();
            for worker in 0..4 {
                let chunk: Vec<_> = items.iter().skip(worker).step_by(4).cloned().collect();
                jobs.spawn(async move {
                    let mut p = live_provider();
                    for item in chunk {
                        if upload {
                            p.upload(&item.local_path, &item.remote_path, None).await?;
                        } else {
                            p.download(&item.remote_path, &item.local_path, None)
                                .await?;
                        }
                    }
                    Ok::<(), ProviderError>(())
                });
            }
            while let Some(result) = jobs.join_next().await {
                result.map_err(|e| ProviderError::TransferFailed(e.to_string()))??;
            }
            Ok(())
        }
        let root = format!(
            "/my-files/aeroftp-bulk-20261020-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        );
        let local = tempfile::tempdir().unwrap();
        let input = local.path().join("input");
        std::fs::create_dir(&input).unwrap();
        for i in 0..200u32 {
            let mut payload = vec![b'x'; 4096];
            payload[..4].copy_from_slice(&i.to_le_bytes());
            std::fs::write(input.join(format!("file-{i:03}.bin")), payload).unwrap();
        }
        let mut rows = Vec::new();
        let mut admin = live_provider();
        let result: Result<(), ProviderError> = async {
            admin.mkdir(&root).await?;
            println!("BULK_LIVE_ROOT {root}");
            // Alternate direct CLI and shared DAG order; take one four-worker
            // per-file baseline separately. No contender runs concurrently.
            for (round, modes) in [(0, vec!["official", "bulk"]), (1, vec!["bulk", "official"]), (2, vec!["official", "bulk"]), (3, vec!["per-file"])] {
                for mode in modes {
                    let remote = format!("{root}/{mode}-{round}");
                    admin.mkdir(&remote).await?;
                    let items: Vec<_> = (0..200).map(|i| {
                        let name = format!("file-{i:03}.bin");
                        BulkTransferItem { local_path: input.join(&name).to_string_lossy().into_owned(), remote_path: format!("{remote}/{name}"), size: 4096 }
                    }).collect();
                    let mut download_items = items.clone();
                    let output = local.path().join(format!("{mode}-{round}"));
                    std::fs::create_dir(&output).unwrap();
                    let file_output = if mode == "official" { output.join(basename(&remote)) } else { output.clone() };
                    for item in &mut download_items { item.local_path = file_output.join(basename(&item.remote_path)).to_string_lossy().into_owned(); }
                    for (upload, transfer_items) in [(true, &items), (false, &download_items)] {
                        let started = std::time::Instant::now();
                        match mode {
                            "bulk" => dag(transfer_items, upload).await?,
                            "per-file" => per_file(transfer_items, upload).await?,
                            _ => {
                                let mut args: Vec<String> = ["filesystem", if upload { "upload" } else { "download" }, "-j", "-f", if upload { "create-new-revision" } else { "remove" }, "-d", "merge"].into_iter().map(str::to_string).collect();
                                if upload { args.extend(transfer_items.iter().map(|item| item.local_path.clone())); }
                                else { args.push(remote.clone()); }
                                args.push(if upload { remote.clone() } else { output.to_string_lossy().into_owned() });
                                let answer = admin.run_bulk_once(&args).await?;
                                if upload {
                                    let remotes: Vec<_> = transfer_items.iter().map(|i| i.remote_path.clone()).collect();
                                    let outcomes = bulk_summary_outcomes(&String::from_utf8_lossy(&answer.stdout), &remotes, answer.status.success(), true);
                                    ensure(outcomes.iter().all(|o| matches!(o, BulkTransferOutcome::Completed)), "Direct CLI upload failed")?;
                                } else {
                                    let summary: Value = serde_json::from_slice(&answer.stdout).map_err(|e| ProviderError::ParseError(e.to_string()))?;
                                    ensure(answer.status.success() && summary.get("failedItems").and_then(Value::as_u64) == Some(0), "Direct CLI folder download failed")?;
                                }
                            }
                        }
                        let seconds = started.elapsed().as_secs_f64();
                        if !upload {
                            for item in &download_items {
                                let expected = std::fs::read(input.join(basename(&item.remote_path))).map_err(ProviderError::IoError)?;
                                let actual = std::fs::read(&item.local_path).map_err(ProviderError::IoError)?;
                                ensure(expected == actual, "Live readback differs")?;
                            }
                        }
                        let row = serde_json::json!({"round":round,"mode":mode,"direction":if upload {"upload"} else {"download"},"files":200,"bytes":200*4096,"seconds":seconds,"integrity":if upload {"readback_pending"} else {"200_byte_identical"}});
                        println!("BULK_LIVE_RESULT {row}");
                        rows.push(row);
                        if let Ok(path) = std::env::var("AEROFTP_BULK_BENCH_OUTPUT") { std::fs::write(path, serde_json::to_vec_pretty(&serde_json::json!({"root":root,"results":rows})).unwrap()).map_err(ProviderError::IoError)?; }
                    }
                }
            }
            Ok(())
        }.await;
        let cleanup = admin.delete_permanent(&root).await;
        println!("BULK_LIVE_CLEANUP {cleanup:?}");
        assert!(
            matches!(cleanup, Ok(true)),
            "Only the generated root must be cleaned: {root}"
        );
        result.unwrap();
    }

    #[tokio::test]
    async fn bulk_provider_executor_and_shared_dag_keep_file_events() {
        use crate::provider_transfer_executor::{
            resolve_provider_transfer_runtime, ProviderUploadExecutor,
        };
        use crate::transfer_domain::{TransferBatchConfig, TransferDirection, TransferEntry};
        use crate::transfer_event_sink::TransferEventSink;
        use crate::transfer_orchestrator::{execute_batch, TransferBatch};
        use std::sync::{atomic::AtomicBool, Arc, Mutex};
        #[derive(Default)]
        struct Events(Mutex<Vec<crate::TransferEvent>>);
        impl TransferEventSink for Events {
            fn emit_transfer_event(&self, event: crate::TransferEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let items = bulk_upload_items(dir.path(), 200);
        let provider: Arc<tokio::sync::Mutex<Option<Box<dyn StorageProvider>>>> =
            Arc::new(tokio::sync::Mutex::new(Some(Box::new(provider(&shim)))));
        let (settings, model, caps) =
            resolve_provider_transfer_runtime(&provider, Default::default()).await;
        assert_eq!(caps.bulk_upload, crate::transfer_dag::Capability::Supported);
        assert_eq!(
            caps.session_pool,
            crate::transfer_dag::Capability::Unsupported
        );
        let events = Arc::new(Events::default());
        let sink: Arc<dyn TransferEventSink> = events.clone();
        let executor = Arc::new(ProviderUploadExecutor::new(
            Arc::clone(&sink),
            provider,
            settings,
            None,
            tokio_util::sync::CancellationToken::new(),
            model,
            caps,
        ));
        let entries = items
            .iter()
            .enumerate()
            .map(|(i, item)| TransferEntry {
                id: format!("file-{i}"),
                display_name: basename(&item.remote_path),
                local_path: item.local_path.clone(),
                remote_path: item.remote_path.clone(),
                size: item.size,
                modified: None,
            })
            .collect();
        let result = execute_batch(
            sink,
            TransferBatch {
                id: "bulk-shim".into(),
                display_name: "Bulk".into(),
                direction: TransferDirection::Upload,
                config: TransferBatchConfig {
                    max_concurrent: 1,
                    ..Default::default()
                },
                entries,
            },
            executor,
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(
            (result.completed, result.failed, result.total),
            (200, 0, 200)
        );
        assert_eq!(verbs(&read_argv(dir.path()), "upload").len(), 1);
        let events = events.0.lock().unwrap();
        for i in 0..200 {
            let file: Vec<_> = events
                .iter()
                .filter(|e| e.transfer_id == format!("file-{i}"))
                .collect();
            assert_eq!(
                file.iter().filter(|e| e.event_type == "file_start").count(),
                1
            );
            assert_eq!(file.first().unwrap().progress.as_ref().unwrap().total, 4);
            assert_eq!(
                file.iter()
                    .filter(|e| e.event_type == "file_complete")
                    .count(),
                1
            );
            assert_eq!(file.last().unwrap().event_type, "file_complete");
        }
    }

    #[tokio::test]
    async fn bulk_upload_200_files_uses_one_process_and_per_file_outcomes() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let items = bulk_upload_items(dir.path(), 200);
        let outcomes = provider(&shim).upload_bulk(&items, None).await.unwrap();
        assert_eq!(outcomes.len(), 200);
        assert!(outcomes
            .iter()
            .all(|r| matches!(r, BulkTransferOutcome::Completed)));
        let argv = read_argv(dir.path());
        assert_eq!(verbs(&argv, "upload").len(), 1);
        assert_eq!(argv[0].len(), 208); // command, verb, JSON, two strategies, 200 files, parent
        assert!(!std::fs::read_to_string(dir.path().join("env.log"))
            .unwrap()
            .contains("AEROFTP_"));
    }

    #[tokio::test]
    async fn bulk_partial_failure_is_attributed_without_replaying_successes() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let items = bulk_upload_items(dir.path(), 3);
        std::fs::write(dir.path().join("bulk-summary.json"), r#"{"transferredItems":2,"skippedItems":0,"failedItems":1,"failures":[{"name":"file-001.txt","error":"Error: 429 Too many requests"}]}"#).unwrap();
        let outcomes = provider(&shim).upload_bulk(&items, None).await.unwrap();
        assert!(matches!(&outcomes[0], BulkTransferOutcome::Completed));
        assert!(matches!(&outcomes[1], BulkTransferOutcome::Failed(_)));
        assert!(matches!(&outcomes[2], BulkTransferOutcome::Completed));
        assert_eq!(
            verbs(&read_argv(dir.path()), "upload").len(),
            1,
            "no whole-bulk retry on transient failure"
        );
    }

    #[tokio::test]
    async fn bulk_malformed_summary_never_reports_unproven_success() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let items = bulk_upload_items(dir.path(), 2);
        std::fs::write(
            dir.path().join("bulk-summary.json"),
            r#"{"transferredItems":1,"skippedItems":0,"failedItems":0,"failures":[]}"#,
        )
        .unwrap();
        let results = provider(&shim).upload_bulk(&items, None).await.unwrap();
        assert!(results
            .iter()
            .all(|r| matches!(r, BulkTransferOutcome::Uncertain(_))));
        assert_eq!(verbs(&read_argv(dir.path()), "upload").len(), 1);
    }

    #[tokio::test]
    async fn bulk_download_publishes_successes_and_keeps_failed_destination() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let items: Vec<_> = (0..2)
            .map(|i| {
                let name = format!("file-{i}.txt");
                let local = dir.path().join(&name);
                std::fs::write(&local, "OLD").unwrap();
                BulkTransferItem {
                    local_path: local.to_string_lossy().into_owned(),
                    remote_path: format!("/my-files/bulk/{name}"),
                    size: 14,
                }
            })
            .collect();
        std::fs::write(dir.path().join("bulk-summary.json"), r#"{"transferredItems":1,"skippedItems":0,"failedItems":1,"failures":[{"name":"file-1.txt","error":"Error: not found"}]}"#).unwrap();
        let results = provider(&shim).download_bulk(&items, None).await.unwrap();
        assert!(matches!(&results[0], BulkTransferOutcome::Completed));
        assert!(matches!(&results[1], BulkTransferOutcome::Failed(_)));
        assert_eq!(
            std::fs::read_to_string(&items[0].local_path).unwrap(),
            "REMOTE-CONTENT"
        );
        assert_eq!(
            std::fs::read_to_string(&items[1].local_path).unwrap(),
            "OLD"
        );
        assert_eq!(verbs(&read_argv(dir.path()), "download").len(), 1);
        assert!(!std::fs::read_dir(dir.path()).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".aeroftp-proton-bulk-")));
    }

    #[tokio::test]
    async fn bulk_download_drop_kills_native_child_and_cleans_staging() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "OLD").unwrap();
        std::fs::write(dir.path().join("transfer-sleep"), "10").unwrap();
        let items = vec![BulkTransferItem {
            local_path: path.to_string_lossy().into_owned(),
            remote_path: "/my-files/bulk/file.txt".into(),
            size: 14,
        }];
        let mut p = provider(&shim);
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(300),
            p.download_bulk(&items, None)
        )
        .await
        .is_err());
        let pid: i32 = std::fs::read_to_string(dir.path().join("transfer.pid"))
            .unwrap()
            .parse()
            .unwrap();
        // tokio reaps the killed child asynchronously.
        for _ in 0..40 {
            if unsafe { libc::kill(pid, 0) } != 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "native child must not outlive cancellation"
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "OLD");
        assert!(!std::fs::read_dir(dir.path()).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".aeroftp-proton-bulk-")));
    }

    #[tokio::test]
    async fn bulk_hands_back_collisions_changed_sources_and_other_parents_unattempted() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let mut items = bulk_upload_items(dir.path(), 3);
        items[1].remote_path = "/my-files/other/file-001.txt".into();
        items[2] = items[0].clone();
        let outcomes = provider(&shim).upload_bulk(&items, None).await.unwrap();
        assert!(matches!(outcomes[0], BulkTransferOutcome::Completed));
        assert!(matches!(outcomes[1], BulkTransferOutcome::NotAttempted(_)));
        assert!(matches!(outcomes[2], BulkTransferOutcome::NotAttempted(_)));
        let argv = read_argv(dir.path());
        assert_eq!(verbs(&argv, "upload").len(), 1);
        // A source that changed since planning goes back to the single-file
        // path, which sends what is there now.
        items = bulk_upload_items(dir.path(), 2);
        items[0].size = 8;
        let outcomes = provider(&shim).upload_bulk(&items, None).await.unwrap();
        assert!(matches!(outcomes[0], BulkTransferOutcome::NotAttempted(_)));
        assert!(matches!(outcomes[1], BulkTransferOutcome::Completed));
    }

    /// A 429 on one file of a 200-file bulk: v4.2.4 reported it as a final
    /// failure (exit 4), and the CLI then re-ran the whole command. Now only
    /// that file runs again, once, on the single-file path.
    #[tokio::test]
    async fn bulk_transient_item_failure_runs_that_file_again_on_its_own() {
        use crate::provider_transfer_executor::{
            resolve_provider_transfer_runtime, ProviderUploadExecutor,
        };
        use crate::transfer_domain::{TransferBatchConfig, TransferDirection, TransferEntry};
        use crate::transfer_event_sink::{NoopTransferSink, TransferEventSink};
        use crate::transfer_orchestrator::{execute_batch, TransferBatch};
        use std::sync::{atomic::AtomicBool, Arc};
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let items = bulk_upload_items(dir.path(), 200);
        std::fs::write(
            dir.path().join("bulk-summary-once.json"),
            r#"{"transferredItems":199,"skippedItems":0,"failedItems":1,"failures":[{"name":"file-042.txt","error":"Error: 429 Too many requests"}]}"#,
        )
        .unwrap();
        let provider: Arc<tokio::sync::Mutex<Option<Box<dyn StorageProvider>>>> =
            Arc::new(tokio::sync::Mutex::new(Some(Box::new(provider(&shim)))));
        let (settings, model, caps) =
            resolve_provider_transfer_runtime(&provider, Default::default()).await;
        let sink: Arc<dyn TransferEventSink> = Arc::new(NoopTransferSink);
        let result = execute_batch(
            Arc::clone(&sink),
            TransferBatch {
                id: "bulk-retry".into(),
                display_name: "Bulk".into(),
                direction: TransferDirection::Upload,
                config: TransferBatchConfig {
                    max_concurrent: 1,
                    ..Default::default()
                },
                entries: items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| TransferEntry {
                        id: i.to_string(),
                        display_name: basename(&item.remote_path),
                        local_path: item.local_path.clone(),
                        remote_path: item.remote_path.clone(),
                        size: item.size,
                        modified: None,
                    })
                    .collect(),
            },
            Arc::new(ProviderUploadExecutor::new(
                sink,
                provider,
                settings,
                None,
                tokio_util::sync::CancellationToken::new(),
                model,
                caps,
            )),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!((result.completed, result.failed), (200, 0));
        let argv = read_argv(dir.path());
        let uploads = verbs(&argv, "upload");
        assert_eq!(uploads.len(), 2);
        assert!(uploads[1].iter().any(|a| a.ends_with("file-042.txt")));
        assert!(!uploads[1].iter().any(|a| a.ends_with("file-041.txt")));
    }

    /// `put -r ./dir` passes relative local paths. v4.2.4 kept them all on the
    /// per-file lane; they now reach the native bulk in their absolute form.
    #[tokio::test]
    async fn bulk_upload_accepts_relative_sources_as_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let items = bulk_upload_items(dir.path(), 3);
        let cwd = std::env::current_dir().unwrap();
        let relative: Vec<_> = items
            .iter()
            .map(|item| {
                let rel = pathdiff_from(&cwd, Path::new(&item.local_path));
                crate::transfer_bulk::bulk_item(&rel, &item.remote_path, item.size)
            })
            .collect();
        assert!(relative
            .iter()
            .all(|i| Path::new(&i.local_path).is_absolute()));
        let outcomes = provider(&shim).upload_bulk(&relative, None).await.unwrap();
        assert!(outcomes
            .iter()
            .all(|r| matches!(r, BulkTransferOutcome::Completed)));
        assert_eq!(verbs(&read_argv(dir.path()), "upload").len(), 1);
    }

    /// `a/b/c` relative to `base`, through `..` where needed.
    fn pathdiff_from(base: &Path, target: &Path) -> String {
        let base: Vec<_> = base.components().collect();
        let target: Vec<_> = target.components().collect();
        let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();
        let mut out = PathBuf::new();
        for _ in common..base.len() {
            out.push("..");
        }
        for part in &target[common..] {
            out.push(part.as_os_str());
        }
        out.to_string_lossy().into_owned()
    }

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

    /// Runs the connection-form status probe with the CLI named by
    /// AEROFTP_PROTON_CLI, inside the environment lock.
    fn status_with(cli: &Path, signed_out: bool) -> (bool, bool) {
        let _env = crate::test_env::lock();
        std::env::set_var("AEROFTP_PROTON_CLI", cli);
        let marker = cli.parent().unwrap().join("signed_out");
        if signed_out {
            std::fs::write(&marker, b"").unwrap();
        }
        let status = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(cli_session_status());
        std::env::remove_var("AEROFTP_PROTON_CLI");
        let _ = std::fs::remove_file(&marker);
        status
    }

    #[test]
    fn the_status_probe_tells_installed_and_signed_in_apart() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        assert_eq!(status_with(&shim, false), (true, true), "signed in");
        assert_eq!(
            status_with(&shim, true),
            (true, false),
            "installed, no session"
        );
    }

    #[test]
    fn the_status_probe_reports_a_missing_cli() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("proton-drive");
        // Only when nothing else is installed on the machine: PATH and the
        // usual locations are searched after the override.
        if resolve_proton_drive(None) != "proton-drive" {
            eprintln!("SKIPPED: a real proton-drive is installed on this machine");
            return;
        }
        assert_eq!(status_with(&missing, false), (false, false));
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

    /// Requested 79: a CLI-mode session opens in My Files, where a native
    /// profile opens, instead of the list of sections; relative paths resolve
    /// there, the sections stay one `cd ..` away, and a disconnect returns to
    /// My Files for the next session.
    #[tokio::test]
    async fn a_session_starts_in_my_files_and_the_sections_stay_reachable() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let mut p = provider(&shim);
        p.connected = false;
        p.connect().await.unwrap();
        assert_eq!(p.pwd().await.unwrap(), START_DIR);
        p.list("docs").await.unwrap();
        let argv = read_argv(dir.path());
        let listed: Vec<&str> = verbs(&argv, "list")
            .iter()
            .filter_map(|a| a.get(2).map(String::as_str))
            .collect();
        assert_eq!(listed.last(), Some(&"/my-files/docs"));
        p.cd("..").await.unwrap();
        assert_eq!(p.pwd().await.unwrap(), "/");
        p.disconnect().await.unwrap();
        assert_eq!(p.pwd().await.unwrap(), START_DIR);
    }

    fn provider(shim: &Path) -> ProtonCliProvider {
        let mut p = ProtonCliProvider::new(ProtonConfig {
            display_name: "t".into(),
            binary_path: Some(shim.to_string_lossy().to_string()),
            connection_mode: ProtonConnectionMode::Cli,
            profile_id: None,
            username: None,
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

    /// proton-drive globs a local path holding * ? [ or {: a file named
    /// with brackets matched nothing and was never uploaded. The path now
    /// reaches it with those characters inside classes, which the glob
    /// matches literally (the shim globs like the CLI).
    #[test]
    fn local_paths_and_names_follow_the_clis_rules() {
        assert_eq!(
            literal_local_path_for_cli("/a/b c.txt").unwrap(),
            "/a/b c.txt"
        );
        assert_eq!(
            literal_local_path_for_cli("/a (1).txt").unwrap(),
            "/a (1).txt"
        );
        assert_eq!(
            literal_local_path_for_cli("/d [1]/f{2}*?(3).txt").unwrap(),
            "/d [[]1]/f[{]2}[*][?][(]3).txt"
        );
        assert_eq!(cli_local_name("plain.txt"), "plain.txt");
        assert_eq!(
            cli_local_name("a\"b?c*d|e<f>g:h.txt"),
            "a_b_c_d_e_f_g_h.txt"
        );
        assert_eq!(cli_local_name("tab\there"), "tab_here");
        assert_eq!(cli_local_name(".."), "__");
        assert_eq!(cli_local_name("."), "_");
        if cfg!(windows) {
            assert_eq!(cli_local_name("con.txt"), "_con.txt");
            assert_eq!(cli_local_name("COM1"), "_COM1");
            assert_eq!(cli_local_name("COM0"), "COM0");
            assert_eq!(cli_local_name("trail. ."), "trail___");
        } else {
            assert_eq!(cli_local_name("con.txt"), "con.txt");
            assert_eq!(cli_local_name("trail. ."), "trail. .");
        }
    }

    #[tokio::test]
    async fn a_local_name_with_glob_characters_is_uploaded_literally() {
        let dir = workdir();
        let shim = link_shim(&dir);
        let local = dir.join("brackets [x] {y} (z) *?.txt");
        std::fs::write(&local, "CONTENT").unwrap();
        let mut p = provider(&shim);
        p.upload(
            local.to_str().unwrap(),
            "/my-files/scratch/brackets [x] {y} (z) *?.txt",
            None,
        )
        .await
        .unwrap();
        let argv = read_argv(&dir);
        let uploads = verbs(&argv, "upload");
        assert_eq!(uploads.len(), 1, "{argv:?}");
        let local_arg = uploads[0].iter().rev().nth(1).unwrap();
        assert!(
            local_arg.ends_with("brackets [[]x] [{]y} [(]z) [*][?].txt"),
            "{local_arg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The download folder is globbed too: one with brackets in its name
    /// matched nothing.
    #[tokio::test]
    async fn a_download_into_a_folder_with_glob_characters_lands() {
        let dir = workdir();
        let shim = link_shim(&dir);
        let dest = dir.join("dest [1] {2}");
        std::fs::create_dir_all(&dest).unwrap();
        let mut p = provider(&shim);
        p.download(
            "/my-files/scratch/notes.txt",
            dest.join("notes.txt").to_str().unwrap(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("notes.txt")).unwrap(),
            "REMOTE-CONTENT"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// proton-drive writes `"` and `?` as `_` on Linux too: the file was
    /// downloaded, then not found under its own name.
    #[tokio::test]
    async fn a_download_finds_the_file_under_the_name_the_cli_wrote() {
        let dir = workdir();
        let shim = link_shim(&dir);
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let mut p = provider(&shim);
        p.download(
            "/my-files/scratch/apos'trophe \"quote\" q?.txt",
            dest.to_str().unwrap(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("apos'trophe \"quote\" q?.txt")).unwrap(),
            "REMOTE-CONTENT"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// On Unix a backslash in a globbed path has no escape the CLI accepts:
    /// the upload is refused with the reason, before any CLI run.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn a_globbed_local_path_with_a_backslash_is_refused_before_the_cli() {
        let dir = workdir();
        let shim = link_shim(&dir);
        let local = dir.join("back\\slash [k].txt");
        std::fs::write(&local, "CONTENT").unwrap();
        let mut p = provider(&shim);
        let error = p
            .upload(
                local.to_str().unwrap(),
                "/my-files/scratch/back\\slash [k].txt",
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ProviderError::InvalidPath(_)), "{error:?}");
        assert!(verbs(&read_argv(&dir), "upload").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn proton_cli_lists_raw_control_names_and_restores_them_after_download() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let mut p = provider(&shim);
        let names = [
            "tab\tname.txt",
            "line\nname.txt",
            "del\u{7f}name.txt",
            "c1\u{85}name.txt",
        ];
        let nodes: Vec<_> = names.iter().map(|name| serde_json::json!({
            "name": {"ok": true, "value": name}, "type": "file", "activeRevision": {"claimedSize": 14}
        })).collect();
        std::fs::write(
            dir.path().join("my-files_scratch.json"),
            serde_json::to_vec(&nodes).unwrap(),
        )
        .unwrap();
        let listed = p.list("/my-files/scratch").await.unwrap();
        assert_eq!(
            listed.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            names
        );
        for name in names {
            let remote = format!("/my-files/scratch/{name}");
            let local = dir.path().join(name);
            p.download(&remote, local.to_str().unwrap(), None)
                .await
                .unwrap();
            assert_eq!(std::fs::read(&local).unwrap(), b"REMOTE-CONTENT");
            assert!(
                !dir.path().join(cli_local_name(name)).exists() || cli_local_name(name) == name
            );
        }
    }

    #[tokio::test]
    async fn filename_whitespace_survives_upload_bulk_mkdir_rename_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let shim = link_shim(dir.path());
        let local = dir.path().join("source.txt");
        std::fs::write(&local, b"DATA").unwrap();
        let mut p = provider(&shim);
        for name in [
            " leading.txt",
            "trailing.txt ",
            " both.txt ",
            " ",
            "\tname\t",
        ] {
            let remote = format!("/my-files/scratch/{name}");
            p.upload(local.to_str().unwrap(), &remote, None)
                .await
                .unwrap();
            let argv = read_argv(dir.path());
            let uploads = verbs(&argv, "upload");
            assert_eq!(
                Path::new(uploads.last().unwrap().iter().rev().nth(1).unwrap())
                    .file_name()
                    .unwrap(),
                name
            );
            let bulk_local = dir.path().join(name);
            std::fs::write(&bulk_local, b"DATA").unwrap();
            let before = verbs(&read_argv(dir.path()), "upload").len();
            let outcomes = p
                .upload_bulk(
                    &[BulkTransferItem {
                        local_path: bulk_local.to_string_lossy().into_owned(),
                        remote_path: remote.clone(),
                        size: 4,
                    }],
                    None,
                )
                .await
                .unwrap();
            if plain_bulk_name(&remote).is_some() {
                assert!(matches!(outcomes[0], BulkTransferOutcome::Completed));
                let argv = read_argv(dir.path());
                let uploads = verbs(&argv, "upload");
                assert_eq!(uploads.len(), before + 1);
                assert_eq!(
                    Path::new(uploads.last().unwrap().iter().rev().nth(1).unwrap())
                        .file_name()
                        .unwrap(),
                    name
                );
            } else {
                assert!(matches!(outcomes[0], BulkTransferOutcome::NotAttempted(_)));
                assert_eq!(verbs(&read_argv(dir.path()), "upload").len(), before);
                p.upload(bulk_local.to_str().unwrap(), &remote, None)
                    .await
                    .unwrap();
                let argv = read_argv(dir.path());
                let uploads = verbs(&argv, "upload");
                assert_eq!(
                    Path::new(uploads.last().unwrap().iter().rev().nth(1).unwrap())
                        .file_name()
                        .unwrap(),
                    name
                );
            }
            p.mkdir(&remote).await.unwrap();
            let argv = read_argv(dir.path());
            assert_eq!(
                verbs(&argv, "create-folder")
                    .last()
                    .unwrap()
                    .last()
                    .unwrap(),
                name
            );
            std::fs::write(
                dir.path().join("existing.json"),
                r#"["/my-files/scratch/source.txt"]"#,
            )
            .unwrap();
            p.rename("/my-files/scratch/source.txt", &remote)
                .await
                .unwrap();
            let argv = read_argv(dir.path());
            assert_eq!(verbs(&argv, "rename").last().unwrap().last().unwrap(), name);
            p.list(&remote).await.unwrap();
            let argv = read_argv(dir.path());
            assert_eq!(verbs(&argv, "list").last().unwrap()[2], remote);
        }
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

    /// The CLI says nothing while it downloads, and the download opened
    /// with `(0, 0)`, which reads as a transfer already done, even when the
    /// download then failed. The only report is now the completed file, once
    /// it is in place, and a failed download reports nothing.
    #[tokio::test]
    async fn download_reports_only_the_completed_file() {
        let dir = workdir();
        let shim = link_shim(&dir);
        let dest = dir.join("got.txt");
        let mut p = provider(&shim);
        let (callback, updates) = crate::providers::upload_progress::fixture::recorder();
        p.download(
            "/my-files/scratch/report.txt",
            dest.to_str().unwrap(),
            Some(callback),
        )
        .await
        .unwrap();
        let size = "REMOTE-CONTENT".len() as u64;
        assert_eq!(*updates.lock().unwrap(), [(size, size)]);

        std::fs::write(dir.join("signed_out"), b"").unwrap();
        let (callback, updates) = crate::providers::upload_progress::fixture::recorder();
        let outcome = p
            .download(
                "/my-files/scratch/report.txt",
                dest.to_str().unwrap(),
                Some(callback),
            )
            .await;
        assert!(outcome.is_err(), "{outcome:?}");
        assert!(
            updates.lock().unwrap().is_empty(),
            "{:?}",
            updates.lock().unwrap()
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
    async fn delete_permanent_refuses_a_duplicate_trash_name_even_with_a_captured_uid() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("trash.json"),
            r#"[{"name":{"ok":true,"value":"dup.txt"},"uid":"CCCCCCCCCCCCCCCCCCCCCC~DDDDDDDDDDDDDDDDDDDDDD","type":"file"},{"name":{"ok":true,"value":"dup.txt"},"uid":"AAAAAAAAAAAAAAAAAAAAAA~BBBBBBBBBBBBBBBBBBBBBB","type":"file"}]"#,
        )
        .unwrap();
        let mut p = provider(&shim);
        let error = p
            .delete_permanent("/my-files/scratch/dup.txt")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("name only"), "{error}");
        let argv = read_argv(&dir);
        assert!(verbs(&argv, "delete").is_empty(), "{argv:?}");
        assert_eq!(verbs(&argv, "trash").len(), 1, "{argv:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn delete_permanent_purges_by_name_when_unique_trash_uid_matches() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("trash.json"),
            r#"[{"name":{"ok":true,"value":"dup.txt"},"uid":"AAAAAAAAAAAAAAAAAAAAAA~BBBBBBBBBBBBBBBBBBBBBB","type":"file"}]"#,
        )
        .unwrap();
        let mut p = provider(&shim);
        assert!(p
            .delete_permanent("/my-files/scratch/dup.txt")
            .await
            .unwrap());
        let argv = read_argv(&dir);
        let deletes = verbs(&argv, "delete");
        assert_eq!(deletes.len(), 1, "{argv:?}");
        assert_eq!(
            deletes[0].last().map(|s| s.as_str()),
            Some("/trash/dup.txt")
        );
        assert_eq!(argv.iter().filter(|args| args[0] == "version").count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn trash_identity_capture_uses_one_listing_and_info_only_for_missing_names() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("my-files_scratch_manyfiles.json"),
            r#"[{"name":{"ok":true,"value":"listed.dat"},"uid":"AAAAAAAAAAAAAAAAAAAAAA~BBBBBBBBBBBBBBBBBBBBBB","type":"file"}]"#,
        )
        .unwrap();
        let mut p = provider(&shim);
        let paths = vec![
            "/my-files/scratch/manyfiles/listed.dat".to_string(),
            "/my-files/scratch/manyfiles/missing.dat".to_string(),
        ];
        let captured = p.capture_trash_identities(&paths).await;
        assert_eq!(captured.len(), 2);
        assert!(captured.iter().all(|uid| matches!(uid, Ok(Some(_)))));
        let argv = read_argv(&dir);
        assert_eq!(verbs(&argv, "list").len(), 1, "{argv:?}");
        let infos = verbs(&argv, "info");
        assert_eq!(infos.len(), 1, "{argv:?}");
        assert!(infos[0].iter().any(|arg| arg.ends_with("missing.dat")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn identity_purges_verify_name_and_uid_with_one_trash_listing() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("trash.json"),
            r#"[{"name":{"ok":true,"value":"dup.txt"},"uid":"AAAAAAAAAAAAAAAAAAAAAA~BBBBBBBBBBBBBBBBBBBBBB","type":"file"},{"name":{"ok":true,"value":"other.txt"},"uid":"CCCCCCCCCCCCCCCCCCCCCC~DDDDDDDDDDDDDDDDDDDDDD","type":"file"}]"#,
        )
        .unwrap();
        let mut p = provider(&shim);
        for (name, uid) in [
            ("dup.txt", "AAAAAAAAAAAAAAAAAAAAAA~BBBBBBBBBBBBBBBBBBBBBB"),
            ("other.txt", "CCCCCCCCCCCCCCCCCCCCCC~DDDDDDDDDDDDDDDDDDDDDD"),
        ] {
            assert!(p
                .delete_permanent_with_identity(&format!("/my-files/scratch/{name}"), Some(uid))
                .await
                .unwrap());
        }
        let argv = read_argv(&dir);
        let deletes = verbs(&argv, "delete");
        assert_eq!(deletes.len(), 2, "{argv:?}");
        assert_eq!(
            deletes[0].last().map(|s| s.as_str()),
            Some("/trash/dup.txt")
        );
        assert_eq!(
            deletes[1].last().map(|s| s.as_str()),
            Some("/trash/other.txt")
        );
        let trash_lists: Vec<_> = verbs(&argv, "list")
            .into_iter()
            .filter(|args| args.iter().any(|arg| arg == "/trash"))
            .collect();
        assert_eq!(trash_lists.len(), 1, "{argv:?}");
        assert_eq!(argv.iter().filter(|args| args[0] == "version").count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn identity_purge_refuses_duplicate_names_without_deleting() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("trash.json"),
            r#"[{"name":{"ok":true,"value":"dup.txt"},"uid":"CCCCCCCCCCCCCCCCCCCCCC~DDDDDDDDDDDDDDDDDDDDDD","type":"file"},{"name":{"ok":true,"value":"dup.txt"},"uid":"AAAAAAAAAAAAAAAAAAAAAA~BBBBBBBBBBBBBBBBBBBBBB","type":"file"}]"#,
        )
        .unwrap();
        let mut p = provider(&shim);
        let error = p
            .delete_permanent_with_identity(
                "/my-files/scratch/dup.txt",
                Some("AAAAAAAAAAAAAAAAAAAAAA~BBBBBBBBBBBBBBBBBBBBBB"),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("name only"), "{error}");
        let argv = read_argv(&dir);
        assert!(verbs(&argv, "delete").is_empty(), "{argv:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn identity_purge_refuses_a_uid_mismatch_without_deleting() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("trash.json"),
            r#"[{"name":{"ok":true,"value":"dup.txt"},"uid":"CCCCCCCCCCCCCCCCCCCCCC~DDDDDDDDDDDDDDDDDDDDDD","type":"file"}]"#,
        )
        .unwrap();
        let mut p = provider(&shim);
        let error = p
            .delete_permanent_with_identity(
                "/my-files/scratch/dup.txt",
                Some("AAAAAAAAAAAAAAAAAAAAAA~BBBBBBBBBBBBBBBBBBBBBB"),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("name only"), "{error}");
        let argv = read_argv(&dir);
        assert!(verbs(&argv, "delete").is_empty(), "{argv:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `/` lists the account sections and is not a folder: a write there, or a
    /// rename, move, copy or delete of a section, is refused with a clear
    /// message before the CLI runs. Reading the root and the sections, and
    /// writing inside a section, still work.
    #[tokio::test]
    async fn the_root_takes_no_writes_and_sections_stay_fixed() {
        let dir = workdir();
        let shim = link_shim(&dir);
        let mut p = provider(&shim);
        let local = dir.join("IMG_1.jpg");
        std::fs::write(&local, b"x").unwrap();
        let local = local.to_string_lossy().into_owned();

        let refused: Vec<(&str, ProviderError)> = vec![
            (
                "upload to /",
                p.upload(&local, "/IMG_1.jpg", None).await.unwrap_err(),
            ),
            ("mkdir in /", p.mkdir("/new").await.unwrap_err()),
            ("delete a section", p.delete("/my-files").await.unwrap_err()),
            (
                "purge a section",
                p.delete_permanent("/photos").await.unwrap_err(),
            ),
            (
                "rename a section",
                p.rename("/devices", "/devices2").await.unwrap_err(),
            ),
            (
                "move into /",
                p.rename("/my-files/a.txt", "/a.txt").await.unwrap_err(),
            ),
            (
                "copy a section",
                p.server_copy("/trash", "/my-files/t").await.unwrap_err(),
            ),
            (
                "copy into /",
                p.server_copy("/my-files/a.txt", "/a.txt")
                    .await
                    .unwrap_err(),
            ),
        ];
        for (what, err) in &refused {
            assert!(
                matches!(err, ProviderError::PermissionDenied(_)),
                "{what}: expected a clear refusal, got {err:?}"
            );
        }
        let argv = read_argv(&dir);
        assert!(
            argv.is_empty(),
            "the CLI must not run for a refused write: {argv:?}"
        );

        p.list("/").await.unwrap();
        p.list("/shared-with-me").await.unwrap();
        p.upload(&local, "/my-files/IMG_1.jpg", None).await.unwrap();
        assert_eq!(verbs(&read_argv(&dir), "upload").len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A used-storage scan of the root counts the account's own storage only:
    /// never the shared views (repeats and other people's files) nor albums.
    /// Photos, which the CLI cannot open yet, are counted as an unreadable
    /// folder, so the figure is declared a lower bound instead of a total.
    #[tokio::test]
    async fn a_root_scan_counts_own_storage_and_names_what_it_could_not_read() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("my-files.json"),
            r#"[{"name":{"ok":true,"value":"a.bin"},"uid":"U1","type":"file","activeRevision":{"claimedSize":1000}}]"#,
        )
        .unwrap();
        let mut p: Box<dyn super::super::StorageProvider> = Box::new(provider(&shim));
        let never = std::sync::atomic::AtomicBool::new(false);
        let scan = crate::used_scan::scan_used_bytes(&mut p, "/", None, 10_000, &never, |_, _| {})
            .await
            .unwrap();
        let listed: Vec<String> = verbs(&read_argv(&dir), "list")
            .iter()
            .filter_map(|a| a.iter().skip(2).find(|x| !x.starts_with('-')).cloned())
            .collect();
        for walked in [
            "/my-files",
            "/devices",
            "/photos",
            "/trash",
            "/photos-trash",
        ] {
            assert!(
                listed.iter().any(|l| l == walked),
                "{walked} not walked: {listed:?}"
            );
        }
        for skipped in [
            "/",
            "/shared-by-me",
            "/shared-with-me",
            "/albums",
            "/photos-shared-by-me",
            "/photos-shared-with-me",
        ] {
            assert!(
                !listed.iter().any(|l| l == skipped),
                "{skipped} must not be counted: {listed:?}"
            );
        }
        assert_eq!(scan.file_count, 1, "{scan:?}");
        assert_eq!(
            scan.unreadable_dirs, 1,
            "photos is unreadable with CLI 0.8.0: {scan:?}"
        );
        assert!(
            scan.truncated && !scan.hit_cap,
            "a lower bound, not a cap: {scan:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn opening_a_photo_section_says_the_cli_cannot_yet() {
        let dir = workdir();
        let shim = link_shim(&dir);
        let mut p = provider(&shim);
        let err = p.list("/photos").await.unwrap_err();
        match err {
            ProviderError::NotSupported(m) => assert!(m.contains("cannot open \"photos\""), "{m}"),
            other => panic!("expected a clear NotSupported, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `rename` and `move` commands the shim received, as argv lines.
    fn changes(dir: &Path) -> Vec<String> {
        read_argv(dir)
            .into_iter()
            .filter(|a| matches!(a.get(1).map(String::as_str), Some("rename" | "move")))
            .map(|a| a.join(" "))
            .collect()
    }

    /// A move to another folder onto a taken name moved the source there and
    /// only then had its rename refused: the source was left moved. It is now
    /// refused before any command.
    #[tokio::test]
    async fn a_move_onto_a_taken_name_is_refused_before_any_command() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("existing.json"),
            r#"["/my-files/src/a.txt", "/my-files/dst/b.txt"]"#,
        )
        .unwrap();
        let mut p = provider(&shim);
        let outcome = p.rename("/my-files/src/a.txt", "/my-files/dst/b.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert!(changes(&dir).is_empty(), "{:?}", changes(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When the rename after the move failed, the file stayed in the new
    /// folder under its old name while the error said nothing of it. The
    /// move is undone.
    #[tokio::test]
    async fn a_move_whose_rename_fails_is_moved_back() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(dir.join("existing.json"), r#"["/my-files/src/a.txt"]"#).unwrap();
        let mut p = provider(&shim);
        let outcome = p
            .rename("/my-files/src/a.txt", "/my-files/dst/fail.txt")
            .await;
        assert!(outcome.is_err(), "{outcome:?}");
        assert_eq!(
            changes(&dir),
            [
                "filesystem move /my-files/src/a.txt /my-files/dst",
                "filesystem rename /my-files/dst/a.txt -- fail.txt",
                "filesystem move /my-files/dst/a.txt /my-files/src",
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The move keeps the old name: with `/my-files/dst/a.txt` there, it
    /// would meet that file. The rename goes first, in the source folder.
    #[tokio::test]
    async fn a_move_whose_destination_holds_the_old_name_renames_first() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("existing.json"),
            r#"["/my-files/src/a.txt", "/my-files/dst/a.txt"]"#,
        )
        .unwrap();
        let mut p = provider(&shim);
        p.rename("/my-files/src/a.txt", "/my-files/dst/c.txt")
            .await
            .expect("rename then move");
        assert_eq!(
            changes(&dir),
            [
                "filesystem rename /my-files/src/a.txt -- c.txt",
                "filesystem move /my-files/src/c.txt /my-files/dst",
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn mkdir_rename_copy_put_dash_guard_before_bare_names() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(dir.join("existing.json"), r#"["/my-files/scratch/a.txt"]"#).unwrap();
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

    /// A share password starting with `-` went out as `--password -x`, two
    /// tokens, the form measured ambiguous for `copy -n`: the CLI could read
    /// the password as an option. It goes out as one `--password=-x` token,
    /// the form the CLI documents for a value that starts with `-`.
    #[tokio::test]
    async fn a_share_password_starting_with_a_dash_is_one_token() {
        let dir = workdir();
        let shim = link_shim(&dir);
        let mut p = provider(&shim);
        let link = p
            .create_share_link(
                "/my-files/a.txt",
                crate::providers::ShareLinkOptions {
                    password: Some("-x".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("share link");
        assert_eq!(link.password.as_deref(), Some("-x"));
        let argv = read_argv(&dir);
        let set_url = argv
            .iter()
            .find(|a| a.first().map(String::as_str) == Some("sharing"))
            .expect("sharing set-url ran");
        assert!(set_url.iter().any(|a| a == "--password=-x"), "{set_url:?}");
        assert!(!set_url.iter().any(|a| a == "--password"), "{set_url:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn share_link_accepts_cli_090_access_policy_response() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("share-response.json"),
            r#"{"members":[],"urlAccess":{"url":"https://drive.proton.me/urls/SHIM#key","customPassword":"-private"},"editorsCanShare":false}"#,
        )
        .unwrap();
        let link = provider(&shim)
            .create_share_link(
                "/my-files/a.txt",
                ShareLinkOptions {
                    password: Some("-private".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(link.url, "https://drive.proton.me/urls/SHIM#key");
        assert_eq!(link.password.as_deref(), Some("-private"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unrecognized_share_response_does_not_disclose_its_payload() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("share-response.json"),
            r#"{"futureAccess":{"customPassword":"private-sentinel","secret":"link-secret"}}"#,
        )
        .unwrap();
        let error = provider(&shim)
            .create_share_link("/my-files/a.txt", ShareLinkOptions::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Could not parse Proton share URL"));
        assert!(!error.contains("private-sentinel"));
        assert!(!error.contains("link-secret"));
        assert!(!error.contains("futureAccess"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn renamed_upload_hardlinks_even_when_the_file_exceeds_free_space() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("source.bin");
        let len = crate::filesystem::filesystem_space(root.path())
            .unwrap()
            .available
            .saturating_add(crate::util::scratch::SCRATCH_HEADROOM + 1);
        let file = std::fs::File::create(&local).unwrap();
        file.set_len(len).unwrap(); // sparse: no payload-sized disk allocation
        let scratch = root.path().join("scratch");
        assert!(crate::util::scratch::scratch_dir_with_room(&scratch, len).is_err());
        let (staging, staged) =
            stage_local_as_dest_in(local.to_str().unwrap(), "renamed.bin", &scratch).unwrap();
        assert_eq!(
            std::fs::metadata(&staged).unwrap().ino(),
            file.metadata().unwrap().ino()
        );
        assert_eq!(std::fs::metadata(&staged).unwrap().len(), len);
        std::fs::remove_dir_all(staging).unwrap();
        assert_eq!(file.metadata().unwrap().nlink(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn renamed_upload_cross_device_copy_checks_room_and_cleans_up_before_copying() {
        use std::os::unix::fs::MetadataExt;
        if !Path::new("/dev/shm").is_dir() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir_in("/dev/shm").unwrap();
        if std::fs::metadata(root.path()).unwrap().dev()
            == std::fs::metadata(scratch.path()).unwrap().dev()
        {
            return;
        }
        let local = root.path().join("source.bin");
        let len = crate::filesystem::filesystem_space(scratch.path())
            .unwrap()
            .available
            .saturating_add(crate::util::scratch::SCRATCH_HEADROOM + 1);
        let file = std::fs::File::create(&local).unwrap();
        file.set_len(len).unwrap();
        let probe = scratch.path().join("hardlink-probe");
        assert_eq!(
            std::fs::hard_link(&local, probe)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EXDEV)
        );
        let error = stage_local_as_dest_in(local.to_str().unwrap(), "renamed.bin", scratch.path())
            .unwrap_err();
        assert!(
            matches!(error, ProviderError::TransferFailed(_)),
            "{error:?}"
        );
        // The liveness control file is permanent; an upload folder or payload
        // must still be removed when copy preflight fails.
        let leftovers: Vec<_> = std::fs::read_dir(scratch.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            leftovers,
            [std::ffi::OsString::from("aeroftp_staging_live.lock")],
            "failed preflight left payload staging behind"
        );
        assert_eq!(file.metadata().unwrap().len(), len);
    }

    /// The upload staging folder lives in the on-disk scratch folder, never in
    /// the temporary folder (on many Linux systems a tmpfs sized from RAM, so
    /// a large upload staged there eats RAM and can fail).
    #[test]
    fn an_upload_is_staged_on_disk_not_in_the_temporary_folder() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("payload.bin");
        std::fs::write(&local, b"proton staging on disk").unwrap();

        let (staging, staged) = stage_local_as_dest(local.to_str().unwrap(), "dest.bin").unwrap();
        if dirs::cache_dir().is_some() {
            assert!(
                !staging.starts_with(std::env::temp_dir()),
                "{} must not be in the temporary folder",
                staging.display()
            );
            assert!(
                staging
                    .ancestors()
                    .any(|a| a.file_name() == Some(std::ffi::OsStr::new("scratch"))),
                "{} must live under a scratch folder",
                staging.display()
            );
        } else {
            // The documented fallback with no cache folder: scratch is
            // temp_dir()/aeroftp-scratch.
            assert!(
                staging.starts_with(std::env::temp_dir().join("aeroftp-scratch")),
                "{} must live under the scratch fallback",
                staging.display()
            );
        }
        assert!(staged.starts_with(&staging));
        assert_eq!(std::fs::read(&staged).unwrap(), b"proton staging on disk");
        let _ = std::fs::remove_dir_all(&staging);
    }

    /// proton-drive 0.9.0 opens its upgrade page in the browser after every
    /// upload that fails on a full drive, unless it runs with JSON, so a
    /// recursive upload to a full drive opened one tab per file. And the
    /// reason was only in the summary on stdout: the error read "1 item(s)
    /// failed to upload", which the transfer engine retries.
    #[tokio::test]
    async fn an_upload_to_a_full_drive_opens_no_browser_and_reads_as_quota() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("transfer-fails.json"),
            r#"[{"name":"a.txt","error":"ValidationError: Insufficient storage quota"}]"#,
        )
        .unwrap();
        let local = dir.join("a.txt");
        std::fs::write(&local, "x").unwrap();
        let mut p = provider(&shim);
        let err = p
            .upload(local.to_str().unwrap(), "/my-files/a.txt", None)
            .await
            .expect_err("the drive is full");
        assert!(
            !dir.join("browser.log").exists(),
            "the upload let proton-drive open the browser"
        );
        let refused = crate::transfer_dag::TransferError::from_provider(&err);
        assert_eq!(
            refused.kind,
            crate::transfer_dag::TransferErrorKind::QuotaExceeded,
            "{err}"
        );
        assert_eq!(
            refused.retry,
            crate::transfer_dag::RetryDirective::Never,
            "{err}"
        );
        assert_eq!(
            verbs(&read_argv(&dir), "upload").len(),
            1,
            "a full drive was retried"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failed_download_says_why_its_item_failed() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("transfer-fails.json"),
            r#"[{"name":"report.txt","error":"NotFoundAPIError: Node not found"}]"#,
        )
        .unwrap();
        let dest = dir.join("got.txt");
        let mut p = provider(&shim);
        let err = p
            .download("/my-files/report.txt", dest.to_str().unwrap(), None)
            .await
            .expect_err("the item failed");
        let text = err.to_string();
        assert!(
            text.contains("report.txt: NotFoundAPIError: Node not found"),
            "{text}"
        );
        let argv = read_argv(&dir);
        assert!(
            verbs(&argv, "download")[0].iter().any(|a| a == "-j"),
            "{argv:?}"
        );
        assert_eq!(
            verbs(&argv, "download").len(),
            1,
            "a missing item was retried: {argv:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A per-item timeout is in the JSON summary, not on stderr. It is
    /// transient, so the transfer is attempted again; the second run succeeds.
    #[tokio::test]
    async fn a_transient_item_failure_is_retried() {
        let dir = workdir();
        let shim = link_shim(&dir);
        std::fs::write(
            dir.join("transfer-fails-once.json"),
            r#"[{"name":"a.txt","error":"APICodeError: request timed out"}]"#,
        )
        .unwrap();
        let local = dir.join("a.txt");
        std::fs::write(&local, "x").unwrap();
        let mut p = provider(&shim);
        p.upload(local.to_str().unwrap(), "/my-files/a.txt", None)
            .await
            .expect("the timed-out item was not retried");
        let argv = read_argv(&dir);
        let uploads = verbs(&argv, "upload");
        assert_eq!(uploads.len(), 2, "expected one retry, got {uploads:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
