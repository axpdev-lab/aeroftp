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
    match outcome {
        BulkTransferOutcome::Completed => BulkSettlement::Done,
        BulkTransferOutcome::NotAttempted(_) => BulkSettlement::SingleFile,
        BulkTransferOutcome::Failed(error) => {
            let mut failure =
                crate::transfer_multipart::transfer_failure_from_provider(&error, Some(local_path));
            if failure.retryable {
                return BulkSettlement::SingleFile;
            }
            failure.retryable = false;
            BulkSettlement::Failed(failure)
        }
        // May already have reached the server: never replayed.
        BulkTransferOutcome::Uncertain(message) => BulkSettlement::Failed(TransferFailure::new(
            TransferFailureKind::RemoteIo,
            message,
            false,
        )),
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
        match crate::restricted_chars::validate_path(provider.provider_type(), &entry.remote_path) {
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
                    crate::preserve_remote_mtime(&entry.local_path, entry.modified.as_deref());
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
mod tests {
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
        // 293 command-line bytes each: all 256 fit 96 KiB; Windows keeps
        // its 24 KiB line (80 per group). v4.2.4 made groups of 75.
        let expected = if cfg!(windows) { 80 } else { 256 };
        assert_eq!(groups[0].len(), expected);
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
    #[tokio::test]
    async fn bulk_graph_fault_settles_every_started_file() {
        let mut e = executor();
        e.panic = true;
        let result = run(3, Arc::new(e)).await;
        assert_eq!((result.completed, result.failed, result.total), (0, 3, 3));
    }
}
