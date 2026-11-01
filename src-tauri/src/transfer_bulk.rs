// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Bounded native bulk planning and the provider-executor boundary. No retry
//! wraps this operation: partial/unknown completion must not replay revisions.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::providers::{bulk::*, StorageProvider};
use crate::transfer_domain::{
    TransferEntry, TransferFailure, TransferFailureKind, TransferOutcome,
};
use crate::transfer_event_sink::TransferEventSink;
use crate::transfer_orchestrator::TransferExecutor;

pub(crate) fn group_key(entry: &TransferEntry, upload: bool) -> Option<String> {
    let name = plain_bulk_name(&entry.remote_path)?;
    // A relative path is planned in the absolute spelling the native program
    // resolves it to, so its command-line bytes are counted exactly. Literal
    // glob quoting can expand an argument beyond the planner's budget: such
    // paths keep the established single-file path.
    let absolute = absolute_local_path(&entry.local_path)?;
    let local = Path::new(&absolute);
    if !(local.is_absolute() || cfg!(windows) && local.has_root())
        || absolute.contains(['*', '?', '[', '{'])
    {
        return None;
    }
    if local.file_name()?.to_str()? != name {
        return None;
    }
    if upload {
        remote_parent(&entry.remote_path).map(str::to_string)
    } else {
        Some(format!(
            "{}\0{}",
            local.parent()?.to_str()?,
            remote_parent(&entry.remote_path)?
        ))
    }
}

/// Indices into the caller's entries; one native invocation per group.
pub(crate) type BulkGroups = Vec<Vec<usize>>;

pub(crate) fn plan_groups<E: TransferExecutor>(
    entries: &[TransferEntry],
    executor: &E,
) -> BulkGroups {
    plan_groups_by_key(entries, |entry| executor.bulk_group_key(entry))
}

/// Files that share a key share one bounded native invocation, also when the
/// caller interleaves them: a sync plan comes out of a hash map and a tree
/// walk alternates folders, and adjacency alone left v4.2.4's CLI sync of a
/// nested tree on one process per file. Groups follow the order of their
/// first file and keep caller order inside. Interleaved files are gathered
/// only when every destination in the batch is distinct, so reordering cannot
/// change which of two writes to one path lands last; otherwise only adjacent
/// files are grouped. Duplicate portable basenames split a group, even if the
/// filesystem happens to be case-sensitive on this host.
pub(crate) fn plan_groups_by_key(
    entries: &[TransferEntry],
    key_for: impl Fn(&TransferEntry) -> Option<String>,
) -> BulkGroups {
    struct Open {
        group: usize,
        bytes: u64,
        arg_bytes: usize,
        names: HashSet<String>,
    }
    let gather = destinations_are_distinct(entries);
    let mut groups: BulkGroups = Vec::new();
    let mut open: HashMap<String, Open> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let absolute = absolute_local_path(&entry.local_path).unwrap_or_default();
        let arg_bytes = command_arg_bytes(&absolute, &entry.remote_path);
        let name = entry
            .remote_path
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_lowercase();
        let key = key_for(entry).filter(|_| {
            entry.size <= MAX_BULK_BYTES
                && BULK_COMMAND_RESERVE_BYTES.saturating_add(arg_bytes) <= MAX_BULK_PATH_BYTES
        });
        let Some(key) = key else {
            if !gather {
                open.clear();
            }
            groups.push(vec![index]);
            continue;
        };
        if !gather {
            open.retain(|open_key, _| open_key == &key);
        }
        if let Some(current) = open.get_mut(&key) {
            let next_bytes = current.bytes.saturating_add(entry.size);
            let next_args = current.arg_bytes.saturating_add(arg_bytes);
            if groups[current.group].len() < MAX_BULK_FILES
                && next_bytes <= MAX_BULK_BYTES
                && next_args <= MAX_BULK_PATH_BYTES
                && !current.names.contains(&name)
            {
                groups[current.group].push(index);
                current.bytes = next_bytes;
                current.arg_bytes = next_args;
                current.names.insert(name);
                continue;
            }
        }
        groups.push(vec![index]);
        open.insert(
            key,
            Open {
                group: groups.len() - 1,
                bytes: entry.size,
                arg_bytes: BULK_COMMAND_RESERVE_BYTES.saturating_add(arg_bytes),
                names: HashSet::from([name]),
            },
        );
    }
    groups
}

/// Upload and download write different sides, so both are checked; local
/// spellings are case-folded because the local filesystem may ignore case.
fn destinations_are_distinct(entries: &[TransferEntry]) -> bool {
    let mut remote = HashSet::with_capacity(entries.len());
    let mut local = HashSet::with_capacity(entries.len());
    entries.iter().all(|entry| {
        remote.insert(entry.remote_path.as_str()) && local.insert(entry.local_path.to_lowercase())
    })
}

pub(crate) fn emit_event(
    sink: &dyn TransferEventSink,
    entry: &TransferEntry,
    upload: bool,
    kind: &str,
    failure: Option<&TransferFailure>,
) {
    let direction = if upload { "upload" } else { "download" };
    sink.emit_transfer_event(crate::TransferEvent {
        event_type: kind.into(),
        transfer_id: entry.id.clone(),
        filename: entry.display_name.clone(),
        direction: direction.into(),
        message: failure.map(|f| f.message.clone()),
        progress: (kind == "file_start").then(|| crate::TransferProgress {
            transfer_id: entry.id.clone(),
            filename: entry.display_name.clone(),
            transferred: 0,
            total: entry.size,
            percentage: 0,
            speed_bps: 0,
            eta_seconds: 0,
            direction: direction.into(),
            total_files: None,
            path: None,
        }),
        path: Some(entry.remote_path.clone()),
        delta_stats: None,
        fallback_reason: None,
    });
}

/// What the generic layer does with one item after the native bulk returned.
pub(crate) enum BulkSettlement {
    Done,
    /// Run the provider's single-file operation once. Only for an item that
    /// never reached the wire, or one the native program reported as failed
    /// for a transient reason: neither can duplicate a committed write.
    SingleFile,
    Failed(TransferFailure),
}

pub(crate) fn settle_bulk_outcome(
    outcome: BulkTransferOutcome,
    local_path: &str,
) -> BulkSettlement {
    if goes_back_to_single_file(&outcome) {
        return BulkSettlement::SingleFile;
    }
    match outcome {
        BulkTransferOutcome::Completed => BulkSettlement::Done,
        BulkTransferOutcome::Failed(error) => {
            let mut failure =
                crate::transfer_multipart::transfer_failure_from_provider(&error, Some(local_path));
            failure.retryable = false;
            BulkSettlement::Failed(failure)
        }
        // May already have reached the server: never replayed.
        BulkTransferOutcome::Uncertain(message) => BulkSettlement::Failed(TransferFailure::new(
            TransferFailureKind::RemoteIo,
            message,
            false,
        )),
        BulkTransferOutcome::NotAttempted(_) => BulkSettlement::SingleFile,
    }
}

/// One single-file attempt on the provider the bulk used. The adapter's own
/// single-file path keeps its transient retries; no further replay follows.
pub(crate) async fn single_file_transfer(
    provider: &mut dyn StorageProvider,
    item: &BulkTransferItem,
    upload: bool,
    per_file_seconds: u64,
) -> Result<(), crate::providers::ProviderError> {
    let timeout = bulk_timeout(per_file_seconds, 1, item.size);
    let operation = async {
        if upload {
            provider
                .upload(&item.local_path, &item.remote_path, None)
                .await
        } else {
            provider
                .download(&item.remote_path, &item.local_path, None)
                .await
        }
    };
    tokio::time::timeout(timeout, operation)
        .await
        .unwrap_or(Err(crate::providers::ProviderError::Timeout))
}

/// The provider-facing item: relative local paths become absolute here, as
/// they were when the planner counted them.
pub(crate) fn bulk_item(local_path: &str, remote_path: &str, size: u64) -> BulkTransferItem {
    BulkTransferItem {
        local_path: absolute_local_path(local_path).unwrap_or_else(|| local_path.to_string()),
        remote_path: remote_path.to_string(),
        size,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_provider_bulk(
    provider: &Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    sink: &Arc<dyn TransferEventSink>,
    entries: &[TransferEntry],
    upload: bool,
    token: &CancellationToken,
    cancel: &AtomicBool,
    timeout_seconds: u64,
) -> Vec<TransferOutcome> {
    for entry in entries {
        emit_event(sink.as_ref(), entry, upload, "file_start", None);
    }
    let mut lock = tokio::select! {
        biased;
        _ = cancelled(token, cancel) => return failed_bulk(entries, sink.as_ref(), upload, cancelled_failure()),
        lock = provider.lock() => lock,
    };
    let Some(provider) = lock.as_mut() else {
        return failed_bulk(
            entries,
            sink.as_ref(),
            upload,
            TransferFailure::new(
                TransferFailureKind::ConnectionLost,
                "Provider disconnected",
                false,
            ),
        );
    };
    let mut settled: Vec<Option<TransferOutcome>> = vec![None; entries.len()];
    // An invalid path fails on its own, before a native process can modify
    // any item; its siblings still travel together.
    let mut submitted = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let validation = if upload {
            crate::restricted_chars::validate_path(provider.provider_type(), &entry.remote_path)
        } else {
            // Read existing remote names as-is; creation rules do not apply
            // to downloads, whose local destination is already explicit.
            Ok(())
        };
        match validation {
            Ok(()) => submitted.push(index),
            Err(error) => {
                let failure = TransferFailure::new(
                    TransferFailureKind::InvalidPath,
                    error.to_string(),
                    false,
                );
                emit_event(sink.as_ref(), entry, upload, "file_error", Some(&failure));
                settled[index] = Some(TransferOutcome::Failed(failure));
            }
        }
    }
    let items: Vec<_> = submitted
        .iter()
        .map(|&index| {
            let e = &entries[index];
            bulk_item(&e.local_path, &e.remote_path, e.size)
        })
        .collect();
    let progress_entries: Vec<_> = submitted.iter().map(|&i| entries[i].clone()).collect();
    let progress_sink = Arc::clone(sink);
    let progress_token = token.clone();
    let on_progress: BulkProgress = Box::new(move |index, sent, total| {
        if progress_token.is_cancelled() {
            return;
        }
        let Some(entry) = progress_entries.get(index) else {
            return;
        };
        let direction = if upload { "upload" } else { "download" };
        progress_sink.emit_transfer_event(crate::TransferEvent {
            event_type: "progress".into(),
            transfer_id: entry.id.clone(),
            filename: entry.display_name.clone(),
            direction: direction.into(),
            message: None,
            progress: Some(crate::TransferProgress {
                transfer_id: entry.id.clone(),
                filename: entry.display_name.clone(),
                transferred: sent,
                total,
                percentage: if total == 0 {
                    0
                } else {
                    ((sent.min(total) as u128 * 100) / total as u128) as u8
                },
                speed_bps: 0,
                eta_seconds: 0,
                direction: direction.into(),
                total_files: None,
                path: None,
            }),
            path: Some(entry.remote_path.clone()),
            delta_stats: None,
            fallback_reason: None,
        });
    });
    let bytes = items
        .iter()
        .fold(0u64, |n, item| n.saturating_add(item.size));
    // The native CLI may resolve selected paths serially before transferring.
    // Retain the per-file timeout budget for every submitted item; a Stop is
    // still observed immediately through both cancellation sources.
    let timeout = bulk_timeout(timeout_seconds, items.len(), bytes);
    let results = if items.is_empty() {
        Vec::new()
    } else {
        let operation = async {
            if upload {
                provider.upload_bulk(&items, Some(on_progress)).await
            } else {
                provider.download_bulk(&items, Some(on_progress)).await
            }
        };
        let raw = tokio::select! {
            biased;
            _ = cancelled(token, cancel) => None,
            result = tokio::time::timeout(timeout, operation) => Some(result),
        };
        match raw {
            None => {
                for &index in &submitted {
                    let failure = cancelled_failure();
                    emit_event(
                        sink.as_ref(),
                        &entries[index],
                        upload,
                        "file_error",
                        Some(&failure),
                    );
                    settled[index] = Some(TransferOutcome::Failed(failure));
                }
                return settled.into_iter().map(Option::unwrap).collect();
            }
            Some(Ok(Ok(results))) if results.len() == items.len() => results,
            // Refused before any item was handed to the native program.
            Some(Ok(Err(error))) => items
                .iter()
                .map(|_| {
                    BulkTransferOutcome::Failed(crate::providers::ProviderError::TransferFailed(
                        error.to_string(),
                    ))
                })
                .collect(),
            _ => uncertain_items(
                items.len(),
                "Bulk did not return complete outcomes; inspect the destination before retrying",
            ),
        }
    };
    let mut single_file = Vec::new();
    for ((&index, item), result) in submitted.iter().zip(&items).zip(results) {
        let entry = &entries[index];
        match settle_bulk_outcome(result, &item.local_path) {
            BulkSettlement::Done => {
                if !upload {
                    crate::preserve_remote_mtime(&entry.local_path, entry.modified.as_deref());
                }
                emit_event(sink.as_ref(), entry, upload, "file_complete", None);
                settled[index] = Some(TransferOutcome::Success);
            }
            BulkSettlement::SingleFile => single_file.push((index, item.clone())),
            BulkSettlement::Failed(failure) => {
                emit_event(sink.as_ref(), entry, upload, "file_error", Some(&failure));
                settled[index] = Some(TransferOutcome::Failed(failure));
            }
        }
    }
    for (index, item) in single_file {
        let entry = &entries[index];
        let result = tokio::select! {
            biased;
            _ = cancelled(token, cancel) => None,
            result = single_file_transfer(provider.as_mut(), &item, upload, timeout_seconds) => Some(result),
        };
        settled[index] = Some(match result {
            Some(Ok(())) => {
                if !upload {
                    let read_time = provider.take_download_time(false);
                    crate::preserve_remote_mtime(
                        &entry.local_path,
                        read_time.as_deref().or(entry.modified.as_deref()),
                    );
                }
                emit_event(sink.as_ref(), entry, upload, "file_complete", None);
                TransferOutcome::Success
            }
            other => {
                let mut failure = match other {
                    Some(Err(error)) => crate::transfer_multipart::transfer_failure_from_provider(
                        &error,
                        Some(&entry.local_path),
                    ),
                    _ => crate::transfer_multipart::cancelled_failure(),
                };
                failure.retryable = false;
                emit_event(sink.as_ref(), entry, upload, "file_error", Some(&failure));
                TransferOutcome::Failed(failure)
            }
        });
    }
    settled
        .into_iter()
        .map(|outcome| {
            outcome.unwrap_or_else(|| {
                TransferOutcome::Failed(TransferFailure::new(
                    TransferFailureKind::Unknown,
                    "Bulk item left unsettled; destination state unknown",
                    false,
                ))
            })
        })
        .collect()
}

/// The two directions a connected provider moves through its native bulk,
/// by the rule the GUI batch executors apply: a provider that resumes a
/// transfer keeps its single-file path, which owns the resume.
pub(crate) fn bulk_directions(caps: &crate::transfer_dag::TransferCapabilities) -> (bool, bool) {
    use crate::transfer_dag::Capability;
    (
        caps.bulk_upload == Capability::Supported && !caps.resume_upload.is_available(),
        caps.bulk_download == Capability::Supported && !caps.resume_download.is_available(),
    )
}

/// One file of an AeroSync run offered to the native bulk.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncBulkItem {
    pub local_path: String,
    pub remote_path: String,
    pub size: u64,
    /// The remote time a download gives the local copy, as the single-file
    /// download does.
    #[serde(default)]
    pub modified: Option<String>,
}

/// What became of one file of [`run_sync_bulk`], in the order the run sent
/// them.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SyncBulkResult {
    Completed,
    /// Final: the file was handed to the native program (and to one
    /// single-file attempt when nothing of it reached the wire), and may have
    /// reached the server, so the run does not send it again.
    Failed {
        message: String,
    },
    /// Back to the run's own single-file path, which keeps its retries and
    /// timeouts: the bulk cannot carry the file (a name the native program
    /// would rename, a file alone in its folder), or a Stop came before its
    /// group started.
    NotAttempted,
}

/// AeroSync's native bulk: the files a sync run sends in a row, grouped as
/// the GUI batch and the CLI group them, one native call per group, with one
/// outcome per file in the run's order. `token` is the session's live cancel
/// token, never a fresh one: the run resets it once when it starts, and a
/// reset here would swallow a Stop that came in between.
pub(crate) async fn run_sync_bulk(
    provider: &Arc<Mutex<Option<Box<dyn StorageProvider>>>>,
    sink: &Arc<dyn TransferEventSink>,
    items: &[SyncBulkItem],
    upload: bool,
    token: &CancellationToken,
    timeout_seconds: u64,
) -> Vec<SyncBulkResult> {
    let run = chrono::Utc::now().timestamp_millis();
    let entries: Vec<TransferEntry> = items
        .iter()
        .enumerate()
        .map(|(index, item)| TransferEntry {
            id: format!("sync-bulk-{run}-{index}"),
            display_name: item
                .remote_path
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string(),
            remote_path: item.remote_path.clone(),
            local_path: item.local_path.clone(),
            size: item.size,
            modified: item.modified.clone(),
        })
        .collect();
    let mut results = vec![SyncBulkResult::NotAttempted; items.len()];
    // The run's own Stop goes through `token`; this flag is the batch's
    // "start no further file", which a sync run does not use.
    let no_batch_stop = AtomicBool::new(false);
    for group in plan_groups_by_key(&entries, |entry| group_key(entry, upload)) {
        if group.len() < 2 || token.is_cancelled() {
            continue;
        }
        let members: Vec<TransferEntry> = group.iter().map(|&i| entries[i].clone()).collect();
        let outcomes = execute_provider_bulk(
            provider,
            sink,
            &members,
            upload,
            token,
            &no_batch_stop,
            timeout_seconds,
        )
        .await;
        for (&index, outcome) in group.iter().zip(outcomes) {
            results[index] = match outcome {
                TransferOutcome::Success => SyncBulkResult::Completed,
                TransferOutcome::Failed(failure) => SyncBulkResult::Failed {
                    message: failure.message,
                },
                TransferOutcome::Skipped { reason } => SyncBulkResult::Failed { message: reason },
            };
        }
    }
    results
}

fn bulk_timeout(per_file_seconds: u64, files: usize, bytes: u64) -> Duration {
    Duration::from_secs(
        per_file_seconds
            .saturating_mul(files as u64)
            .saturating_add(bytes / 50_000),
    )
}

fn failed_bulk(
    entries: &[TransferEntry],
    sink: &dyn TransferEventSink,
    upload: bool,
    failure: TransferFailure,
) -> Vec<TransferOutcome> {
    entries
        .iter()
        .map(|entry| {
            emit_event(sink, entry, upload, "file_error", Some(&failure));
            TransferOutcome::Failed(failure.clone())
        })
        .collect()
}

fn cancelled_failure() -> TransferFailure {
    TransferFailure::new(
        TransferFailureKind::Cancelled,
        "Bulk cancelled; some files may already have reached the destination",
        false,
    )
}

async fn cancelled(token: &CancellationToken, cancel: &AtomicBool) {
    loop {
        if token.is_cancelled() || cancel.load(Ordering::Relaxed) {
            return;
        }
        tokio::select! {
            _ = token.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(50)) => {},
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::transfer_dag::{Capability, TransferCapabilities, TransferSessionLease};
    use crate::transfer_domain::{TransferBatchConfig, TransferDirection};
    use crate::transfer_event_sink::NoopTransferSink;
    use crate::transfer_orchestrator::{execute_batch, TransferBatch};
    use async_trait::async_trait;

    struct BulkExecutor {
        calls: std::sync::Mutex<Vec<usize>>,
        cancel_after_first: bool,
        partial: bool,
        panic: bool,
    }
    #[async_trait]
    impl TransferExecutor for BulkExecutor {
        async fn execute(&self, _: TransferEntry) -> TransferOutcome {
            panic!("bulk group decomposed into per-file calls")
        }
        fn bulk_group_key(&self, entry: &TransferEntry) -> Option<String> {
            group_key(entry, true)
        }
        fn transfer_capabilities(&self) -> TransferCapabilities {
            TransferCapabilities {
                bulk_upload: Capability::Supported,
                ..Default::default()
            }
        }
        async fn execute_bulk_with_session(
            &self,
            entries: Vec<TransferEntry>,
            _: TransferSessionLease,
            cancel: Arc<AtomicBool>,
        ) -> Vec<TransferOutcome> {
            self.calls.lock().unwrap().push(entries.len());
            assert!(!self.panic, "injected native bulk panic");
            if self.cancel_after_first {
                cancel.store(true, Ordering::Relaxed);
            }
            entries
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    if self.partial && i == 1 {
                        TransferOutcome::Failed(TransferFailure::new(
                            TransferFailureKind::PermissionDenied,
                            "Refused",
                            false,
                        ))
                    } else {
                        TransferOutcome::Success
                    }
                })
                .collect()
        }
    }
    fn entries(count: usize) -> Vec<TransferEntry> {
        (0..count)
            .map(|i| TransferEntry {
                id: i.to_string(),
                display_name: format!("file-{i}.txt"),
                remote_path: format!("/bulk/file-{i}.txt"),
                local_path: format!("/tmp/bulk/file-{i}.txt"),
                size: 4,
                modified: None,
            })
            .collect()
    }
    fn executor() -> BulkExecutor {
        BulkExecutor {
            calls: Default::default(),
            cancel_after_first: false,
            partial: false,
            panic: false,
        }
    }
    async fn run(
        count: usize,
        executor: Arc<BulkExecutor>,
    ) -> crate::transfer_domain::TransferBatchResult {
        execute_batch(
            Arc::new(NoopTransferSink),
            TransferBatch {
                id: "bulk-test".into(),
                display_name: "Bulk".into(),
                direction: TransferDirection::Upload,
                entries: entries(count),
                config: TransferBatchConfig {
                    max_concurrent: 1,
                    ..Default::default()
                },
            },
            executor,
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
    }
    #[test]
    fn bulk_timeout_retains_every_files_budget_before_native_io() {
        assert_eq!(bulk_timeout(30, 200, 819200), Duration::from_secs(6016));
        assert_eq!(bulk_timeout(30, 1, 0), Duration::from_secs(30));
    }

    #[test]
    fn bulk_groups_are_bounded_by_files_payload_paths_and_collisions() {
        let e = executor();
        let all: Vec<usize> = (0..300).collect();
        assert_eq!(
            plan_groups(&entries(300), &e),
            vec![all[..256].to_vec(), all[256..].to_vec()]
        );
        let mut files = entries(3);
        files[1].size = MAX_BULK_BYTES;
        assert_eq!(plan_groups(&files, &e), vec![vec![0], vec![1], vec![2]]);
        // A repeated destination keeps the adjacent-only plan: order decides
        // which write lands last.
        files = entries(3);
        files[1] = files[0].clone();
        assert_eq!(plan_groups(&files, &e), vec![vec![0], vec![1, 2]]);
        files = entries(3);
        files[1].local_path = format!("/{}/file-1.txt", "a".repeat(MAX_BULK_PATH_BYTES));
        assert_eq!(plan_groups(&files, &e), vec![vec![0, 2], vec![1]]);
        files = entries(3);
        files[1].remote_path = "/other/file-1.txt".into();
        assert_eq!(plan_groups(&files, &e), vec![vec![0, 2], vec![1]]);
    }

    /// v4.2.4 grouped only adjacent files. A CLI sync plan comes out of a hash
    /// map, so a nested tree interleaved its folders and every group was a
    /// single file (live: 80 launches for an 80-file, 5-folder tree).
    #[test]
    fn bulk_groups_gather_interleaved_folders_in_first_seen_order() {
        let e = executor();
        let files: Vec<_> = (0..12)
            .map(|i| TransferEntry {
                id: i.to_string(),
                display_name: format!("f{i}.txt"),
                remote_path: format!("/bulk/d{}/f{i}.txt", i % 3),
                local_path: format!("/tmp/bulk/d{}/f{i}.txt", i % 3),
                size: 4,
                modified: None,
            })
            .collect();
        assert_eq!(
            plan_groups(&files, &e),
            vec![vec![0, 3, 6, 9], vec![1, 4, 7, 10], vec![2, 5, 8, 11]]
        );
    }

    #[test]
    fn bulk_groups_count_only_the_command_line_side_of_each_file() {
        let e = executor();
        // A deep local tree: 300-byte local paths, short remote paths. The
        // v4.2.4 planner counted both sides and split 256 such files.
        let deep = "d".repeat(280);
        let files: Vec<_> = (0..256)
            .map(|i| TransferEntry {
                id: i.to_string(),
                display_name: format!("f{i:03}.txt"),
                remote_path: format!("/bulk/f{i:03}.txt"),
                local_path: format!("/{deep}/f{i:03}.txt"),
                size: 4,
                modified: None,
            })
            .collect();
        let groups = plan_groups(&files, &e);
        // The budget rule written out: the longer of the absolute local path
        // and the remote path (at least 64 bytes), plus 3. On Windows the
        // absolute spelling gains the drive ("D:"), so it is measured, not
        // assumed: 295 bytes there, 79 per 24 KiB group.
        let absolute = absolute_local_path(&files[0].local_path).unwrap();
        let per_file = absolute.len().max(files[0].remote_path.len().max(64)) + 3;
        let fit = (MAX_BULK_PATH_BYTES - BULK_COMMAND_RESERVE_BYTES) / per_file;
        assert_eq!(groups[0].len(), fit.min(MAX_BULK_FILES));
        // Outside Windows all 256 fit one 96 KiB line; v4.2.4 made groups
        // of 75.
        if !cfg!(windows) {
            assert_eq!(groups[0].len(), 256);
        }
    }

    #[test]
    fn bulk_names_requiring_a_rename_or_portable_mapping_keep_single_file_path() {
        let mut entry = entries(1).remove(0);
        entry.remote_path = "/bulk/renamed.txt".into();
        assert!(group_key(&entry, true).is_none());
        entry.remote_path = "/bulk/question?.txt".into();
        entry.local_path = "/tmp/bulk/question?.txt".into();
        assert!(group_key(&entry, false).is_none());
    }

    #[test]
    fn bulk_admits_relative_paths_and_keeps_glob_expanding_parents_on_single_file_lane() {
        let mut entry = entries(1).remove(0);
        entry.local_path = "file-0.txt".into();
        entry.remote_path = "/bulk/file-0.txt".into();
        assert_eq!(group_key(&entry, true).as_deref(), Some("/bulk"));
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            group_key(&entry, false),
            Some(format!("{}\0/bulk", cwd.to_str().unwrap()))
        );
        entry.local_path = "/tmp/[archive]/file-0.txt".into();
        assert!(group_key(&entry, true).is_none());
        assert!(group_key(&entry, false).is_none());
    }

    #[test]
    fn bulk_download_groups_do_not_join_different_remote_parents() {
        let files = entries(2);
        let mut other = files[1].clone();
        other.remote_path = "/elsewhere/file-1.txt".into();
        assert_eq!(group_key(&files[0], false), group_key(&files[1], false));
        assert_ne!(group_key(&files[0], false), group_key(&other, false));
    }

    #[test]
    fn bulk_capabilities_default_to_unsupported_on_old_snapshots() {
        let mut old = serde_json::to_value(TransferCapabilities::default()).unwrap();
        old.as_object_mut().unwrap().remove("bulk_upload");
        old.as_object_mut().unwrap().remove("bulk_download");
        let caps: TransferCapabilities = serde_json::from_value(old).unwrap();
        assert_eq!(caps.bulk_upload, Capability::Unsupported);
        assert_eq!(caps.bulk_download, Capability::Unsupported);
    }

    #[tokio::test]
    async fn proton_bulk_upload_isolates_a_restricted_item_before_the_native_call() {
        let scripted = ScriptedBulk {
            kind: Some(crate::providers::ProviderType::Proton),
            ..Default::default()
        };
        let bulks = scripted.bulks.clone();
        let provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>> =
            Arc::new(Mutex::new(Some(Box::new(scripted))));
        let sink: Arc<dyn TransferEventSink> = Arc::new(NoopTransferSink);
        let mut files = entries(3);
        files[1].remote_path = "/r/bad\0name".into();
        let outcomes = execute_provider_bulk(
            &provider,
            &sink,
            &files,
            true,
            &CancellationToken::new(),
            &AtomicBool::new(false),
            30,
        )
        .await;
        assert!(matches!(outcomes[0], TransferOutcome::Success));
        assert!(
            matches!(&outcomes[1], TransferOutcome::Failed(e) if e.message.contains("U+0000") && !e.retryable)
        );
        assert!(matches!(outcomes[2], TransferOutcome::Success));
        assert_eq!(
            bulks.lock().unwrap()[0],
            vec![files[0].remote_path.clone(), files[2].remote_path.clone()]
        );
    }

    #[tokio::test]
    async fn bulk_download_reads_existing_control_names_without_upload_validation() {
        let scripted = ScriptedBulk::default();
        let bulks = scripted.bulks.clone();
        let provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>> =
            Arc::new(Mutex::new(Some(Box::new(scripted))));
        let sink: Arc<dyn TransferEventSink> = Arc::new(NoopTransferSink);
        let mut files = entries(2);
        files[0].remote_path = "/existing/tab\tname".into();
        files[1].remote_path = "/existing/line\nname".into();
        let outcomes = execute_provider_bulk(
            &provider,
            &sink,
            &files,
            false,
            &CancellationToken::new(),
            &AtomicBool::new(false),
            30,
        )
        .await;
        assert!(outcomes
            .iter()
            .all(|o| matches!(o, TransferOutcome::Success)));
        assert_eq!(
            bulks.lock().unwrap()[0],
            files
                .iter()
                .map(|e| e.remote_path.clone())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn bulk_early_failure_keeps_per_file_start_size_and_terminal_events() {
        #[derive(Default)]
        struct Events(std::sync::Mutex<Vec<crate::TransferEvent>>);
        impl TransferEventSink for Events {
            fn emit_transfer_event(&self, event: crate::TransferEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        for stop in [false, true] {
            let events = Arc::new(Events::default());
            let sink: Arc<dyn TransferEventSink> = events.clone();
            let token = CancellationToken::new();
            if stop {
                token.cancel();
            }
            let files = entries(2);
            let provider = Arc::new(Mutex::new(None));
            let outcomes = execute_provider_bulk(
                &provider,
                &sink,
                &files,
                false,
                &token,
                &AtomicBool::new(false),
                30,
            )
            .await;
            assert!(outcomes
                .iter()
                .all(|o| matches!(o, TransferOutcome::Failed(_))));
            let recorded = events.0.lock().unwrap();
            for file in files {
                let row: Vec<_> = recorded
                    .iter()
                    .filter(|event| event.transfer_id == file.id)
                    .collect();
                assert_eq!(row.len(), 2);
                assert_eq!(row[0].event_type, "file_start");
                assert_eq!(row[0].progress.as_ref().unwrap().total, 4);
                assert_eq!(row[1].event_type, "file_error");
            }
        }
    }
    #[tokio::test]
    async fn bulk_dag_accounts_each_file_and_partial_failure_once() {
        let mut e = executor();
        e.partial = true;
        let e = Arc::new(e);
        let result = run(200, Arc::clone(&e)).await;
        assert_eq!(
            (result.completed, result.failed, result.total),
            (199, 1, 200)
        );
        assert_eq!(*e.calls.lock().unwrap(), vec![200]);
        assert_eq!(result.engine_stats.unwrap().metrics.logical_bytes, 199 * 4);
    }
    #[tokio::test]
    async fn bulk_cancel_leaves_later_groups_unstarted() {
        let mut e = executor();
        e.cancel_after_first = true;
        let e = Arc::new(e);
        let result = run(300, Arc::clone(&e)).await;
        assert!(result.cancelled);
        assert_eq!(
            (result.completed, result.failed, result.total),
            (256, 0, 300)
        );
        assert_eq!(*e.calls.lock().unwrap(), vec![256]);
    }
    /// A provider with a native bulk whose answer for a file depends on its
    /// name: `fail` fails, `unsure` comes back uncertain, `skip` was not
    /// attempted; anything else completes. `hang` keeps the bulk running and
    /// `refusal` refuses the whole call.
    #[derive(Default)]
    pub(crate) struct ScriptedBulk {
        pub(crate) kind: Option<crate::providers::ProviderType>,
        pub(crate) kind_queries: Arc<std::sync::atomic::AtomicUsize>,
        pub(crate) bulks: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        pub(crate) singles: Arc<std::sync::Mutex<Vec<String>>>,
        pub(crate) hang: bool,
        pub(crate) refusal: Option<fn() -> crate::providers::ProviderError>,
    }

    #[async_trait]
    impl StorageProvider for ScriptedBulk {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> crate::providers::ProviderType {
            self.kind_queries.fetch_add(1, Ordering::SeqCst);
            self.kind.unwrap_or(crate::providers::ProviderType::Sftp)
        }
        fn display_name(&self) -> String {
            "scripted bulk".into()
        }
        async fn connect(&mut self) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(
            &mut self,
            _: &str,
        ) -> Result<Vec<crate::providers::RemoteEntry>, crate::providers::ProviderError> {
            Ok(Vec::new())
        }
        async fn pwd(&mut self) -> Result<String, crate::providers::ProviderError> {
            Ok("/".into())
        }
        async fn cd(&mut self, _: &str) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            remote: &str,
            _: &str,
            _: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), crate::providers::ProviderError> {
            self.singles.lock().unwrap().push(remote.to_string());
            Ok(())
        }
        async fn download_to_bytes(
            &mut self,
            _: &str,
        ) -> Result<Vec<u8>, crate::providers::ProviderError> {
            Ok(Vec::new())
        }
        async fn upload(
            &mut self,
            _: &str,
            remote: &str,
            _: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), crate::providers::ProviderError> {
            self.singles.lock().unwrap().push(remote.to_string());
            Ok(())
        }
        async fn mkdir(&mut self, _: &str) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn delete(&mut self, _: &str) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn rmdir(&mut self, _: &str) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(
            &mut self,
            _: &str,
        ) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn rename(
            &mut self,
            _: &str,
            _: &str,
        ) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn stat(
            &mut self,
            path: &str,
        ) -> Result<crate::providers::RemoteEntry, crate::providers::ProviderError> {
            Err(crate::providers::ProviderError::NotFound(path.into()))
        }
        async fn size(&mut self, _: &str) -> Result<u64, crate::providers::ProviderError> {
            Ok(0)
        }
        async fn exists(&mut self, _: &str) -> Result<bool, crate::providers::ProviderError> {
            Ok(false)
        }
        async fn keep_alive(&mut self) -> Result<(), crate::providers::ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, crate::providers::ProviderError> {
            Ok(String::new())
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
            _: Option<BulkProgress>,
        ) -> Result<Vec<BulkTransferOutcome>, crate::providers::ProviderError> {
            self.scripted(items).await
        }
        async fn download_bulk(
            &mut self,
            items: &[BulkTransferItem],
            _: Option<BulkProgress>,
        ) -> Result<Vec<BulkTransferOutcome>, crate::providers::ProviderError> {
            self.scripted(items).await
        }
    }

    impl ScriptedBulk {
        async fn scripted(
            &self,
            items: &[BulkTransferItem],
        ) -> Result<Vec<BulkTransferOutcome>, crate::providers::ProviderError> {
            self.bulks
                .lock()
                .unwrap()
                .push(items.iter().map(|i| i.remote_path.clone()).collect());
            if self.hang {
                std::future::pending::<()>().await;
            }
            if let Some(refusal) = self.refusal {
                return Err(refusal());
            }
            Ok(items
                .iter()
                .map(|item| match &item.remote_path {
                    p if p.contains("fail") => BulkTransferOutcome::Failed(
                        crate::providers::ProviderError::PermissionDenied("refused".into()),
                    ),
                    p if p.contains("unsure") => {
                        BulkTransferOutcome::Uncertain("the program stopped".into())
                    }
                    p if p.contains("skip") => {
                        BulkTransferOutcome::NotAttempted("not handed over".into())
                    }
                    _ => BulkTransferOutcome::Completed,
                })
                .collect())
        }
    }

    fn sync_items(remote: &[&str]) -> Vec<SyncBulkItem> {
        remote
            .iter()
            .map(|path| SyncBulkItem {
                local_path: format!("/tmp/sync-bulk{path}"),
                remote_path: format!("/r{path}"),
                size: 4,
                modified: None,
            })
            .collect()
    }

    async fn sync_bulk(
        provider: ScriptedBulk,
        items: &[SyncBulkItem],
        upload: bool,
        token: &CancellationToken,
    ) -> Vec<SyncBulkResult> {
        let provider: Arc<Mutex<Option<Box<dyn StorageProvider>>>> =
            Arc::new(Mutex::new(Some(Box::new(provider))));
        let sink: Arc<dyn TransferEventSink> = Arc::new(NoopTransferSink);
        run_sync_bulk(&provider, &sink, items, upload, token, 30).await
    }

    /// An `Err` from a native bulk means nothing reached the wire. The GUI
    /// batch, the CLI and the AeroSync runner all settle it here: a transport
    /// failure runs each file once on the single-file path, any other refusal
    /// fails them all and sends nothing.
    #[tokio::test]
    async fn a_refused_sync_bulk_follows_the_shared_predicate() {
        use crate::providers::ProviderError as E;
        let transient: [fn() -> E; 3] = [
            || E::Timeout,
            || E::ConnectionLost("peer closed".into()),
            || E::ServerError("503 service unavailable".into()),
        ];
        for upload in [true, false] {
            for refusal in transient {
                let provider = ScriptedBulk {
                    refusal: Some(refusal),
                    ..Default::default()
                };
                let (bulks, singles) = (provider.bulks.clone(), provider.singles.clone());
                let items = sync_items(&["/a/1.txt", "/a/2.txt"]);
                let results = sync_bulk(provider, &items, upload, &CancellationToken::new()).await;
                assert_eq!(
                    results,
                    vec![SyncBulkResult::Completed, SyncBulkResult::Completed],
                    "{:?}",
                    refusal()
                );
                assert_eq!(bulks.lock().unwrap().len(), 1);
                assert_eq!(*singles.lock().unwrap(), vec!["/r/a/1.txt", "/r/a/2.txt"]);
            }
            let provider = ScriptedBulk {
                refusal: Some(|| E::PermissionDenied("read-only share".into())),
                ..Default::default()
            };
            let singles = provider.singles.clone();
            let items = sync_items(&["/a/1.txt", "/a/2.txt"]);
            let results = sync_bulk(provider, &items, upload, &CancellationToken::new()).await;
            assert!(
                results
                    .iter()
                    .all(|r| matches!(r, SyncBulkResult::Failed { .. })),
                "{results:?}"
            );
            assert!(singles.lock().unwrap().is_empty());
        }
    }

    /// AeroSync sent one `provider_upload_file` per file, so on Proton Drive
    /// in CLI mode every file paid a native program's start (Requested 77).
    /// The files of one folder go in one call, also when the run interleaves
    /// folders; a file alone in its folder goes back to the run, and the
    /// answers keep the run's order.
    #[tokio::test]
    async fn a_sync_run_sends_each_folder_in_one_bulk_and_keeps_its_order() {
        for upload in [true, false] {
            let provider = ScriptedBulk::default();
            let (bulks, singles) = (provider.bulks.clone(), provider.singles.clone());
            let items = sync_items(&[
                "/a/1.txt", "/b/1.txt", "/a/2.txt", "/b/2.txt", "/a/3.txt", "/c/1.txt",
            ]);
            let results = sync_bulk(provider, &items, upload, &CancellationToken::new()).await;
            use SyncBulkResult::*;
            assert_eq!(
                results,
                [
                    Completed,
                    Completed,
                    Completed,
                    Completed,
                    Completed,
                    NotAttempted
                ]
            );
            assert_eq!(
                *bulks.lock().unwrap(),
                [
                    vec!["/r/a/1.txt", "/r/a/2.txt", "/r/a/3.txt"],
                    vec!["/r/b/1.txt", "/r/b/2.txt"],
                ]
            );
            assert!(
                singles.lock().unwrap().is_empty(),
                "the lone file is the run's"
            );
        }
    }

    /// A file the native program never took gets one single-file attempt
    /// here; one it failed or may have sent is final, never sent again.
    #[tokio::test]
    async fn a_file_the_bulk_did_not_send_gets_one_attempt_and_a_doubtful_one_none() {
        let provider = ScriptedBulk::default();
        let singles = provider.singles.clone();
        let items = sync_items(&["/a/ok.txt", "/a/skip.txt", "/a/fail.txt", "/a/unsure.txt"]);
        let results = sync_bulk(provider, &items, true, &CancellationToken::new()).await;
        assert_eq!(results[0], SyncBulkResult::Completed);
        assert_eq!(results[1], SyncBulkResult::Completed);
        assert!(matches!(&results[2], SyncBulkResult::Failed { .. }));
        assert!(
            matches!(&results[3], SyncBulkResult::Failed { message } if message.contains("stopped"))
        );
        assert_eq!(*singles.lock().unwrap(), ["/r/a/skip.txt"]);
    }

    /// A Stop before a group leaves its files to the run, which records them
    /// as not sent; a Stop during a group ends it at once, and its files are
    /// failed, since some may have reached the server.
    #[tokio::test]
    async fn a_stop_before_a_group_leaves_it_and_a_stop_during_it_ends_it() {
        let provider = ScriptedBulk::default();
        let bulks = provider.bulks.clone();
        let stopped = CancellationToken::new();
        stopped.cancel();
        let items = sync_items(&["/a/1.txt", "/a/2.txt"]);
        let results = sync_bulk(provider, &items, true, &stopped).await;
        assert_eq!(
            results,
            [SyncBulkResult::NotAttempted, SyncBulkResult::NotAttempted]
        );
        assert!(bulks.lock().unwrap().is_empty());

        let provider = ScriptedBulk {
            hang: true,
            ..Default::default()
        };
        let token = CancellationToken::new();
        let stop = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            stop.cancel();
        });
        let results = tokio::time::timeout(
            Duration::from_secs(10),
            sync_bulk(provider, &items, true, &token),
        )
        .await
        .expect("a Stop ends a running bulk");
        assert!(results.iter().all(
            |r| matches!(r, SyncBulkResult::Failed { message } if message.contains("cancelled"))
        ));
    }

    #[test]
    fn a_provider_that_resumes_keeps_its_single_file_path() {
        let mut caps = TransferCapabilities {
            bulk_upload: Capability::Supported,
            bulk_download: Capability::Supported,
            ..Default::default()
        };
        assert_eq!(bulk_directions(&caps), (true, true));
        caps.resume_upload = Capability::Supported;
        assert_eq!(bulk_directions(&caps), (false, true));
        caps.bulk_download = Capability::Unsupported;
        assert_eq!(bulk_directions(&caps), (false, false));
    }

    #[tokio::test]
    async fn bulk_graph_fault_settles_every_started_file() {
        let mut e = executor();
        e.panic = true;
        let result = run(3, Arc::new(e)).await;
        assert_eq!((result.completed, result.failed, result.total), (0, 3, 3));
    }
}
