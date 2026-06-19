// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! DAG-ENGINE phase 2: the multi-file batch transfer path through `execute_dag`.
//!
//! [`crate::transfer_orchestrator::execute_batch`] is the single chokepoint
//! for every multi-file transfer in the app: GUI drag&drop, GUI file-list
//! batch operations, the FTP batch path in `lib.rs`, the CLI file-level batch,
//! and cross-profile transfers. This module is the flag-gated bridge that
//! routes that one chokepoint through the shared graph engine instead of the
//! hand-rolled `JoinSet` sliding window.
//!
//! ## Scope
//!
//! [`execute_batch_dag`] builds one [`BatchDag`](crate::transfer_dag::BatchDag)
//! from the batch entries (one seven-node single-file sub-DAG per file) and
//! runs it through [`execute_dag`]. The graph's single
//! [`TransferResourceManager`] is the only file-level concurrency governor, so
//! `max_concurrent` flows in through `TransferBatchConfig::transfer_budget()`
//! exactly as the legacy path consumes it (F2-T03).
//!
//! The real per-file I/O is unchanged: every transfer node calls the same
//! [`TransferExecutor::execute_with_session`] the legacy path called, holding
//! the same per-file session lease from the same `executor.session_pool(..)`.
//! Only the *scheduling* moves onto the graph; the bytes on the wire and the
//! emitted `transfer_batch_*` events are byte-identical.
//!
//! ## Byte-identical contract
//!
//! - `emit_batch_started` / `emit_batch_progress` (one per transferred file) /
//!   `emit_batch_completed` are emitted with the same payloads as the legacy
//!   orchestrator.
//! - A failed file does NOT abort the batch. The legacy executor records the
//!   failure in the progress snapshot and proceeds to the next entry, so each
//!   transfer node always reports [`NodeOutcome::Completed`] to the graph
//!   executor; the failure lives only in the [`BatchProgressSnapshot`].
//! - Cancellation leaves the remaining files untouched and unaccounted,
//!   mirroring the legacy task that returns early after `cancel` is observed.
//!
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;

use crate::transfer_dag::executor::{execute_dag, DagNodeRunner, NodeFuture, NodeOutcome};
use crate::transfer_dag::graph::{TransferNode, TransferNodeKind};
use crate::transfer_dag::{
    AimdConfig, AimdController, BatchDagItem, DagObserver, NoopDagObserver, TransferCapabilities,
    TransferDagBuilder, TransferDirection, TransferResourceManager,
};
use crate::transfer_domain::{
    BatchProgressSnapshot, TransferBatchResult, TransferDirection as DomainTransferDirection,
    TransferOutcome,
};
use crate::transfer_event_sink::TransferEventSink;
use crate::transfer_orchestrator::{ProgressObserver, TransferBatch, TransferExecutor};

/// Run a multi-file batch transfer through the graph engine.
///
/// A drop-in replacement for [`crate::transfer_orchestrator::execute_batch`]:
/// same arguments, same [`TransferBatchResult`]. Every batch transfer
/// routes through this function; the legacy `JoinSet` sliding window
/// orchestrator is gone (DAG-ENGINE A-branch SG-T18 closure).
pub async fn execute_batch_dag<E>(
    sink: Arc<dyn TransferEventSink>,
    batch: TransferBatch,
    executor: Arc<E>,
    cancel: Arc<AtomicBool>,
    progress_observer: Option<ProgressObserver>,
) -> TransferBatchResult
where
    E: TransferExecutor + Send + Sync + 'static,
{
    let started_at = Instant::now();
    let TransferBatch {
        id,
        display_name,
        direction,
        config,
        entries,
    } = batch;
    let total = entries.len() as u32;
    let dag_direction = match direction {
        DomainTransferDirection::Download => TransferDirection::Download,
        DomainTransferDirection::Upload => TransferDirection::Upload,
    };

    sink.emit_batch_started(serde_json::json!({
        "batch_id": id.clone(),
        "display_name": display_name,
        "direction": direction,
        "total": total,
    }));

    let progress = Arc::new(Mutex::new(BatchProgressSnapshot {
        batch_id: id,
        total,
        bytes_total: entries.iter().map(|entry| entry.size).sum(),
        ..BatchProgressSnapshot::default()
    }));

    // One capability-shaped sub-DAG per entry, merged into one graph.
    // Every batch is bound to a single provider session, so the caps set is
    // shared across files. The shared executor does not yet expose a
    // capability snapshot to the batch surface (that is the SG-T14 wire-in
    // gate for native multipart batch uploads); until it does, default caps
    // produce a graph shape identical to the legacy single-transfer-core
    // chain, so this is byte-identical with the previous wiring.
    let caps = TransferCapabilities::default();
    let items: Vec<BatchDagItem> = entries
        .iter()
        .map(|entry| BatchDagItem::with_size(entry.id.clone(), dag_direction, entry.size))
        .collect();
    let built = TransferDagBuilder::from_batch_shaped(&items, &caps);

    // The builder preserves entry order, so `built.files[i]` is `entries[i]`.
    // Map every transfer-core node id back to its entry index. With the
    // shaped batch builder a file may carry more than one transfer node
    // (multipart fan-out), so we iterate `transfer_nodes` rather than
    // binding the single `transfer` id.
    let mut node_to_entry: HashMap<usize, usize> = HashMap::with_capacity(built.files.len());
    for (index, file) in built.files.iter().enumerate() {
        for &node_id in &file.transfer_nodes {
            node_to_entry.insert(node_id, index);
        }
    }
    // The legacy `execute_with_session` is per-file, not per-part: when the
    // shaped graph hands out N `UploadPart` nodes for one entry, only the
    // first one to enter the runner performs the transfer; the rest become
    // structural no-ops. This preserves the batch contract (one
    // `execute_with_session` per file, one progress snapshot per file) and
    // keeps the multipart fan-out as a future optimisation gate that
    // engages once the executor exposes a native per-part contract.
    let entry_transferred = Arc::new(Mutex::new(vec![false; entries.len()]));

    let entries = Arc::new(entries);
    let node_to_entry = Arc::new(node_to_entry);
    let max_concurrent = config.max_concurrent.max(1) as usize;
    let session_pool = Arc::new(executor.session_pool(max_concurrent));
    let resource_manager = TransferResourceManager::new(config.transfer_budget());
    let provider_type = executor.provider_type();

    // AIMD backpressure (F3-T05). The controller's File ceiling is the batch
    // `file_slots` budget and starts there, so a congestion-free batch
    // dispatches every file exactly as before; when a file fails with a
    // genuine congestion signal the File dispatch target halves, throttling
    // the remaining files instead of hammering an overloaded server.
    let aimd = Arc::new(AimdController::from_budget_for_provider(
        &resource_manager.budget(),
        provider_type,
        AimdConfig::runtime(),
    ));

    let runner: Arc<dyn DagNodeRunner> = {
        let sink = Arc::clone(&sink);
        let executor = Arc::clone(&executor);
        let cancel = Arc::clone(&cancel);
        let progress = Arc::clone(&progress);
        let entries = Arc::clone(&entries);
        let node_to_entry = Arc::clone(&node_to_entry);
        let session_pool = Arc::clone(&session_pool);
        let entry_transferred = Arc::clone(&entry_transferred);
        Arc::new(move |node: TransferNode| -> NodeFuture {
            let sink = Arc::clone(&sink);
            let executor = Arc::clone(&executor);
            let cancel = Arc::clone(&cancel);
            let progress = Arc::clone(&progress);
            let progress_observer = progress_observer.clone();
            let entries = Arc::clone(&entries);
            let node_to_entry = Arc::clone(&node_to_entry);
            let session_pool = Arc::clone(&session_pool);
            let entry_transferred = Arc::clone(&entry_transferred);
            Box::pin(async move {
                // Only the real transfer nodes do I/O; every structural node
                // (discover / acquire / verify / preserve / commit / emit) is
                // a no-op anchor, exactly like the single-file bridge.
                if !matches!(
                    node.kind,
                    TransferNodeKind::DownloadFile
                        | TransferNodeKind::UploadFile
                        | TransferNodeKind::UploadPart
                ) {
                    return NodeOutcome::Completed;
                }
                let Some(&entry_index) = node_to_entry.get(&node.id) else {
                    return NodeOutcome::Completed;
                };
                let entry = entries[entry_index].clone();

                // Multiple UploadPart nodes can map to the same entry under
                // the shaped fan-out shape. The legacy `execute_with_session`
                // is a per-file contract, so only the first part to enter the
                // runner performs the transfer; the others fall through as
                // structural no-ops and let the graph drain.
                {
                    let mut transferred = entry_transferred.lock().await;
                    if transferred[entry_index] {
                        return NodeOutcome::Completed;
                    }
                    transferred[entry_index] = true;
                }

                // Cancellation: a cancelled batch leaves the remaining files
                // untouched and unaccounted, mirroring the legacy task that
                // returns early. The node still completes so the graph drains.
                if cancel.load(Ordering::Relaxed) {
                    return NodeOutcome::Completed;
                }

                let session_lease = match session_pool.acquire().await {
                    Ok(lease) => lease,
                    Err(error) => {
                        tracing::warn!("Transfer session acquisition failed: {}", error);
                        // Issue #234: a transient session-pool acquire failure
                        // must be recorded in the batch snapshot too, or the
                        // GUI "X of Y completed" counters silently miss the
                        // failed entry. Mirror the success/skip/fail branches
                        // below: bump `failed`, emit progress, notify observer.
                        let snapshot_clone = {
                            let mut snapshot = progress.lock().await;
                            snapshot.failed += 1;
                            snapshot.clone()
                        };
                        sink.emit_batch_progress(&snapshot_clone);
                        if let Some(observer) = progress_observer {
                            observer(snapshot_clone);
                        }
                        return NodeOutcome::Completed;
                    }
                };

                if cancel.load(Ordering::Relaxed) {
                    return NodeOutcome::Completed;
                }

                {
                    let mut snapshot = progress.lock().await;
                    snapshot.active += 1;
                }

                let outcome = executor
                    .execute_with_session(entry.clone(), session_lease)
                    .await;

                let snapshot_clone = {
                    let mut snapshot = progress.lock().await;
                    snapshot.active = snapshot.active.saturating_sub(1);
                    match &outcome {
                        TransferOutcome::Success => {
                            snapshot.completed += 1;
                            snapshot.bytes_transferred += entry.size;
                        }
                        TransferOutcome::Skipped { .. } => {
                            snapshot.skipped += 1;
                        }
                        TransferOutcome::Failed(_) => {
                            snapshot.failed += 1;
                        }
                    }
                    snapshot.clone()
                };

                sink.emit_batch_progress(&snapshot_clone);
                if let Some(observer) = progress_observer {
                    observer(snapshot_clone);
                }

                // A failed file must NOT abort the batch: the legacy executor
                // records the failure and proceeds to the next entry. The node
                // therefore always reports Completed; the failure lives only in
                // the progress snapshot.
                NodeOutcome::Completed
            })
        })
    };

    let observer: Arc<dyn DagObserver> = Arc::new(NoopDagObserver);
    if let Err(error) =
        execute_dag(&built.dag, &resource_manager, runner, observer, Some(aimd)).await
    {
        // The runner never returns Failed, so a graph error here means an
        // executor-level fault (panic, semaphore closed). Surface it and still
        // build the honest result from whatever the snapshot recorded.
        tracing::warn!("Batch transfer graph stopped early: {}", error);
    }

    let snapshot = progress.lock().await.clone();
    let batch_result = TransferBatchResult {
        completed: snapshot.completed,
        skipped: snapshot.skipped,
        failed: snapshot.failed,
        total: snapshot.total,
        cancelled: cancel.load(Ordering::Relaxed),
        duration_ms: started_at.elapsed().as_millis() as u64,
    };

    sink.emit_batch_completed(&batch_result);

    batch_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_dag::TransferSessionPoolHandle;
    use crate::transfer_domain::{
        TransferBatchConfig, TransferEntry, TransferFailure, TransferFailureKind,
    };
    use crate::TransferEvent;
    use async_trait::async_trait;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    /// A `TransferExecutor` whose outcome per entry is scripted, with peak
    /// concurrency tracking and a configurable session-pool capacity.
    struct MockExecutor {
        fail: HashSet<String>,
        skip: HashSet<String>,
        executed: StdMutex<Vec<String>>,
        in_flight: AtomicUsize,
        peak: AtomicUsize,
        session_capacity: usize,
    }

    impl MockExecutor {
        fn new(session_capacity: usize) -> Self {
            Self {
                fail: HashSet::new(),
                skip: HashSet::new(),
                executed: StdMutex::new(Vec::new()),
                in_flight: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                session_capacity,
            }
        }

        fn executed(&self) -> Vec<String> {
            self.executed.lock().unwrap().clone()
        }

        fn peak(&self) -> usize {
            self.peak.load(AtomicOrdering::SeqCst)
        }
    }

    #[async_trait]
    impl TransferExecutor for MockExecutor {
        async fn execute(&self, entry: TransferEntry) -> TransferOutcome {
            let now = self.in_flight.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            self.peak.fetch_max(now, AtomicOrdering::SeqCst);
            tokio::time::sleep(Duration::from_millis(15)).await;
            self.in_flight.fetch_sub(1, AtomicOrdering::SeqCst);
            self.executed.lock().unwrap().push(entry.id.clone());

            if self.fail.contains(&entry.id) {
                TransferOutcome::Failed(TransferFailure {
                    kind: TransferFailureKind::Unknown,
                    message: "mock failure".to_string(),
                    retryable: false,
                })
            } else if self.skip.contains(&entry.id) {
                TransferOutcome::Skipped {
                    reason: "mock skip".to_string(),
                }
            } else {
                TransferOutcome::Success
            }
        }

        fn session_pool(&self, max_concurrent: usize) -> TransferSessionPoolHandle {
            TransferSessionPoolHandle::http_clone(
                "mock",
                self.session_capacity.min(max_concurrent).max(1),
            )
        }
    }

    /// A sink that counts the three batch-lifecycle events.
    #[derive(Default)]
    struct CountingSink {
        started: AtomicUsize,
        progress: AtomicUsize,
        completed: AtomicUsize,
    }

    impl TransferEventSink for CountingSink {
        fn emit_transfer_event(&self, _event: TransferEvent) {}
        fn emit_batch_started(&self, _payload: serde_json::Value) {
            self.started.fetch_add(1, AtomicOrdering::SeqCst);
        }
        fn emit_batch_progress(&self, _snapshot: &BatchProgressSnapshot) {
            self.progress.fetch_add(1, AtomicOrdering::SeqCst);
        }
        fn emit_batch_completed(&self, _result: &TransferBatchResult) {
            self.completed.fetch_add(1, AtomicOrdering::SeqCst);
        }
    }

    fn entry(id: &str, size: u64) -> TransferEntry {
        TransferEntry {
            id: id.to_string(),
            display_name: id.to_string(),
            remote_path: format!("/remote/{id}"),
            local_path: format!("/local/{id}"),
            size,
            modified: None,
        }
    }

    fn batch(entries: Vec<TransferEntry>, max_concurrent: u32) -> TransferBatch {
        TransferBatch {
            id: "batch-1".to_string(),
            display_name: "Test batch".to_string(),
            direction: DomainTransferDirection::Download,
            config: TransferBatchConfig {
                max_concurrent,
                max_retries: 0,
                timeout_ms: 30_000,
            },
            entries,
        }
    }

    #[tokio::test]
    async fn batch_dag_executes_every_entry_and_counts_outcomes() {
        let mut executor = MockExecutor::new(1);
        executor.fail.insert("c".to_string());
        executor.skip.insert("b".to_string());
        let executor = Arc::new(executor);
        let sink = Arc::new(CountingSink::default());

        let result = execute_batch_dag(
            Arc::clone(&sink) as Arc<dyn TransferEventSink>,
            batch(vec![entry("a", 100), entry("b", 200), entry("c", 300)], 1),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;

        assert_eq!(result.total, 3);
        assert_eq!(result.completed, 1);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.failed, 1);
        assert!(!result.cancelled);
        assert_eq!(executor.executed().len(), 3);
        // Lifecycle events: one start, one completed, one progress per file.
        assert_eq!(sink.started.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(sink.completed.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(sink.progress.load(AtomicOrdering::SeqCst), 3);
    }

    #[tokio::test]
    async fn batch_dag_failed_file_does_not_abort_remaining() {
        let mut executor = MockExecutor::new(1);
        // The first entry fails; the remaining two must still transfer.
        executor.fail.insert("first".to_string());
        let executor = Arc::new(executor);

        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            batch(
                vec![entry("first", 10), entry("second", 20), entry("third", 30)],
                1,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;

        assert_eq!(result.failed, 1);
        assert_eq!(result.completed, 2);
        let executed = executor.executed();
        assert_eq!(executed.len(), 3, "a failed file must not abort the batch");
        assert!(executed.contains(&"second".to_string()));
        assert!(executed.contains(&"third".to_string()));
    }

    #[tokio::test]
    async fn batch_dag_respects_file_slot_concurrency() {
        // file_slots = 2 (from max_concurrent), session pool wide enough: the
        // graph's resource manager is the binding limit, peak must be 2.
        let executor = Arc::new(MockExecutor::new(8));
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            batch(
                vec![entry("a", 1), entry("b", 1), entry("c", 1), entry("d", 1)],
                2,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;

        assert_eq!(result.completed, 4);
        assert_eq!(executor.peak(), 2, "file_slots=2 must cap concurrency at 2");
    }

    #[tokio::test]
    async fn batch_dag_serializes_when_one_file_slot() {
        let executor = Arc::new(MockExecutor::new(8));
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            batch(vec![entry("a", 1), entry("b", 1), entry("c", 1)], 1),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;

        assert_eq!(result.completed, 3);
        assert_eq!(executor.peak(), 1, "file_slots=1 must force serialization");
    }

    #[tokio::test]
    async fn batch_dag_cancellation_skips_transfers() {
        let executor = Arc::new(MockExecutor::new(1));
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            batch(vec![entry("a", 1), entry("b", 1)], 1),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(true)),
            None,
        )
        .await;

        assert!(result.cancelled);
        assert_eq!(result.completed, 0);
        assert_eq!(result.failed, 0);
        assert!(
            executor.executed().is_empty(),
            "a pre-cancelled batch transfers nothing"
        );
    }

    #[tokio::test]
    async fn batch_dag_empty_batch_emits_lifecycle_and_zero_result() {
        let executor = Arc::new(MockExecutor::new(1));
        let sink = Arc::new(CountingSink::default());
        let result = execute_batch_dag(
            Arc::clone(&sink) as Arc<dyn TransferEventSink>,
            batch(vec![], 1),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;

        assert_eq!(result.total, 0);
        assert_eq!(result.completed, 0);
        assert!(executor.executed().is_empty());
        assert_eq!(sink.started.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(sink.completed.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(sink.progress.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    async fn batch_dag_progress_observer_fires_once_per_file() {
        let executor = Arc::new(MockExecutor::new(1));
        let observed = Arc::new(AtomicUsize::new(0));
        let progress_observer: ProgressObserver = {
            let observed = Arc::clone(&observed);
            Arc::new(move |_snapshot| {
                observed.fetch_add(1, AtomicOrdering::SeqCst);
            })
        };

        execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            batch(vec![entry("a", 1), entry("b", 1)], 1),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            Some(progress_observer),
        )
        .await;

        assert_eq!(observed.load(AtomicOrdering::SeqCst), 2);
    }

    // ============ Issue #234: session_pool.acquire() failure accounting ============

    use crate::transfer_dag::session_pool::{
        SessionLeaseKind, SessionPoolCapacity, SessionPoolError, TransferSessionLease,
        TransferSessionPool,
    };

    /// A `TransferSessionPool` whose `acquire()` always fails with a
    /// `Closed` error. Stand-in for the production failure mode where a
    /// semaphore is closed mid-batch.
    struct AlwaysFailingPool;

    #[async_trait]
    impl TransferSessionPool for AlwaysFailingPool {
        async fn acquire(&self) -> Result<TransferSessionLease, SessionPoolError> {
            Err(SessionPoolError::Closed {
                kind: SessionLeaseKind::HttpClone,
                label: "always-failing".to_string(),
            })
        }
        fn label(&self) -> &str {
            "always-failing"
        }
        fn kind(&self) -> SessionLeaseKind {
            SessionLeaseKind::HttpClone
        }
        fn capacity(&self) -> SessionPoolCapacity {
            SessionPoolCapacity {
                kind: SessionLeaseKind::HttpClone,
                label: "always-failing".to_string(),
                max_leases: 1,
            }
        }
    }

    /// MockExecutor variant that hands out a pool whose acquire() always
    /// fails. The transfer node never gets to call `execute_with_session`.
    struct FailingPoolExecutor;

    #[async_trait]
    impl TransferExecutor for FailingPoolExecutor {
        async fn execute(&self, _entry: TransferEntry) -> TransferOutcome {
            unreachable!("execute() is unreachable when session_pool.acquire() always fails")
        }
        fn session_pool(&self, _max_concurrent: usize) -> TransferSessionPoolHandle {
            TransferSessionPoolHandle::new(AlwaysFailingPool)
        }
    }

    #[tokio::test]
    async fn batch_dag_session_pool_acquire_failure_is_counted_in_snapshot() {
        // Issue #234: when session_pool.acquire() returns Err for every
        // entry, the batch result must report each entry as `failed` (not
        // silently zero), and the sink must see one progress emission per
        // entry to keep "X of Y completed" honest.
        let executor = Arc::new(FailingPoolExecutor);
        let sink = Arc::new(CountingSink::default());
        let observed = Arc::new(AtomicUsize::new(0));
        let progress_observer: ProgressObserver = {
            let observed = Arc::clone(&observed);
            Arc::new(move |_snapshot| {
                observed.fetch_add(1, AtomicOrdering::SeqCst);
            })
        };

        let result = execute_batch_dag(
            Arc::clone(&sink) as Arc<dyn TransferEventSink>,
            batch(vec![entry("a", 1), entry("b", 1), entry("c", 1)], 2),
            executor,
            Arc::new(AtomicBool::new(false)),
            Some(progress_observer),
        )
        .await;

        assert_eq!(result.total, 3);
        assert_eq!(
            result.failed, 3,
            "each acquire failure must be recorded as a failed entry"
        );
        assert_eq!(result.completed, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(
            sink.progress.load(AtomicOrdering::SeqCst),
            3,
            "every entry must emit a progress event, even on acquire failure"
        );
        assert_eq!(
            observed.load(AtomicOrdering::SeqCst),
            3,
            "progress observer must be invoked on acquire failure too"
        );
    }
}
