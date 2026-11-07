// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Native bulk transfers through MEGAcmd.
//!
//! The MEGAcmd server, not the `mega-put` / `mega-get` client, runs the
//! transfers, and it outlives the client: killing the client cancels nothing.
//! So a bulk owns its transfers by their tags, proves every file from the
//! server's own record of terminal states, and cancels what it owns, one tag
//! at a time, when it is dropped. Measured on MEGAcmd 2.6.0.2 (2026-11-05):
//!
//! - `mega-put --print-tag-at-start a b c dir/` prints one
//!   `Upload started: Tag = N. Source = <path>` line per source, then waits
//!   for all of them.
//! - `mega-transfers --show-completed` keeps COMPLETED, CANCELLED and FAILED
//!   per tag after the end, in the server's memory only: a server restart
//!   loses the record, and then nothing is proven.
//! - `mega-get` has no tag option, but a folder `get` into a private staging
//!   folder lists one child transfer per file with its own tag and its
//!   destination path, so ownership follows from the unique staging path.
//! - `mega-transfers -c TAG` cancels that transfer alone.
//!
//! MEGA has no content hash a client can read back: the proof of a file is
//! its terminal state in that record and the size MEGA lists for it.

use super::MegaCmdProvider;
use crate::providers::bulk::{
    absolute_local_path, command_arg_bytes, plain_bulk_name, remote_parent, BulkProgress,
    BulkTransferItem, BulkTransferOutcome, BULK_COMMAND_RESERVE_BYTES, MAX_BULK_BYTES,
    MAX_BULK_FILES, MAX_BULK_PATH_BYTES,
};
use crate::providers::{ProviderError, StorageProvider};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Rows `mega-transfers` is asked for: the server keeps a bounded history,
/// and a row that is gone is simply not a proof.
const TRANSFER_ROWS: &str = "--limit=1000";
/// Paths are printed in full up to this many characters; a row whose path
/// is cut anyway never matches a staged file exactly, so it proves nothing.
const PATH_DISPLAY: &str = "--path-display-size=4096";
/// Silence after which a bulk command is cut, as a single transfer is; short
/// in tests so a stalled stand-in is cut quickly.
const BULK_IDLE: Duration = if cfg!(test) {
    Duration::from_secs(3)
} else {
    Duration::from_secs(super::MEGA_TRANSFER_IDLE_SECS)
};
/// How long one `mega-transfers -c` may take when a dropped bulk cancels
/// what it owns.
const CANCEL_WAIT: Duration = Duration::from_secs(5);

/// One row of `mega-transfers --col-separator=| --output-cols=TAG,STATE`, or
/// of `...,DESTINYPATH` for downloads.
#[derive(Debug, Clone, PartialEq)]
struct TransferRow {
    tag: String,
    state: String,
    destination: Option<String>,
}

impl TransferRow {
    fn finished(&self) -> bool {
        matches!(self.state.as_str(), "COMPLETED" | "CANCELLED" | "FAILED")
    }
}

/// The rows of a `mega-transfers` table. Lines that are not rows (the header,
/// "Showing first N transfers", an empty line) are skipped, as is any line
/// whose tag or state is not a plain word. The destination is the last
/// column, so it is split off at most once more than the columns before it:
/// a `|` in a path stays in the path.
fn transfer_rows(output: &str, with_destination: bool) -> Vec<TransferRow> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            let mut columns = line.splitn(if with_destination { 3 } else { 2 }, '|');
            let tag = columns.next()?.trim();
            let state = columns.next()?.trim();
            let destination = if with_destination {
                Some(columns.next()?.to_string())
            } else {
                None
            };
            (!tag.is_empty()
                && tag.bytes().all(|b| b.is_ascii_digit())
                && !state.is_empty()
                && state.bytes().all(|b| b.is_ascii_uppercase()))
            .then(|| TransferRow {
                tag: tag.to_string(),
                state: state.to_string(),
                destination,
            })
        })
        .collect()
}

/// The tag and source of an `Upload started: Tag = N. Source = <path>` line.
fn upload_started(line: &str) -> Option<(String, String)> {
    let rest = line
        .trim_end_matches('\r')
        .strip_prefix("Upload started: Tag = ")?;
    let (tag, source) = rest.split_once(". Source = ")?;
    (!tag.is_empty() && tag.bytes().all(|b| b.is_ascii_digit()))
        .then(|| (tag.to_string(), source.to_string()))
}

/// A path as MEGAcmd may print it on any platform, made comparable: `\` as
/// `/`, without a `\\?\` verbatim prefix, the drive letter in lower case,
/// no trailing separator.
fn comparable(path: &str) -> String {
    let mut path = path.replace('\\', "/");
    if let Some(rest) = path.strip_prefix("//?/") {
        path = rest.to_string();
    }
    if path.len() >= 2 && path.as_bytes()[1] == b':' && path.as_bytes()[0].is_ascii_alphabetic() {
        let drive = path[..1].to_ascii_lowercase();
        path.replace_range(..1, &drive);
    }
    while path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    path
}

/// What a row's `destination` names under the staged `folder`: the path
/// below it, or `None` for any other path and for the folder itself.
fn staged_name(destination: &str, folder: &str) -> Option<String> {
    let (destination, folder) = (comparable(destination), comparable(folder));
    let below = destination
        .strip_prefix(folder.as_str())?
        .strip_prefix('/')?;
    (!below.is_empty()).then(|| below.to_string())
}

/// Whether a row's `destination` is the staged `folder` itself.
fn is_staged_folder(destination: &str, folder: &str) -> bool {
    comparable(destination) == comparable(folder)
}

/// Run `mega-transfers` with `args`, blocking, for at most [`CANCEL_WAIT`]:
/// its output, or `None`. Used only where a bulk is dropped and cannot await.
fn transfers_now(command: &str, args: &[&str]) -> Option<String> {
    use std::io::Read;
    let mut builder = std::process::Command::new(command);
    builder
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        builder.creation_flags(super::CREATE_NO_WINDOW);
    }
    let mut child = builder.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a thread: a long table fills the pipe before the process ends.
    // The thread is never joined: on Windows `mega-transfers.bat` runs
    // `MegaClient.exe`, which can keep the pipe open after the batch file is
    // killed, and the drop handler must not wait for it.
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        let _ = sent.send(out);
    });
    let deadline = std::time::Instant::now() + CANCEL_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    // At least a moment for a table that is already complete.
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    received
        .recv_timeout(left.max(Duration::from_millis(200)))
        .ok()
}

/// The transfers one bulk handed to the MEGAcmd server, which goes on with
/// them after the client is gone. While armed, dropping this (a cancelled
/// future, an early return) cancels the ones not finished, tag by tag and
/// never with `-a`: other transfers of the same server are not this bulk's.
/// The wait runs off the async workers. Declared after a download's staging
/// folder, it is dropped before that folder is removed.
struct OwnedTransfers {
    command: String,
    /// Upload tags, as the client printed them.
    tags: Arc<Mutex<Vec<String>>>,
    /// A download's staged folder: its rows are the folder's own and the
    /// ones below it.
    staging: Option<String>,
    armed: bool,
}

impl OwnedTransfers {
    fn new(command: String, staging: Option<String>) -> Self {
        Self {
            command,
            tags: Arc::new(Mutex::new(Vec::new())),
            staging,
            armed: true,
        }
    }

    /// The tags of this bulk's transfers that are not finished in `rows`. An
    /// upload tag missing from the rows may still be queued, so it counts.
    fn unfinished(&self, rows: &[TransferRow]) -> Vec<String> {
        match &self.staging {
            Some(folder) => rows
                .iter()
                .filter(|row| {
                    !row.finished()
                        && row.destination.as_deref().is_some_and(|d| {
                            is_staged_folder(d, folder) || staged_name(d, folder).is_some()
                        })
                })
                .map(|row| row.tag.clone())
                .collect(),
            None => {
                let finished: HashSet<&str> = rows
                    .iter()
                    .filter(|row| row.finished())
                    .map(|row| row.tag.as_str())
                    .collect();
                self.tags
                    .lock()
                    .map(|tags| {
                        tags.iter()
                            .filter(|tag| !finished.contains(tag.as_str()))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default()
            }
        }
    }

    fn cancel_now(&self) {
        let downloads = self.staging.is_some();
        let rows = transfers_now(
            &self.command,
            &MegaCmdProvider::transfer_rows_args(downloads),
        )
        .map(|out| transfer_rows(&out, downloads))
        .unwrap_or_default();
        for tag in self.unfinished(&rows) {
            transfers_now(&self.command, &["-c", &tag]);
        }
    }
}

impl Drop for OwnedTransfers {
    fn drop(&mut self) {
        if self.armed {
            crate::credential_store::off_async_worker(|| self.cancel_now());
        }
    }
}

/// Run a bulk `mega-put` / `mega-get` once, reading both streams as they
/// come: every line of either that is not a progress line goes to `on_line`
/// (MEGAcmd 2.6 writes its progress on stderr, and a version that moves the
/// start lines there is still followed). Cut after `idle` without output, as
/// a single transfer is. `Err(None)` is that cut, `Err(Some(_))` a failure to
/// run the command at all.
async fn run_bulk_command(
    mut command: tokio::process::Command,
    idle: Duration,
    on_line: &mut (dyn FnMut(&str) + Send),
) -> Result<std::process::ExitStatus, Option<std::io::Error>> {
    use tokio::io::AsyncReadExt;
    command
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(Some)?;
    let (Some(mut out), Some(mut err)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(Some(std::io::Error::other("no pipes to the command")));
    };
    let (mut out_line, mut err_line) = (Vec::new(), Vec::new());
    let (mut out_open, mut err_open) = (true, true);
    let (mut out_buf, mut err_buf) = ([0u8; 4096], [0u8; 4096]);
    let mut feed = |line: &mut Vec<u8>, bytes: &[u8]| {
        for &byte in bytes {
            if byte == b'\r' || byte == b'\n' {
                if !line.is_empty() && MegaCmdProvider::transfer_percent(line).is_none() {
                    on_line(&String::from_utf8_lossy(line));
                }
                line.clear();
            } else {
                line.push(byte);
            }
        }
    };
    while out_open || err_open {
        let read = tokio::time::timeout(idle, async {
            tokio::select! {
                n = out.read(&mut out_buf), if out_open => (true, n),
                n = err.read(&mut err_buf), if err_open => (false, n),
            }
        })
        .await
        .map_err(|_| None)?;
        match read {
            (true, Ok(0)) | (true, Err(_)) => out_open = false,
            (false, Ok(0)) | (false, Err(_)) => err_open = false,
            (true, Ok(n)) => feed(&mut out_line, &out_buf[..n]),
            (false, Ok(n)) => feed(&mut err_line, &err_buf[..n]),
        }
    }
    feed(&mut out_line, b"\n");
    feed(&mut err_line, b"\n");
    tokio::time::timeout(idle, child.wait())
        .await
        .map_err(|_| None)?
        .map_err(Some)
}

/// A file admitted to a bulk.
struct Admitted {
    index: usize,
    name: String,
    /// The local path exactly as handed to MEGAcmd.
    local_arg: String,
}

/// What [`MegaCmdProvider::download_admission`] admitted.
struct DownloadPlan {
    remote_folder: String,
    local_folder: std::path::PathBuf,
    folder_name: String,
    /// Planned name → item index.
    names: HashMap<String, usize>,
    /// Name → (entries of that name, size) in the folder listing.
    listed: HashMap<String, (usize, u64)>,
}

fn not_admitted(items: &[BulkTransferItem]) -> Vec<BulkTransferOutcome> {
    items
        .iter()
        .map(|_| BulkTransferOutcome::NotAttempted("Not admitted to the native bulk".into()))
        .collect()
}

/// Name → (how many entries, size of the last) in a folder listing.
fn listed_names(entries: &[crate::providers::RemoteEntry]) -> HashMap<String, (usize, u64)> {
    let mut names: HashMap<String, (usize, u64)> = HashMap::new();
    for entry in entries {
        let slot = names.entry(entry.name.clone()).or_insert((0, entry.size));
        slot.0 += 1;
        slot.1 = entry.size;
    }
    names
}

impl MegaCmdProvider {
    fn transfer_rows_args(downloads: bool) -> [&'static str; 6] {
        [
            "--show-completed",
            if downloads {
                "--only-downloads"
            } else {
                "--only-uploads"
            },
            TRANSFER_ROWS,
            PATH_DISPLAY,
            "--col-separator=|",
            if downloads {
                "--output-cols=TAG,STATE,DESTINYPATH"
            } else {
                "--output-cols=TAG,STATE"
            },
        ]
    }

    /// The server's rows for one direction. An error is kept apart from an
    /// empty record: without the record nothing is known about what the
    /// server is still doing.
    async fn transfer_record(&self, downloads: bool) -> Result<Vec<TransferRow>, ProviderError> {
        self.run_mega_cmd("mega-transfers", &Self::transfer_rows_args(downloads))
            .await
            .map(|out| transfer_rows(&out, downloads))
    }

    /// Cancel `tags`, one at a time. Only when every cancel went through is
    /// `owned` disarmed and `true` returned; otherwise the guard stays armed,
    /// so dropping it tries again, and the caller must treat the server as
    /// possibly still writing.
    async fn cancel_owned(&self, owned: &mut OwnedTransfers, tags: Vec<String>) -> bool {
        let mut all = true;
        for tag in tags {
            if let Err(e) = self.run_mega_cmd("mega-transfers", &["-c", &tag]).await {
                tracing::warn!(target: "mega", "[BULK] cancel of transfer {tag} failed: {e}");
                all = false;
            }
        }
        if all {
            owned.armed = false;
        }
        all
    }

    fn bulk_command(&self, cmd: &str, args: &[String]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(self.command_path(cmd));
        command.args(args);
        #[cfg(windows)]
        command.creation_flags(super::CREATE_NO_WINDOW);
        command
    }

    /// Whether the bulk takes `items` as one group, with the checks the bulk
    /// itself runs ([`Self::upload_admission`], [`Self::download_admission`]):
    /// a caller keeps its independent workers for a group the bulk would hand
    /// back file by file. An upload counts only if every item is admitted.
    pub(super) async fn bulk_admits_group(
        &mut self,
        items: &[BulkTransferItem],
        upload: bool,
    ) -> bool {
        if upload {
            self.upload_admission(items)
                .await
                .is_some_and(|(_, admitted)| admitted.len() == items.len())
        } else {
            self.download_admission(items).await.is_some()
        }
    }

    /// The files of `items` one `mega-put` can carry: one remote folder, plain
    /// names the local file shares, unchanged sources, within the bounds, and
    /// only names the folder does not hold yet (one listing). `None` when
    /// fewer than two remain: one file gains nothing from a bulk, and the
    /// single-file path also shows its progress.
    async fn upload_admission(
        &mut self,
        items: &[BulkTransferItem],
    ) -> Option<(String, Vec<Admitted>)> {
        let mut admitted: Vec<Admitted> = Vec::new();
        let mut group_parent: Option<String> = None;
        let mut names = HashSet::new();
        let (mut bytes, mut arg_bytes) = (0u64, BULK_COMMAND_RESERVE_BYTES);
        for (index, item) in items.iter().enumerate() {
            let remote = self.resolve_path(&item.remote_path);
            let (Some(name), Some(parent)) = (plain_bulk_name(&remote), remote_parent(&remote))
            else {
                continue;
            };
            let Some(absolute) = absolute_local_path(&item.local_path) else {
                continue;
            };
            let local = Path::new(&absolute);
            let item_args = command_arg_bytes(&absolute, &remote);
            if local.file_name().and_then(|n| n.to_str()) != Some(name)
                || names.contains(name)
                || group_parent.as_deref().is_some_and(|p| p != parent)
                || admitted.len() >= MAX_BULK_FILES
                || bytes.saturating_add(item.size) > MAX_BULK_BYTES
                || arg_bytes.saturating_add(item_args) > MAX_BULK_PATH_BYTES
            {
                continue;
            }
            // A source that changed since planning keeps the single-file
            // path, which sends what is there now.
            match std::fs::symlink_metadata(local) {
                Ok(meta) if meta.is_file() && meta.len() == item.size => {}
                _ => continue,
            }
            names.insert(name.to_string());
            group_parent = Some(parent.to_string());
            bytes = bytes.saturating_add(item.size);
            arg_bytes = arg_bytes.saturating_add(item_args);
            admitted.push(Admitted {
                index,
                name: name.to_string(),
                local_arg: Self::normalize_local_path_for_cli(&absolute),
            });
        }
        let parent = group_parent?;
        // Only new destinations: one listing of the folder before the put.
        let Ok(before) = self.list(&parent).await else {
            return None;
        };
        let present = listed_names(&before);
        admitted.retain(|a| !present.contains_key(&a.name));
        if admitted.len() < 2 {
            // One file gains nothing from a bulk; the single-file path also
            // shows its progress.
            return None;
        }
        Some((parent, admitted))
    }

    /// The plan of one folder `mega-get` for `items`: every file of one remote
    /// folder into one local folder, plain names that no filesystem folds
    /// together, within the bounds, and a folder that holds exactly these
    /// files and no folder (one listing). `None` otherwise.
    async fn download_admission(&mut self, items: &[BulkTransferItem]) -> Option<DownloadPlan> {
        if items.len() < 2 || items.len() > MAX_BULK_FILES {
            return None;
        }
        let mut remote_folder: Option<String> = None;
        let mut local_folder: Option<std::path::PathBuf> = None;
        let mut names: HashMap<String, usize> = HashMap::new();
        let mut folded = HashSet::new();
        let mut bytes = 0u64;
        for (index, item) in items.iter().enumerate() {
            let remote = self.resolve_path(&item.remote_path);
            let (Some(name), Some(parent)) = (plain_bulk_name(&remote), remote_parent(&remote))
            else {
                return None;
            };
            let absolute = absolute_local_path(&item.local_path)?;
            let local = Path::new(&absolute);
            let local_parent = local.parent().map(Path::to_path_buf);
            bytes = bytes.saturating_add(item.size);
            if parent == "/"
                || local.file_name().and_then(|n| n.to_str()) != Some(name)
                || names.insert(name.to_string(), index).is_some()
                // Names this filesystem may fold together would land on one
                // staged file.
                || !folded.insert(name.to_lowercase())
                || remote_folder.as_deref().is_some_and(|f| f != parent)
                || local_folder.is_some() && local_folder != local_parent
                || local_parent.is_none()
                || bytes > MAX_BULK_BYTES
            {
                return None;
            }
            remote_folder = Some(parent.to_string());
            local_folder = local_parent;
        }
        let (Some(remote_folder), Some(local_folder)) = (remote_folder, local_folder) else {
            return None;
        };
        // The folder holds exactly these files: no other file, no folder.
        let Ok(listing) = self.list(&remote_folder).await else {
            return None;
        };
        let listed = listed_names(&listing);
        if listing.iter().any(|e| e.is_dir)
            || listed.len() != names.len()
            || listed.values().any(|(count, _)| *count != 1)
            || names.keys().any(|n| !listed.contains_key(n))
        {
            return None;
        }
        let folder_name = remote_folder.rsplit('/').next().filter(|n| !n.is_empty())?;
        Some(DownloadPlan {
            folder_name: folder_name.to_string(),
            remote_folder,
            local_folder,
            names,
            listed,
        })
    }

    /// Several new files of one remote folder in one `mega-put`. Only names
    /// the folder does not hold yet are admitted: an existing name keeps the
    /// single-file path and its overwrite rules. A file is Completed only
    /// when the server's record shows its tag COMPLETED and MEGA lists it
    /// once at its size. Without a tag and without the file on MEGA it was
    /// never started (NotAttempted); anything else unproven may be on MEGA
    /// (Uncertain). The bulk command runs once and is never replayed.
    pub(super) async fn bulk_upload(
        &mut self,
        items: &[BulkTransferItem],
        progress: Option<BulkProgress>,
    ) -> Result<Vec<BulkTransferOutcome>, ProviderError> {
        let mut outcomes = not_admitted(items);
        let Some((parent, admitted)) = self.upload_admission(items).await else {
            return Ok(outcomes);
        };

        let mut args = vec!["--print-tag-at-start".to_string()];
        args.extend(admitted.iter().map(|a| a.local_arg.clone()));
        args.push(if parent == "/" {
            "/".to_string()
        } else {
            format!("{parent}/")
        });
        if args.iter().map(|a| a.len() + 3).sum::<usize>() > MAX_BULK_PATH_BYTES {
            return Ok(outcomes);
        }
        for a in &admitted {
            if let Some(cb) = &progress {
                cb(a.index, 0, items[a.index].size);
            }
        }

        // From here on any of these files may reach MEGA.
        let mut owned = OwnedTransfers::new(self.command_path("mega-transfers"), None);
        let started: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let (tags, record) = (owned.tags.clone(), started.clone());
        let mut on_line = move |line: &str| {
            if let Some((tag, source)) = upload_started(line) {
                if let Ok(mut tags) = tags.lock() {
                    tags.push(tag.clone());
                }
                if let Ok(mut record) = record.lock() {
                    record.push((tag, source));
                }
            }
        };
        let run = run_bulk_command(
            self.bulk_command("mega-put", &args),
            BULK_IDLE,
            &mut on_line,
        )
        .await;
        if let Err(e) = &run {
            tracing::warn!(target: "mega", "[BULK] mega-put did not end normally: {e:?}");
        }
        // Cut for silence, the client printed no more start lines, but the
        // server may already hold sources it had not announced: then no tag
        // does not mean never started. (A put that could not be launched at
        // all, or that ended by itself, announced every source it took.)
        let put_was_cut = matches!(run, Err(None));

        // Without the record every tag counts as unfinished and is cancelled,
        // and no file is proven (uncertain).
        let rows = self.transfer_record(false).await.unwrap_or_default();
        let unfinished = owned.unfinished(&rows);
        self.cancel_owned(&mut owned, unfinished.clone()).await;
        let mut states: HashMap<&str, Option<&str>> = HashMap::new();
        for row in &rows {
            states
                .entry(row.tag.as_str())
                // Two rows of one tag that disagree prove nothing.
                .and_modify(|state| {
                    if *state != Some(row.state.as_str()) {
                        *state = None;
                    }
                })
                .or_insert(Some(row.state.as_str()));
        }
        let after = self.list(&parent).await.ok().map(|e| listed_names(&e));
        let started = started.lock().map(|s| s.clone()).unwrap_or_default();

        for a in &admitted {
            let item = &items[a.index];
            let tags: Vec<&str> = started
                .iter()
                .filter(|(_, source)| comparable(source) == comparable(&a.local_arg))
                .map(|(tag, _)| tag.as_str())
                .collect();
            let listed = after.as_ref().map(|names| names.get(&a.name).copied());
            outcomes[a.index] = match (tags.as_slice(), listed) {
                ([], Some(None)) if !put_was_cut => BulkTransferOutcome::NotAttempted(
                    "MEGAcmd did not start this upload".into(),
                ),
                ([], _) => BulkTransferOutcome::Uncertain(
                    "MEGAcmd gave no transfer tag for this upload; it may be on MEGA".into(),
                ),
                ([tag], listed) if unfinished.iter().any(|t| t == tag) => {
                    BulkTransferOutcome::Uncertain(format!(
                        "The upload had not finished and was cancelled{}",
                        if matches!(listed, Some(Some(_))) {
                            "; MEGA lists the file"
                        } else {
                            ""
                        }
                    ))
                }
                ([tag], listed) => match (states.get(tag).copied().flatten(), listed) {
                    (Some("COMPLETED"), Some(Some((1, size)))) if size == item.size => {
                        if let Some(cb) = &progress {
                            cb(a.index, item.size, item.size);
                        }
                        BulkTransferOutcome::Completed
                    }
                    (Some("COMPLETED"), Some(Some((count, size)))) => {
                        BulkTransferOutcome::Uncertain(format!(
                            "MEGAcmd reported the upload done, but MEGA lists {count} file(s) of that name, the last of {size} bytes for {} planned",
                            item.size
                        ))
                    }
                    // Ended for good and not on MEGA: nothing reached the
                    // folder, so the single-file path may try it once, with
                    // its own retries, as for a transient failure.
                    (Some(state @ ("FAILED" | "CANCELLED")), Some(None)) => {
                        BulkTransferOutcome::NotAttempted(format!(
                            "MEGAcmd reported the upload as {} and MEGA does not list the file",
                            state.to_lowercase()
                        ))
                    }
                    (state, _) => BulkTransferOutcome::Uncertain(format!(
                        "No proof of this upload ({}); it may be on MEGA",
                        state.map_or("no terminal state in the transfer record".to_string(), |s| {
                            format!("state {s} and the listing do not agree")
                        })
                    )),
                },
                (_, _) => BulkTransferOutcome::Uncertain(
                    "MEGAcmd gave this source more than one transfer tag".into(),
                ),
            };
        }
        Ok(outcomes)
    }

    /// Every file of one remote folder in one folder `mega-get` into a
    /// private staging folder beside the destination. Admitted only when the
    /// folder holds exactly the planned files and nothing else: a folder get
    /// never brings a file the caller did not select. A file is published
    /// (renamed into place) only when the server's record shows its own
    /// destination COMPLETED and the staged file has the size MEGA listed.
    /// Anything else goes back to the single-file path: it was only ever
    /// written into staging.
    pub(super) async fn bulk_download(
        &mut self,
        items: &[BulkTransferItem],
        progress: Option<BulkProgress>,
    ) -> Result<Vec<BulkTransferOutcome>, ProviderError> {
        let mut outcomes = not_admitted(items);
        let Some(DownloadPlan {
            remote_folder,
            local_folder,
            folder_name,
            names,
            listed,
        }) = self.download_admission(items).await
        else {
            return Ok(outcomes);
        };

        tokio::fs::create_dir_all(&local_folder)
            .await
            .map_err(ProviderError::IoError)?;
        // The prefix is one `is_download_staging_dir_name` knows, so scans and
        // watchers never take staged files for content.
        let staging = tempfile::Builder::new()
            .prefix(".aeroftp-mega-bulk-")
            .tempdir_in(&local_folder)
            .map_err(ProviderError::IoError)?;
        let staged_folder = staging.path().join(folder_name);
        let folder = staged_folder.to_string_lossy().into_owned();
        // Dropped before `staging`: the server's transfers into it are
        // cancelled before the folder goes.
        let mut owned =
            OwnedTransfers::new(self.command_path("mega-transfers"), Some(folder.clone()));
        for (index, item) in items.iter().enumerate() {
            if let Some(cb) = &progress {
                cb(index, 0, item.size);
            }
        }
        let args = vec![
            remote_folder.clone(),
            Self::normalize_local_path_for_cli(&staging.path().to_string_lossy()),
        ];
        let run =
            run_bulk_command(self.bulk_command("mega-get", &args), BULK_IDLE, &mut |_| {}).await;
        if let Err(e) = &run {
            tracing::warn!(target: "mega", "[BULK] mega-get did not end normally: {e:?}");
        }

        // The staging folder goes only once the server is known to have
        // stopped writing into it: with no record, or a cancel that did not
        // go through, it stays (hidden from scans and watchers by its prefix)
        // and every file goes back to the single-file path.
        let rows = match self.transfer_record(true).await {
            Ok(rows) => rows,
            Err(e) => {
                let kept = staging.keep();
                tracing::warn!(target: "mega", "[BULK] no transfer record ({e}); staging kept at {}", kept.display());
                return Ok(not_admitted_after(
                    items,
                    "MEGAcmd's transfer record could not be read; nothing published",
                ));
            }
        };
        let unfinished = owned.unfinished(&rows);
        if !self.cancel_owned(&mut owned, unfinished).await {
            let kept = staging.keep();
            tracing::warn!(target: "mega", "[BULK] a transfer could not be cancelled; staging kept at {}", kept.display());
            return Ok(not_admitted_after(
                items,
                "A MEGAcmd transfer of this bulk could not be cancelled; nothing published",
            ));
        }
        // This bulk's rows, by the name under the staged folder. Two rows for
        // one name, or a row of this staging that is not one planned name,
        // and the record cannot be read for this folder: the whole group
        // goes back to the single-file path.
        let mut finished: HashMap<String, &str> = HashMap::new();
        for row in &rows {
            let Some(name) = row
                .destination
                .as_deref()
                .and_then(|d| staged_name(d, &folder))
            else {
                continue;
            };
            if !names.contains_key(&name) || finished.insert(name, row.state.as_str()).is_some() {
                return Ok(not_admitted_after(
                    items,
                    "MEGAcmd's transfer record for this folder is ambiguous",
                ));
            }
        }
        for (name, &index) in &names {
            let item = &items[index];
            let staged = staged_folder.join(name);
            let size = listed.get(name).map(|(_, size)| *size);
            outcomes[index] = match finished.get(name) {
                Some(&"COMPLETED") => match tokio::fs::symlink_metadata(&staged).await {
                    Ok(meta) if meta.is_file() && Some(meta.len()) == size => {
                        match tokio::fs::rename(&staged, &item.local_path).await {
                            Ok(()) => {
                                if let Some(cb) = &progress {
                                    cb(index, meta.len(), meta.len());
                                }
                                BulkTransferOutcome::Completed
                            }
                            Err(e) => BulkTransferOutcome::Failed(ProviderError::IoError(e)),
                        }
                    }
                    Ok(meta) => BulkTransferOutcome::NotAttempted(format!(
                        "The staged file has {} bytes, MEGA lists {}",
                        meta.len(),
                        size.unwrap_or_default()
                    )),
                    Err(_) => BulkTransferOutcome::NotAttempted(
                        "MEGAcmd reported the download done but wrote no file".into(),
                    ),
                },
                Some(state) => BulkTransferOutcome::NotAttempted(format!(
                    "MEGAcmd reported the download as {}",
                    state.to_lowercase()
                )),
                None => BulkTransferOutcome::NotAttempted(
                    "No row for this file in MEGAcmd's transfer record".into(),
                ),
            };
        }
        drop(owned);
        drop(staging);
        Ok(outcomes)
    }
}

/// Every item back to the single-file path, with `reason`.
fn not_admitted_after(items: &[BulkTransferItem], reason: &str) -> Vec<BulkTransferOutcome> {
    items
        .iter()
        .map(|_| BulkTransferOutcome::NotAttempted(reason.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_rows_skip_what_is_not_a_row_and_keep_a_bar_in_a_path() {
        let table = " ...  Showing first 1000 transfers ...\n\
                     TAG|STATE|DESTINYPATH\n\
                     2401|COMPLETED|/tmp/a|b/stage/f.bin\n\
                     2402|ACTIVE|/tmp/x/g.bin\r\n\
                     24x3|COMPLETED|/tmp/x/h.bin\n\
                     2404|in progress|/tmp/x/i.bin\n\
                     2405|FAILED\n";
        assert_eq!(
            transfer_rows(table, true),
            [
                TransferRow {
                    tag: "2401".into(),
                    state: "COMPLETED".into(),
                    destination: Some("/tmp/a|b/stage/f.bin".into()),
                },
                TransferRow {
                    tag: "2402".into(),
                    state: "ACTIVE".into(),
                    destination: Some("/tmp/x/g.bin".into()),
                },
            ]
        );
        // An upload table has two columns: a row with a third one is not
        // read, so it can prove nothing.
        let uploads = "TAG|STATE\n2436|COMPLETED\n2437|FAILED|extra\n2438|FAILED\n";
        let rows = transfer_rows(uploads, false);
        assert_eq!(
            rows.iter()
                .map(|r| (r.tag.as_str(), r.state.as_str()))
                .collect::<Vec<_>>(),
            [("2436", "COMPLETED"), ("2438", "FAILED")]
        );
    }

    /// Rows are matched to the staged folder whatever separators and drive
    /// letter case MEGAcmd prints: Windows paths, a verbatim prefix, `/` and
    /// `\\` mixed, a `|` in a folder name.
    #[test]
    fn staged_rows_match_windows_shaped_paths() {
        let folder = r"C:\Users\me\Down|loads\.aeroftp-mega-bulk-x\folder";
        for destination in [
            r"C:\Users\me\Down|loads\.aeroftp-mega-bulk-x\folder\f.bin",
            r"c:\Users\me\Down|loads\.aeroftp-mega-bulk-x\folder\f.bin",
            r"C:/Users/me/Down|loads/.aeroftp-mega-bulk-x/folder/f.bin",
            r"C:\Users/me\Down|loads/.aeroftp-mega-bulk-x\folder/f.bin",
            r"\\?\C:\Users\me\Down|loads\.aeroftp-mega-bulk-x\folder\f.bin",
        ] {
            assert_eq!(
                staged_name(destination, folder).as_deref(),
                Some("f.bin"),
                "{destination}"
            );
        }
        assert!(is_staged_folder(
            r"c:/Users/me/Down|loads/.aeroftp-mega-bulk-x/folder/",
            folder
        ));
        for other in [
            r"C:\Users\me\Down|loads\.aeroftp-mega-bulk-x\folder",
            r"C:\Users\me\Down|loads\.aeroftp-mega-bulk-x\folder2\f.bin",
            r"D:\Users\me\Down|loads\.aeroftp-mega-bulk-x\folder\f.bin",
            r"...\me\Down|loads\.aeroftp-mega-bulk-x\folder\f.bin",
        ] {
            assert_eq!(staged_name(other, folder), None, "{other}");
        }
        // An upload's source is matched the same way.
        assert_eq!(
            comparable(r"\\?\c:\Users\me\up\a.bin"),
            comparable(r"C:\Users\me/up/a.bin")
        );
        assert_ne!(comparable("/up/a.bin"), comparable("/up/A.bin"));
        assert_eq!(
            staged_name("/tmp/a|b/stage/folder/f.bin", "/tmp/a|b/stage/folder").as_deref(),
            Some("f.bin")
        );
        // A file deeper down is named with its path, which no planned name
        // matches, so the group goes back to the single-file path.
        assert_eq!(
            staged_name("/s/folder/sub/f.bin", "/s/folder").as_deref(),
            Some("sub/f.bin")
        );
    }

    #[test]
    fn upload_started_reads_the_tag_and_the_whole_source() {
        let long = format!("/tmp/{}.bin", "n".repeat(240));
        assert_eq!(
            upload_started(&format!("Upload started: Tag = 2436. Source = {long}")),
            Some(("2436".to_string(), long))
        );
        assert_eq!(
            upload_started("Upload started: Tag = 7. Source = /a. Source = b"),
            Some(("7".to_string(), "/a. Source = b".to_string()))
        );
        for other in [
            "Upload finished: /x",
            "Upload started: Tag = . Source = /x",
            "Upload started: Tag = 1a. Source = /x",
        ] {
            assert_eq!(upload_started(other), None, "{other}");
        }
    }

    /// Against a stand-in MEGAcmd: links to a checked-in script.
    #[cfg(unix)]
    mod stand_in {
        use super::*;

        type Events = Arc<Mutex<Vec<(usize, u64, u64)>>>;

        fn recorder() -> (BulkProgress, Events) {
            let events: Events = Arc::new(Mutex::new(Vec::new()));
            let sink = events.clone();
            (
                Box::new(move |index, done, total| sink.lock().unwrap().push((index, done, total))),
                events,
            )
        }

        /// A provider whose MEGAcmd commands are links to the bulk stand-in, and
        /// the folder holding them (and the stand-in's state files).
        fn provider_on_bulk_stand_in() -> (MegaCmdProvider, tempfile::TempDir) {
            let dir = tempfile::tempdir().expect("tempdir");
            let shim = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/megacmd_bulk_shim.sh");
            for name in ["mega-ls", "mega-put", "mega-get", "mega-transfers"] {
                std::os::unix::fs::symlink(&shim, dir.path().join(name)).unwrap();
            }
            let mut provider = crate::providers::mega::tests::test_provider();
            provider.cmd_dir = Some(dir.path().to_path_buf());
            (provider, dir)
        }

        fn read(path: &Path) -> String {
            std::fs::read_to_string(path).unwrap_or_default()
        }

        fn uploads(local: &Path, names: &[&str]) -> Vec<BulkTransferItem> {
            names
                .iter()
                .map(|name| {
                    let path = local.join(name);
                    std::fs::write(&path, name.as_bytes()).unwrap();
                    BulkTransferItem {
                        local_path: path.to_string_lossy().into_owned(),
                        remote_path: format!("/up/{name}"),
                        size: name.len() as u64,
                    }
                })
                .collect()
        }

        #[tokio::test]
        async fn a_bulk_upload_is_one_put_with_a_proof_per_file() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            std::fs::write(dir.path().join("listing.txt"), b"").unwrap();
            let local = tempfile::tempdir().unwrap();
            let items = uploads(local.path(), &["a.bin", "bb.bin", "ccc.bin"]);
            let (progress, events) = recorder();
            let outcomes = provider.bulk_upload(&items, Some(progress)).await.unwrap();
            assert!(
                outcomes
                    .iter()
                    .all(|o| matches!(o, BulkTransferOutcome::Completed)),
                "{outcomes:?}"
            );
            let puts = read(&dir.path().join("put.log"));
            assert_eq!(puts.lines().count(), 1, "{puts}");
            assert!(puts.starts_with("--print-tag-at-start "), "{puts}");
            assert!(puts.trim_end().ends_with(" /up/"), "{puts}");
            let events = events.lock().unwrap().clone();
            for (index, item) in items.iter().enumerate() {
                assert!(events.contains(&(index, 0, item.size)), "{events:?}");
                assert!(
                    events.contains(&(index, item.size, item.size)),
                    "{events:?}"
                );
            }
            assert_eq!(read(&dir.path().join("cancel.log")), "");
        }

        /// Each file is settled on its own proof: a name MEGA already holds is
        /// not sent (the single-file path keeps its overwrite rules), a source
        /// MEGAcmd never started and that is not on MEGA goes back, a failure
        /// stays a failure, and a completed transfer MEGA lists at another size
        /// is not called done.
        #[tokio::test]
        async fn a_bulk_upload_settles_each_file_on_its_own() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            std::fs::write(
                dir.path().join("listing.txt"),
                b"----  1  5  15Jan2026  14:30 taken.bin\n",
            )
            .unwrap();
            for (flag, content) in [
                ("put-skip-skip.bin", ""),
                ("put-fail-fail.bin", ""),
                ("put-size-short.bin", "1"),
            ] {
                std::fs::write(dir.path().join(flag), content).unwrap();
            }
            let local = tempfile::tempdir().unwrap();
            let names = [
                "taken.bin",
                "skip.bin",
                "fail.bin",
                "short.bin",
                "ok1.bin",
                "ok2.bin",
            ];
            let items = uploads(local.path(), &names);
            let outcomes = provider.bulk_upload(&items, None).await.unwrap();
            assert!(
                matches!(outcomes[0], BulkTransferOutcome::NotAttempted(_)),
                "{:?}",
                outcomes[0]
            );
            assert!(
                matches!(outcomes[1], BulkTransferOutcome::NotAttempted(_)),
                "{:?}",
                outcomes[1]
            );
            assert!(
                matches!(outcomes[2], BulkTransferOutcome::NotAttempted(_)),
                "{:?}",
                outcomes[2]
            );
            assert!(
                matches!(outcomes[3], BulkTransferOutcome::Uncertain(_)),
                "{:?}",
                outcomes[3]
            );
            assert!(
                matches!(outcomes[4], BulkTransferOutcome::Completed),
                "{:?}",
                outcomes[4]
            );
            assert!(
                matches!(outcomes[5], BulkTransferOutcome::Completed),
                "{:?}",
                outcomes[5]
            );
            let puts = read(&dir.path().join("put.log"));
            assert!(!puts.contains("taken.bin"), "{puts}");
            assert_eq!(puts.lines().count(), 1, "{puts}");
        }

        /// Dropped while MEGAcmd still runs the transfers (a cancelled future),
        /// the bulk cancels exactly the tags it started, one at a time.
        #[tokio::test]
        async fn a_dropped_bulk_upload_cancels_its_own_transfers() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            std::fs::write(dir.path().join("listing.txt"), b"").unwrap();
            std::fs::write(dir.path().join("put-hang"), b"").unwrap();
            let local = tempfile::tempdir().unwrap();
            let items = uploads(local.path(), &["a.bin", "b.bin"]);
            let cut = tokio::time::timeout(
                Duration::from_millis(1500),
                provider.bulk_upload(&items, None),
            )
            .await;
            assert!(cut.is_err(), "the put was still running");
            let cancelled = read(&dir.path().join("cancel.log"));
            assert_eq!(
                cancelled.lines().collect::<Vec<_>>(),
                ["101", "102"],
                "{cancelled}"
            );
        }

        /// A folder of `count` files, each `size` bytes, in the stand-in's
        /// listing, and the items that download them into `local`.
        fn downloads(dir: &Path, local: &Path, names: &[&str], size: u64) -> Vec<BulkTransferItem> {
            let listing: String = names
                .iter()
                .map(|name| format!("----  1  {size}  15Jan2026  14:30 {name}\n"))
                .collect();
            std::fs::write(dir.join("listing.txt"), listing).unwrap();
            names
                .iter()
                .map(|name| BulkTransferItem {
                    local_path: local.join(name).to_string_lossy().into_owned(),
                    remote_path: format!("/shared/folder/{name}"),
                    size,
                })
                .collect()
        }

        fn no_staging_left(local: &Path) -> bool {
            std::fs::read_dir(local).map_or(true, |entries| {
                entries.flatten().all(|e| {
                    !e.file_name()
                        .to_string_lossy()
                        .starts_with(".aeroftp-mega-bulk-")
                })
            })
        }

        /// One folder get into private staging, every file proven and renamed
        /// into place, the staging gone. The destination folder has a `|` in
        /// its name, which every row's path then carries, and one name is long.
        #[tokio::test]
        async fn a_bulk_download_publishes_every_proven_file() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            let root = tempfile::tempdir().unwrap();
            let local = root.path().join("dest|with a bar");
            let long = format!("{}.bin", "n".repeat(200));
            let items = downloads(dir.path(), &local, &["a.bin", "b.bin", &long], 32);
            let (progress, events) = recorder();
            let outcomes = provider
                .bulk_download(&items, Some(progress))
                .await
                .unwrap();
            assert!(
                outcomes
                    .iter()
                    .all(|o| matches!(o, BulkTransferOutcome::Completed)),
                "{outcomes:?}"
            );
            for item in &items {
                assert_eq!(std::fs::metadata(&item.local_path).unwrap().len(), 32);
            }
            assert_eq!(read(&dir.path().join("get.log")).lines().count(), 1);
            assert!(no_staging_left(&local));
            assert_eq!(
                events.lock().unwrap().iter().filter(|e| e.1 == 32).count(),
                3
            );
            assert_eq!(read(&dir.path().join("cancel.log")), "");
        }

        /// A file MEGAcmd failed, and one whose row is cut so that its path is
        /// not the staged file's, go back to the single-file path; the proven
        /// ones are published.
        #[tokio::test]
        async fn a_bulk_download_sends_back_what_it_cannot_prove() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            std::fs::write(dir.path().join("get-fail-b.bin"), b"").unwrap();
            std::fs::write(dir.path().join("get-cut-c.bin"), b"").unwrap();
            let local = tempfile::tempdir().unwrap();
            let items = downloads(
                dir.path(),
                local.path(),
                &["a.bin", "b.bin", "c.bin", "d.bin"],
                8,
            );
            let outcomes = provider.bulk_download(&items, None).await.unwrap();
            assert!(
                matches!(outcomes[0], BulkTransferOutcome::Completed),
                "{:?}",
                outcomes[0]
            );
            assert!(
                matches!(outcomes[1], BulkTransferOutcome::NotAttempted(_)),
                "{:?}",
                outcomes[1]
            );
            assert!(
                matches!(outcomes[2], BulkTransferOutcome::NotAttempted(_)),
                "{:?}",
                outcomes[2]
            );
            assert!(
                matches!(outcomes[3], BulkTransferOutcome::Completed),
                "{:?}",
                outcomes[3]
            );
            assert!(!Path::new(&items[1].local_path).exists());
            assert!(!Path::new(&items[2].local_path).exists());
            assert!(no_staging_left(local.path()));
        }

        /// Two rows for one staged file: the record cannot be read for this
        /// folder, so nothing is published and every file goes back.
        #[tokio::test]
        async fn an_ambiguous_record_sends_the_whole_download_back() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            std::fs::write(dir.path().join("get-dup"), b"").unwrap();
            let local = tempfile::tempdir().unwrap();
            let items = downloads(dir.path(), local.path(), &["a.bin", "b.bin"], 8);
            let outcomes = provider.bulk_download(&items, None).await.unwrap();
            assert!(
                outcomes
                    .iter()
                    .all(|o| matches!(o, BulkTransferOutcome::NotAttempted(_))),
                "{outcomes:?}"
            );
            assert!(items.iter().all(|i| !Path::new(&i.local_path).exists()));
            assert!(no_staging_left(local.path()));
        }

        /// A folder get never brings a file the caller did not select: a folder
        /// holding one more file, or a subfolder, is not fetched at all.
        #[tokio::test]
        async fn a_folder_with_more_than_the_selection_is_never_fetched() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            let local = tempfile::tempdir().unwrap();
            let items = downloads(dir.path(), local.path(), &["a.bin", "b.bin"], 8);
            for extra in [
                "----  1  8  15Jan2026  14:30 unselected.bin\n",
                "d---  -  -  15Jan2026  14:30 sub\n",
            ] {
                let mut listing = read(&dir.path().join("listing.txt"));
                listing.push_str(extra);
                std::fs::write(dir.path().join("listing.txt"), &listing).unwrap();
                let outcomes = provider.bulk_download(&items, None).await.unwrap();
                assert!(
                    outcomes
                        .iter()
                        .all(|o| matches!(o, BulkTransferOutcome::NotAttempted(_))),
                    "{outcomes:?}"
                );
                let kept: String = listing.lines().take(2).map(|l| format!("{l}\n")).collect();
                std::fs::write(dir.path().join("listing.txt"), kept).unwrap();
            }
            assert_eq!(read(&dir.path().join("get.log")), "");
        }

        /// Dropped while MEGAcmd still downloads, the bulk cancels the folder
        /// transfer and its files' transfers into its own staging, and the
        /// staging goes.
        #[tokio::test]
        async fn a_dropped_bulk_download_cancels_its_own_transfers() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            std::fs::write(dir.path().join("get-hang"), b"").unwrap();
            let local = tempfile::tempdir().unwrap();
            let items = downloads(dir.path(), local.path(), &["a.bin", "b.bin"], 8);
            let cut = tokio::time::timeout(
                Duration::from_millis(1500),
                provider.bulk_download(&items, None),
            )
            .await;
            assert!(cut.is_err(), "the get was still running");
            let cancelled = read(&dir.path().join("cancel.log"));
            assert_eq!(
                cancelled.lines().collect::<Vec<_>>(),
                ["101", "102", "103"],
                "{cancelled}"
            );
            assert!(no_staging_left(local.path()));
        }
        async fn pays(
            provider: &mut MegaCmdProvider,
            items: &[BulkTransferItem],
            upload: bool,
        ) -> bool {
            let caps = provider.transfer_capabilities();
            crate::providers::bulk::cli_bulk_pays(provider, &caps, items, upload, 4).await
        }

        /// The CLI gives up its 4 independent workers only for groups the
        /// bulk takes, asked with the adapter's own admission: a group it
        /// would refuse would run file after file on the bulk's single lease.
        #[tokio::test]
        async fn the_cli_keeps_its_workers_for_a_group_the_bulk_would_refuse() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            let local = tempfile::tempdir().unwrap();
            // A whole folder: one bulk.
            let items = downloads(dir.path(), local.path(), &["a.bin", "b.bin", "c.bin"], 8);
            assert!(pays(&mut provider, &items, false).await);
            // The folder holds one more file than the selection.
            let mut listing = read(&dir.path().join("listing.txt"));
            listing.push_str("----  1  8  15Jan2026  14:30 unselected.bin\n");
            std::fs::write(dir.path().join("listing.txt"), listing).unwrap();
            assert!(!pays(&mut provider, &items, false).await);
            // 300 files: the engine forms groups of 256 and 44, neither the
            // whole folder.
            let names: Vec<String> = (0..300).map(|i| format!("f{i:03}.bin")).collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let many = downloads(dir.path(), local.path(), &names, 8);
            assert!(!pays(&mut provider, &many, false).await);
            assert_eq!(
                read(&dir.path().join("get.log")),
                "",
                "admission fetched nothing"
            );

            // Uploads: new names are one bulk; one name already there is not.
            std::fs::write(dir.path().join("listing.txt"), b"").unwrap();
            let up = tempfile::tempdir().unwrap();
            let files = uploads(up.path(), &["n1.bin", "n2.bin", "n3.bin"]);
            assert!(pays(&mut provider, &files, true).await);
            std::fs::write(
                dir.path().join("listing.txt"),
                b"----  1  6  15Jan2026  14:30 n2.bin\n",
            )
            .unwrap();
            assert!(!pays(&mut provider, &files, true).await);
            assert_eq!(
                read(&dir.path().join("put.log")),
                "",
                "admission sent nothing"
            );
        }

        /// An admitted group whose transfer then fails for one file still
        /// hands that file to the single-file path, as before admission
        /// existed; the others are published.
        #[tokio::test]
        async fn an_admitted_group_that_fails_midway_goes_back_file_by_file() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            std::fs::write(dir.path().join("get-fail-b.bin"), b"").unwrap();
            let local = tempfile::tempdir().unwrap();
            let items = downloads(dir.path(), local.path(), &["a.bin", "b.bin", "c.bin"], 8);
            assert!(pays(&mut provider, &items, false).await);
            let outcomes = provider.bulk_download(&items, None).await.unwrap();
            assert!(
                matches!(outcomes[0], BulkTransferOutcome::Completed),
                "{outcomes:?}"
            );
            assert!(
                crate::providers::bulk::goes_back_to_single_file(&outcomes[1]),
                "{outcomes:?}"
            );
            assert!(
                matches!(outcomes[2], BulkTransferOutcome::Completed),
                "{outcomes:?}"
            );
        }

        /// A put cut for silence may leave sources the server took but the
        /// client never announced: such a file is uncertain, never sent again
        /// by the single-file path, even when MEGA does not list it yet.
        #[tokio::test]
        async fn a_cut_put_leaves_unannounced_files_uncertain() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            std::fs::write(dir.path().join("listing.txt"), b"").unwrap();
            std::fs::write(dir.path().join("put-stall"), b"").unwrap();
            let local = tempfile::tempdir().unwrap();
            let items = uploads(local.path(), &["a.bin", "b.bin"]);
            let outcomes = provider.bulk_upload(&items, None).await.unwrap();
            assert!(
                matches!(outcomes[0], BulkTransferOutcome::Uncertain(_)),
                "{outcomes:?}"
            );
            assert!(
                matches!(outcomes[1], BulkTransferOutcome::Uncertain(_)),
                "{outcomes:?}"
            );
            assert_eq!(read(&dir.path().join("cancel.log")).trim(), "101");
        }

        fn staging_left(local: &Path) -> bool {
            !no_staging_left(local)
        }

        /// The server's record cannot be read after the get, or one of the
        /// bulk's unfinished transfers cannot be cancelled: the server may
        /// still write into staging, so the staging folder stays (hidden from
        /// scans by its prefix), nothing is published and every file goes
        /// back to the single-file path.
        #[tokio::test]
        async fn staging_stays_while_the_server_may_still_write() {
            for marker in ["transfers-fail", "cancel-fail"] {
                let (mut provider, dir) = provider_on_bulk_stand_in();
                std::fs::write(dir.path().join(marker), b"").unwrap();
                if marker == "cancel-fail" {
                    std::fs::write(dir.path().join("get-active"), b"").unwrap();
                }
                let local = tempfile::tempdir().unwrap();
                let items = downloads(dir.path(), local.path(), &["a.bin", "b.bin"], 8);
                let outcomes = provider.bulk_download(&items, None).await.unwrap();
                assert!(
                    outcomes
                        .iter()
                        .all(|o| matches!(o, BulkTransferOutcome::NotAttempted(_))),
                    "{marker}: {outcomes:?}"
                );
                assert!(
                    items.iter().all(|i| !Path::new(&i.local_path).exists()),
                    "{marker}"
                );
                assert!(staging_left(local.path()), "{marker}: staging removed");
                if marker == "cancel-fail" {
                    let cancelled = read(&dir.path().join("cancel.log"));
                    assert!(cancelled.lines().count() >= 3, "{cancelled}");
                }
            }
        }

        fn replay(dir: &Path) {
            let real = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/megacmd_real");
            std::fs::write(dir.join("replay"), real.to_string_lossy().as_bytes()).unwrap();
        }

        /// What MEGAcmd 2.6.0.2 printed for a two-file upload on a real
        /// account, replayed: the start lines and the record are read as they
        /// came, and both files are proven.
        #[tokio::test]
        async fn a_replayed_real_upload_is_proven_file_by_file() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            replay(dir.path());
            std::fs::write(dir.path().join("listing.txt"), b"").unwrap();
            let local = tempfile::tempdir().unwrap();
            let items = uploads(local.path(), &["cap-a.bin", "cap-b.bin"]);
            let outcomes = provider.bulk_upload(&items, None).await.unwrap();
            assert!(
                outcomes
                    .iter()
                    .all(|o| matches!(o, BulkTransferOutcome::Completed)),
                "{outcomes:?}"
            );
            assert_eq!(read(&dir.path().join("cancel.log")), "");
        }

        /// What MEGAcmd 2.6.0.2 printed for a 20-file folder get on a real
        /// account, replayed: the folder row, the 20 file rows among other
        /// transfers of the history, all proven and published.
        #[tokio::test]
        async fn a_replayed_real_folder_download_is_proven_file_by_file() {
            let (mut provider, dir) = provider_on_bulk_stand_in();
            replay(dir.path());
            let local = tempfile::tempdir().unwrap();
            let names: Vec<String> = (0..20).map(|i| format!("f{i:04}.bin")).collect();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            let mut items = downloads(dir.path(), local.path(), &names, 16);
            for item in &mut items {
                item.remote_path = item.remote_path.replace("/folder/", "/wm-aero2-B-r0/");
            }
            let outcomes = provider.bulk_download(&items, None).await.unwrap();
            assert!(
                outcomes
                    .iter()
                    .all(|o| matches!(o, BulkTransferOutcome::Completed)),
                "{outcomes:?}"
            );
            for item in &items {
                assert_eq!(std::fs::metadata(&item.local_path).unwrap().len(), 16);
            }
            assert!(no_staging_left(local.path()));
        }
    }
}
