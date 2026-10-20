// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Bounded native bulk planning and the provider-executor boundary. No retry
//! wraps this operation: partial/unknown completion must not replay revisions.

use std::ops::Range;
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
    let local = Path::new(&entry.local_path);
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

/// Preserve caller ordering and conflict semantics. Adjacent compatible files
/// share one bounded native invocation. Duplicate portable basenames split a
/// group, even if the filesystem happens to be case-sensitive on this host.
pub(crate) fn plan_groups<E: TransferExecutor>(
    entries: &[TransferEntry],
    executor: &E,
) -> Vec<Range<usize>> {
    plan_groups_by_key(entries, |entry| executor.bulk_group_key(entry))
}

pub(crate) fn plan_groups_by_key(
    entries: &[TransferEntry],
    key_for: impl Fn(&TransferEntry) -> Option<String>,
) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < entries.len() {
        let first = &entries[start];
        let key = key_for(first);
        let mut end = start + 1;
        let mut bytes = first.size;
        let mut path_bytes = first
            .local_path
            .len()
            .saturating_add(first.remote_path.len());
        let mut names = std::collections::HashSet::new();
        names.insert(
            first
                .remote_path
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_lowercase(),
        );
        if key.is_some() && bytes <= MAX_BULK_BYTES && path_bytes <= MAX_BULK_PATH_BYTES {
            while end < entries.len() && end - start < MAX_BULK_FILES {
                let entry = &entries[end];
                let next_bytes = bytes.saturating_add(entry.size);
                let next_paths = path_bytes
                    .saturating_add(entry.local_path.len())
                    .saturating_add(entry.remote_path.len());
                if key_for(entry) != key
                    || next_bytes > MAX_BULK_BYTES
                    || next_paths > MAX_BULK_PATH_BYTES
                    || !names.insert(
                        entry
                            .remote_path
                            .rsplit('/')
                            .next()
                            .unwrap_or("")
                            .to_lowercase(),
                    )
                {
                    break;
                }
                bytes = next_bytes;
                path_bytes = next_paths;
                end += 1;
            }
        }
        ranges.push(start..end);
        start = end;
    }
    ranges
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
    // Reject any invalid path before a native process can modify any item.
    let invalid = entries.iter().find_map(|entry| {
        crate::restricted_chars::validate_path(provider.provider_type(), &entry.remote_path).err()
    });
    if let Some(error) = invalid {
        return failed_bulk(
            entries,
            sink.as_ref(),
            upload,
            TransferFailure::new(TransferFailureKind::InvalidPath, error.to_string(), false),
        );
    }
    let items: Vec<_> = entries
        .iter()
        .map(|e| BulkTransferItem {
            local_path: e.local_path.clone(),
            remote_path: e.remote_path.clone(),
            size: e.size,
        })
        .collect();
    let progress_entries = entries.to_vec();
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
    let timeout = bulk_timeout(timeout_seconds, entries.len(), bytes);
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
    let results = match raw {
        None => {
            return entries
                .iter()
                .map(|entry| {
                    let failure = cancelled_failure();
                    emit_event(sink.as_ref(), entry, upload, "file_error", Some(&failure));
                    TransferOutcome::Failed(failure)
                })
                .collect()
        }
        Some(Ok(Ok(results))) if results.len() == entries.len() => results,
        Some(Ok(Err(error))) => entries
            .iter()
            .map(|_| {
                BulkTransferOutcome::Failed(crate::providers::ProviderError::TransferFailed(
                    error.to_string(),
                ))
            })
            .collect(),
        _ => uncertain_items(
            entries.len(),
            "Bulk did not return complete outcomes; inspect the destination before retrying",
        ),
    };
    entries
        .iter()
        .zip(results)
        .map(|(entry, result)| {
            match result {
                BulkTransferOutcome::Completed => {
                    if !upload {
                        crate::preserve_remote_mtime(&entry.local_path, entry.modified.as_deref());
                    }
                    emit_event(sink.as_ref(), entry, upload, "file_complete", None);
                    TransferOutcome::Success
                }
                other => {
                    let mut failure = match other {
                        BulkTransferOutcome::Failed(error) => {
                            crate::transfer_multipart::transfer_failure_from_provider(
                                &error,
                                Some(&entry.local_path),
                            )
                        }
                        BulkTransferOutcome::Uncertain(message) => {
                            TransferFailure::new(TransferFailureKind::RemoteIo, message, false)
                        }
                        BulkTransferOutcome::Completed => unreachable!(),
                    };
                    // No automatic replay, even for a known failed item, until an
                    // adapter supplies a separate file-only retry contract.
                    failure.retryable = false;
                    emit_event(sink.as_ref(), entry, upload, "file_error", Some(&failure));
                    TransferOutcome::Failed(failure)
                }
            }
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
        assert_eq!(plan_groups(&entries(300), &e), vec![0..256, 256..300]);
        let mut files = entries(3);
        files[1].size = MAX_BULK_BYTES;
        assert_eq!(plan_groups(&files, &e), vec![0..1, 1..2, 2..3]);
        files = entries(3);
        files[1] = files[0].clone();
        assert_eq!(plan_groups(&files, &e), vec![0..1, 1..3]);
        files = entries(3);
        files[1].local_path = format!("/{}/file-1.txt", "a".repeat(MAX_BULK_PATH_BYTES));
        assert_eq!(plan_groups(&files, &e), vec![0..1, 1..2, 2..3]);
        files = entries(3);
        files[1].remote_path = "/other/file-1.txt".into();
        assert_eq!(plan_groups(&files, &e), vec![0..1, 1..2, 2..3]);
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
