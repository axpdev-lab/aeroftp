// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! DAG-ENGINE phase 2: the multi-file batch transfer path through `execute_dag`.
//!
//! [`crate::transfer_orchestrator::execute_batch`] is the single chokepoint
//! for every multi-file transfer in the app: GUI drag&drop, GUI file-list
//! batch operations, the FTP batch path in `lib.rs`, the CLI file-level batch,
//! and cross-profile transfers. This module is the bridge that routes that
//! chokepoint through the shared graph engine.
//!
//! ## Scope
//!
//! [`execute_batch_dag`] builds one [`BatchDag`](crate::transfer_dag::BatchDag)
//! from the batch entries (one seven-node single-file sub-DAG per file) and
//! runs it through [`execute_dag`]. The graph's single
//! [`TransferResourceManager`](crate::transfer_dag::TransferResourceManager) is
//! the only node-level concurrency governor; DAG-P2-01 makes its buffer-byte
//! pool the process-global governor's while slot classes stay per-job.
//!
//! ## DAG-P1-03 — real per-part multipart wire I/O
//!
//! When the executor advertises multipart and implements
//! [`TransferExecutor::supports_multipart_wire`], shaped `UploadPart` nodes
//! each perform one provider `upload_part` call over an exact local byte
//! range. One multipart session is begun lazily per file; `CommitTemp`
//! completes once (or aborts once after in-flight parts drain). File/session
//! ownership is a single session-pool lease per multipart file — not one
//! lease per part. Plain `UploadFile` / `DownloadFile` stay on the whole-file
//! `execute_with_session` contract.
//!
//! ## Payload-compatible contract
//!
//! - `emit_batch_started` / `emit_batch_progress` / `emit_batch_completed`
//!   are produced at the same lifecycle points with the same payload shapes.
//! - A failed file does NOT abort the batch (DAG-P1-04): whole-file transfer
//!   nodes and multipart `CommitTemp` report
//!   [`NodeOutcome::FileFailedButGraphContinues`] after exactly-once snapshot
//!   accounting, so DAG/AIMD see a typed file-local failure while independent
//!   files continue. Part nodes still return [`NodeOutcome::Completed`] so
//!   siblings drain; the single file-class AIMD signal is at commit.
//! - Cancellation leaves unstarted files untouched and unaccounted, and is
//!   never treated as congestion.
//! - Preflight unsupported-multipart failures are accounted once before graph
//!   execution (not congestion); part/commit nodes for those entries no-op.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;

use crate::transfer_dag::executor::{execute_dag, DagNodeRunner, NodeFuture, NodeOutcome};
use crate::transfer_dag::graph::{TransferNode, TransferNodeKind};
use crate::transfer_dag::{
    AimdConfig, AimdController, BatchDagItem, DagObserver, DiskLeaseRequest, NoopDagObserver,
    TransferCapabilities, TransferDagBuilder, TransferDirection, TransferGraphProfile,
    TransferPriority, TransferSessionLease,
};
use crate::transfer_domain::{
    BatchProgressSnapshot, TransferBatchResult, TransferDirection as DomainTransferDirection,
    TransferFailure, TransferFailureKind, TransferOutcome,
};
use crate::transfer_event_sink::TransferEventSink;
use crate::transfer_multipart::{
    cancelled_failure, read_chunk, unsupported_multipart_failure, MultipartFileState,
    MultipartLayout,
};
use crate::transfer_orchestrator::{ProgressObserver, TransferBatch, TransferExecutor};

/// Per-multipart-file runtime: shared driver state + one session lease.
struct MultipartFileRuntime {
    state: Arc<MultipartFileState>,
    /// One file/session lease for the whole multipart lifecycle.
    /// `None` = not yet acquired; acquire is serialised by this mutex.
    session_lease: Mutex<SessionLeaseSlot>,
    /// Process-global endpoint plus local-device lease for the multipart file.
    /// Held from the first part through terminal commit/abort so individual
    /// part nodes cannot bypass the provider-job governor.
    governor_lease: Mutex<Option<crate::transfer_dag::GovernorJobLease>>,
}

enum SessionLeaseSlot {
    /// No acquire attempted yet.
    Vacant,
    /// Lease held for the multipart file lifecycle.
    Held(#[allow(dead_code)] TransferSessionLease),
    /// Acquire failed; siblings must drain without I/O.
    Failed,
}

/// Run a multi-file batch transfer through the graph engine.
///
/// A drop-in replacement for [`crate::transfer_orchestrator::execute_batch`]:
/// same arguments, same [`TransferBatchResult`].
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
    execute_batch_dag_inner(sink, batch, executor, cancel, progress_observer, None).await
}

/// Test-only entry that injects a pre-built AIMD controller so probes can
/// assert target decrease without live network or process-global state.
#[cfg(test)]
async fn execute_batch_dag_with_aimd<E>(
    sink: Arc<dyn TransferEventSink>,
    batch: TransferBatch,
    executor: Arc<E>,
    cancel: Arc<AtomicBool>,
    progress_observer: Option<ProgressObserver>,
    aimd: Arc<AimdController>,
) -> TransferBatchResult
where
    E: TransferExecutor + Send + Sync + 'static,
{
    execute_batch_dag_inner(sink, batch, executor, cancel, progress_observer, Some(aimd)).await
}

async fn execute_batch_dag_inner<E>(
    sink: Arc<dyn TransferEventSink>,
    batch: TransferBatch,
    executor: Arc<E>,
    cancel: Arc<AtomicBool>,
    progress_observer: Option<ProgressObserver>,
    aimd_override: Option<Arc<AimdController>>,
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

    // DAG-P1-01/P1-02: consume exactly the executor's runtime capability
    // snapshot for graph shape and budget ceilings.
    let caps: TransferCapabilities = executor.transfer_capabilities();
    let items: Vec<BatchDagItem> = entries
        .iter()
        .map(|entry| BatchDagItem::with_size(entry.id.clone(), dag_direction, entry.size))
        .collect();
    let built = TransferDagBuilder::from_batch_shaped(&items, &caps);

    // Map transfer-core nodes AND multipart CommitTemp nodes to entry index.
    let mut node_to_entry: HashMap<usize, usize> = HashMap::with_capacity(built.files.len() * 2);
    let mut multipart_runtimes: Vec<Option<Arc<MultipartFileRuntime>>> =
        Vec::with_capacity(built.files.len());

    for (index, file) in built.files.iter().enumerate() {
        for &node_id in &file.transfer_nodes {
            node_to_entry.insert(node_id, index);
        }

        let is_multipart = file.transfer_nodes.len() > 1;
        if is_multipart {
            // Fail closed when the graph is multipart-shaped but the executor
            // has no wire contract (conservative default / capability mismatch).
            if !executor.supports_multipart_wire() {
                multipart_runtimes.push(None);
                // Still map commit so we can account a single failure.
                node_to_entry.insert(file.commit, index);
                continue;
            }
            let entry = &entries[index];
            let profile = TransferGraphProfile::resolve(dag_direction, &caps, entry.size);
            let layout = MultipartLayout::from_profile(
                entry.size,
                profile.upload_parts,
                profile.preferred_chunk_size,
                &entry.local_path,
            );
            let node_to_part: HashMap<usize, u32> = file
                .transfer_nodes
                .iter()
                .enumerate()
                .map(|(idx, node_id)| (*node_id, (idx + 1) as u32))
                .collect();
            let state = MultipartFileState::new(layout, node_to_part);
            node_to_entry.insert(file.commit, index);
            multipart_runtimes.push(Some(Arc::new(MultipartFileRuntime {
                state,
                session_lease: Mutex::new(SessionLeaseSlot::Vacant),
                governor_lease: Mutex::new(None),
            })));
        } else {
            multipart_runtimes.push(None);
        }
    }

    // Preflight: multipart-shaped files without wire support fail closed once.
    let preflight_unsupported: Vec<usize> = built
        .files
        .iter()
        .enumerate()
        .filter(|(i, f)| f.transfer_nodes.len() > 1 && multipart_runtimes[*i].is_none())
        .map(|(i, _)| i)
        .collect();
    if !preflight_unsupported.is_empty() && !executor.supports_multipart_wire() {
        for &index in &preflight_unsupported {
            let entry = &entries[index];
            let failure = unsupported_multipart_failure();
            executor.multipart_emit_file_error(entry, &failure);
            let snapshot_clone = {
                let mut snapshot = progress.lock().await;
                snapshot.failed += 1;
                snapshot.clone()
            };
            sink.emit_batch_progress(&snapshot_clone);
            if let Some(observer) = progress_observer.as_ref() {
                observer(snapshot_clone);
            }
        }
        // Do not schedule multipart wire work for unsupported files; strip by
        // leaving runtime None and making part/commit nodes no-op after
        // accounting above. Parts/commit for those indices short-circuit.
    }

    let entries = Arc::new(entries);
    let node_to_entry = Arc::new(node_to_entry);
    let multipart_runtimes = Arc::new(multipart_runtimes);
    // Track preflight-accounted unsupported entries so commit/part skip re-account.
    let preflight_failed = Arc::new(std::sync::Mutex::new(
        preflight_unsupported
            .into_iter()
            .collect::<std::collections::HashSet<_>>(),
    ));

    let max_concurrent = config.max_concurrent.max(1) as usize;
    let session_pool = Arc::new(executor.session_pool(max_concurrent));
    // DAG-P1-03: expose resolved chunk/disk-read credits from capabilities.
    // DAG-P2-01: borrow the buffer-byte pool from the process-global governor so
    // concurrent batch jobs share one memory cap; slot classes remain per-job.
    let resource_manager = crate::transfer_dag::governor::global()
        .child_manager(config.transfer_budget_for_capabilities(&caps));
    let provider_type = executor.provider_type();

    let aimd = aimd_override.unwrap_or_else(|| {
        Arc::new(AimdController::from_budget_for_provider(
            &resource_manager.budget(),
            provider_type,
            AimdConfig::runtime(),
        ))
    });

    let runner: Arc<dyn DagNodeRunner> = {
        let sink = Arc::clone(&sink);
        let executor = Arc::clone(&executor);
        let cancel = Arc::clone(&cancel);
        let progress = Arc::clone(&progress);
        let entries = Arc::clone(&entries);
        let node_to_entry = Arc::clone(&node_to_entry);
        let session_pool = Arc::clone(&session_pool);
        let multipart_runtimes = Arc::clone(&multipart_runtimes);
        let preflight_failed = Arc::clone(&preflight_failed);
        Arc::new(move |node: TransferNode| -> NodeFuture {
            let sink = Arc::clone(&sink);
            let executor = Arc::clone(&executor);
            let cancel = Arc::clone(&cancel);
            let progress = Arc::clone(&progress);
            let progress_observer = progress_observer.clone();
            let entries = Arc::clone(&entries);
            let node_to_entry = Arc::clone(&node_to_entry);
            let session_pool = Arc::clone(&session_pool);
            let multipart_runtimes = Arc::clone(&multipart_runtimes);
            let preflight_failed = Arc::clone(&preflight_failed);
            Box::pin(async move {
                match node.kind {
                    TransferNodeKind::DownloadFile | TransferNodeKind::UploadFile => {
                        let direction = match node.kind {
                            TransferNodeKind::DownloadFile => TransferDirection::Download,
                            TransferNodeKind::UploadFile => TransferDirection::Upload,
                            _ => unreachable!("whole-file branch has one of two kinds"),
                        };
                        run_whole_file_node(
                            node.id,
                            direction,
                            &node_to_entry,
                            &entries,
                            &executor,
                            &cancel,
                            &session_pool,
                            &progress,
                            &sink,
                            progress_observer.as_ref(),
                        )
                        .await
                    }
                    TransferNodeKind::UploadPart => {
                        run_multipart_part_node(
                            node.id,
                            &node_to_entry,
                            &entries,
                            &multipart_runtimes,
                            &preflight_failed,
                            &executor,
                            &cancel,
                            &session_pool,
                            &progress,
                            &sink,
                            progress_observer.as_ref(),
                        )
                        .await
                    }
                    TransferNodeKind::CommitTemp => {
                        run_multipart_commit_node(
                            node.id,
                            &node_to_entry,
                            &entries,
                            &multipart_runtimes,
                            &preflight_failed,
                            &executor,
                            &cancel,
                            &progress,
                            &sink,
                            progress_observer.as_ref(),
                        )
                        .await
                    }
                    // Structural anchors: discover / acquire / verify / preserve / emit.
                    _ => NodeOutcome::Completed,
                }
            })
        })
    };

    let observer: Arc<dyn DagObserver> = Arc::new(NoopDagObserver);
    if let Err(error) =
        execute_dag(&built.dag, &resource_manager, runner, observer, Some(aimd)).await
    {
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

#[allow(clippy::too_many_arguments)]
async fn run_whole_file_node<E>(
    node_id: usize,
    direction: TransferDirection,
    node_to_entry: &HashMap<usize, usize>,
    entries: &[crate::transfer_domain::TransferEntry],
    executor: &Arc<E>,
    cancel: &AtomicBool,
    session_pool: &crate::transfer_dag::TransferSessionPoolHandle,
    progress: &Mutex<BatchProgressSnapshot>,
    sink: &Arc<dyn TransferEventSink>,
    progress_observer: Option<&ProgressObserver>,
) -> NodeOutcome
where
    E: TransferExecutor + Send + Sync + 'static,
{
    let Some(&entry_index) = node_to_entry.get(&node_id) else {
        return NodeOutcome::Completed;
    };
    let entry = entries[entry_index].clone();

    if cancel.load(Ordering::Relaxed) {
        return NodeOutcome::Completed;
    }

    let session_lease = match session_pool.acquire().await {
        Ok(lease) => lease,
        Err(error) => {
            tracing::warn!("Transfer session acquisition failed: {}", error);
            // Session-pool acquire failure is visible and non-fatal, but never
            // congestion (Unknown kind → no AIMD decrease).
            let failure = TransferFailure::new(
                TransferFailureKind::Unknown,
                format!("Transfer session acquisition failed: {error}"),
                true,
            );
            account_outcome(
                progress,
                sink,
                progress_observer,
                TransferOutcome::Failed(failure.clone()),
                0,
                true,
            )
            .await;
            return node_outcome_for_file_result(&TransferOutcome::Failed(failure));
        }
    };

    if cancel.load(Ordering::Relaxed) {
        return NodeOutcome::Completed;
    }

    let endpoint = executor.endpoint_identity().await;
    let disk_request = match direction {
        TransferDirection::Upload => DiskLeaseRequest::read(entry.local_path.clone()),
        TransferDirection::Download => DiskLeaseRequest::write(entry.local_path.clone()),
    };
    // Batches are background work. The endpoint and local-device permits are
    // held only while this file's session is active, preserving file-level
    // parallelism across different endpoints and disks.
    let _job_lease = crate::transfer_dag::governor::global()
        .acquire_job(endpoint, TransferPriority::Background, [disk_request])
        .await;

    {
        let mut snapshot = progress.lock().await;
        snapshot.active += 1;
    }

    let outcome = executor
        .execute_with_session(entry.clone(), session_lease)
        .await;

    let bytes = match &outcome {
        TransferOutcome::Success => entry.size,
        _ => 0,
    };
    // Exactly-once snapshot/event accounting, then the typed node outcome so
    // DAG/AIMD see the file terminal without aborting the batch graph.
    let node_outcome = node_outcome_for_file_result(&outcome);
    account_outcome(progress, sink, progress_observer, outcome, bytes, true).await;
    node_outcome
}

#[allow(clippy::too_many_arguments)]
async fn run_multipart_part_node<E>(
    node_id: usize,
    node_to_entry: &HashMap<usize, usize>,
    entries: &[crate::transfer_domain::TransferEntry],
    multipart_runtimes: &[Option<Arc<MultipartFileRuntime>>],
    preflight_failed: &std::sync::Mutex<std::collections::HashSet<usize>>,
    executor: &Arc<E>,
    cancel: &AtomicBool,
    session_pool: &crate::transfer_dag::TransferSessionPoolHandle,
    progress: &Mutex<BatchProgressSnapshot>,
    _sink: &Arc<dyn TransferEventSink>,
    _progress_observer: Option<&ProgressObserver>,
) -> NodeOutcome
where
    E: TransferExecutor + Send + Sync + 'static,
{
    let Some(&entry_index) = node_to_entry.get(&node_id) else {
        return NodeOutcome::Completed;
    };
    if preflight_failed
        .lock()
        .expect("preflight set")
        .contains(&entry_index)
    {
        return NodeOutcome::Completed;
    }
    let Some(runtime) = multipart_runtimes
        .get(entry_index)
        .and_then(|r| r.as_ref())
        .cloned()
    else {
        return NodeOutcome::Completed;
    };
    let entry = entries[entry_index].clone();
    let state = Arc::clone(&runtime.state);

    // Already failed: drain as no-op so siblings finish before commit.
    if state.has_failure().await {
        return NodeOutcome::Completed;
    }

    // Pre-cancel before begin/lease: leave the file unaccounted (legacy contract).
    // After begin, record cancellation so CommitTemp aborts once.
    let cancelled_now = cancel.load(Ordering::Relaxed) || executor.is_transfer_cancelled();
    if cancelled_now {
        if state.is_begun() {
            state.record_failure(cancelled_failure()).await;
        }
        return NodeOutcome::Completed;
    }

    let Some(part_number) = state.part_number_for_node(node_id) else {
        state
            .record_failure(TransferFailure {
                kind: TransferFailureKind::Unknown,
                message: format!("UploadPart node {node_id} not mapped to a part number"),
                retryable: false,
                retry_after_secs: None,
            })
            .await;
        return NodeOutcome::Completed;
    };

    // Serialize the one job-level governor acquire with the multipart file
    // state. Once acquired it remains held until CommitTemp has completed or
    // aborted the file, so parallel parts cannot each consume a fresh endpoint
    // slot or evade the local disk-device cap.
    {
        let mut lease = runtime.governor_lease.lock().await;
        if lease.is_none() {
            let endpoint = executor.endpoint_identity().await;
            let acquired = crate::transfer_dag::governor::global()
                .acquire_job(
                    endpoint,
                    TransferPriority::Background,
                    [DiskLeaseRequest::read(entry.local_path.clone())],
                )
                .await;
            *lease = Some(acquired);
        }
    }

    // One session lease per multipart file (file/session dimension). Holding the
    // mutex during acquire serialises siblings so they never race the slot.
    {
        let mut slot = runtime.session_lease.lock().await;
        match &*slot {
            SessionLeaseSlot::Held(_) => {}
            SessionLeaseSlot::Failed => {
                return NodeOutcome::Completed;
            }
            SessionLeaseSlot::Vacant => {
                // Re-check cancel before acquiring a lease for an unstarted file.
                if cancel.load(Ordering::Relaxed) || executor.is_transfer_cancelled() {
                    return NodeOutcome::Completed;
                }
                match session_pool.acquire().await {
                    Ok(lease) => {
                        *slot = SessionLeaseSlot::Held(lease);
                        let mut snapshot = progress.lock().await;
                        snapshot.active += 1;
                    }
                    Err(error) => {
                        tracing::warn!("Multipart session acquisition failed: {}", error);
                        *slot = SessionLeaseSlot::Failed;
                        *runtime.governor_lease.lock().await = None;
                        state
                            .record_failure(TransferFailure {
                                kind: TransferFailureKind::Unknown,
                                message: format!("Transfer session acquisition failed: {error}"),
                                retryable: true,
                                retry_after_secs: None,
                            })
                            .await;
                        return NodeOutcome::Completed;
                    }
                }
            }
        }
    }

    if cancel.load(Ordering::Relaxed) || executor.is_transfer_cancelled() {
        if state.is_begun() {
            state.record_failure(cancelled_failure()).await;
        }
        return NodeOutcome::Completed;
    }
    if state.has_failure().await {
        return NodeOutcome::Completed;
    }

    // Lazy begin once under the begin gate.
    ensure_multipart_begun(executor, &entry, &state).await;

    if state.has_failure().await {
        return NodeOutcome::Completed;
    }
    if cancel.load(Ordering::Relaxed) || executor.is_transfer_cancelled() {
        if state.is_begun() {
            state.record_failure(cancelled_failure()).await;
        }
        return NodeOutcome::Completed;
    }

    let handle = match state.clone_handle().await {
        Some(h) => h,
        None => {
            if !state.has_failure().await {
                state
                    .record_failure(TransferFailure {
                        kind: TransferFailureKind::Unknown,
                        message: "Multipart session was not begun".to_string(),
                        retryable: false,
                        retry_after_secs: None,
                    })
                    .await;
            }
            return NodeOutcome::Completed;
        }
    };

    let (offset, len) = match state.layout().part_range(part_number) {
        Ok(range) => range,
        Err(failure) => {
            state.record_failure(failure).await;
            return NodeOutcome::Completed;
        }
    };

    let data = match read_chunk(&entry.local_path, offset, len).await {
        Ok(buf) => buf,
        Err(error) => {
            state
                .record_failure(crate::transfer_multipart::transfer_failure_from_provider(
                    &error,
                    Some(&entry.local_path),
                ))
                .await;
            return NodeOutcome::Completed;
        }
    };

    if cancel.load(Ordering::Relaxed) || executor.is_transfer_cancelled() {
        // Session already begun: cancel is a terminal file failure.
        state.record_failure(cancelled_failure()).await;
        return NodeOutcome::Completed;
    }

    match executor
        .multipart_upload_part(&entry, &handle, part_number, data)
        .await
    {
        Ok(receipt) => {
            if let Err(failure) = state.store_receipt_for_part(part_number, receipt).await {
                state.record_failure(failure).await;
            }
        }
        Err(failure) => {
            state.record_failure(failure).await;
        }
    }

    // File failures still return Completed so sibling parts and unrelated
    // files drain (DAG-P1-04 policy). Abort is deferred to CommitTemp.
    NodeOutcome::Completed
}

/// Serialize lazy begin so only one part opens the provider session.
async fn ensure_multipart_begun<E>(
    executor: &Arc<E>,
    entry: &crate::transfer_domain::TransferEntry,
    state: &MultipartFileState,
) where
    E: TransferExecutor + Send + Sync + 'static,
{
    state
        .with_begin_gate(async {
            if !state.needs_begin().await || state.has_failure().await {
                return;
            }
            if state.claim_start_event() {
                executor.multipart_emit_file_start(entry, state.layout().total_size);
            }
            if executor.is_transfer_cancelled() {
                state.record_failure(cancelled_failure()).await;
                return;
            }
            match executor.multipart_begin(entry, state.layout()).await {
                Ok(handle) => {
                    state.install_handle(handle).await;
                }
                Err(failure) => {
                    state.record_failure(failure).await;
                }
            }
        })
        .await;
}

#[allow(clippy::too_many_arguments)]
async fn run_multipart_commit_node<E>(
    node_id: usize,
    node_to_entry: &HashMap<usize, usize>,
    entries: &[crate::transfer_domain::TransferEntry],
    multipart_runtimes: &[Option<Arc<MultipartFileRuntime>>],
    preflight_failed: &std::sync::Mutex<std::collections::HashSet<usize>>,
    executor: &Arc<E>,
    cancel: &AtomicBool,
    progress: &Mutex<BatchProgressSnapshot>,
    sink: &Arc<dyn TransferEventSink>,
    progress_observer: Option<&ProgressObserver>,
) -> NodeOutcome
where
    E: TransferExecutor + Send + Sync + 'static,
{
    let Some(&entry_index) = node_to_entry.get(&node_id) else {
        return NodeOutcome::Completed;
    };
    if preflight_failed
        .lock()
        .expect("preflight set")
        .contains(&entry_index)
    {
        return NodeOutcome::Completed;
    }
    let Some(runtime) = multipart_runtimes
        .get(entry_index)
        .and_then(|r| r.as_ref())
        .cloned()
    else {
        return NodeOutcome::Completed;
    };
    let entry = entries[entry_index].clone();
    let state = Arc::clone(&runtime.state);

    // All part nodes have drained (DAG dependency). Decide complete vs abort.
    let cancel_flag = cancel.load(Ordering::Relaxed) || executor.is_transfer_cancelled();
    let lease_was_held = {
        let slot = runtime.session_lease.lock().await;
        matches!(*slot, SessionLeaseSlot::Held(_))
    };

    // Pre-cancel / cancel-before-begin: leave the file unaccounted (legacy).
    if !state.is_begun() && !state.has_failure().await && (cancel_flag || !lease_was_held) {
        if lease_was_held {
            let mut snapshot = progress.lock().await;
            snapshot.active = snapshot.active.saturating_sub(1);
        }
        let mut slot = runtime.session_lease.lock().await;
        *slot = SessionLeaseSlot::Vacant;
        *runtime.governor_lease.lock().await = None;
        return NodeOutcome::Completed;
    }

    if cancel_flag {
        state.record_failure(cancelled_failure()).await;
    }

    let outcome = decide_multipart_file_outcome(executor, &entry, &state).await;
    // Capture the node outcome before moving `outcome` into accounting.
    let node_outcome = node_outcome_for_file_result(&outcome);

    if state.claim_account() {
        let bytes = match &outcome {
            TransferOutcome::Success => entry.size,
            _ => 0,
        };
        account_outcome(
            progress,
            sink,
            progress_observer,
            outcome,
            bytes,
            lease_was_held,
        )
        .await;
    }

    // Release the one file/session lease after terminal decision (and after
    // the single file-class feedback will be applied by the executor).
    {
        let mut slot = runtime.session_lease.lock().await;
        *slot = SessionLeaseSlot::Vacant;
    }
    *runtime.governor_lease.lock().await = None;
    node_outcome
}

/// Map a batch file terminal into the DAG node outcome (DAG-P1-04).
///
/// Success and skip remain honest healthy completions. Failures — including
/// cancellation — become [`NodeOutcome::FileFailedButGraphContinues`] so the
/// executor can count them, release dependents, and apply at most one
/// file-class AIMD decrease for the D2 congestion set. Graph-fatal
/// [`NodeOutcome::Failed`] is reserved for true scheduler/engine faults.
fn node_outcome_for_file_result(outcome: &TransferOutcome) -> NodeOutcome {
    match outcome {
        TransferOutcome::Success | TransferOutcome::Skipped { .. } => NodeOutcome::Completed,
        TransferOutcome::Failed(failure) => {
            NodeOutcome::FileFailedButGraphContinues(failure.to_transfer_error())
        }
    }
}

async fn decide_multipart_file_outcome<E>(
    executor: &Arc<E>,
    entry: &crate::transfer_domain::TransferEntry,
    state: &MultipartFileState,
) -> TransferOutcome
where
    E: TransferExecutor + Send + Sync + 'static,
{
    if state.has_failure().await {
        if let Some(handle) = state.take_for_abort().await {
            executor.multipart_abort(entry, handle).await;
        }
        let failure = state
            .take_first_failure()
            .await
            .unwrap_or_else(cancelled_failure);
        executor.multipart_emit_file_error(entry, &failure);
        return TransferOutcome::Failed(failure);
    }

    if !state.has_all_receipts().await {
        let count = state.receipt_count().await;
        let failure = TransferFailure {
            kind: TransferFailureKind::Unknown,
            message: format!(
                "multipart receipt count {count} != expected {}",
                state.layout().total_parts
            ),
            retryable: false,
            retry_after_secs: None,
        };
        if let Some(handle) = state.take_for_abort().await {
            executor.multipart_abort(entry, handle).await;
        }
        executor.multipart_emit_file_error(entry, &failure);
        return TransferOutcome::Failed(failure);
    }

    let Some(handle) = state.clone_handle().await else {
        let failure = TransferFailure {
            kind: TransferFailureKind::Unknown,
            message: "multipart complete without handle".to_string(),
            retryable: false,
            retry_after_secs: None,
        };
        executor.multipart_emit_file_error(entry, &failure);
        return TransferOutcome::Failed(failure);
    };

    let parts = state.take_sorted_receipts().await;
    match executor.multipart_complete(entry, handle, parts).await {
        Ok(()) => {
            state.clear_handle_after_complete().await;
            executor.multipart_emit_file_complete(entry);
            TransferOutcome::Success
        }
        Err(failure) => {
            // Complete-time failure keeps the handle available for one abort.
            if let Some(handle) = state.take_for_abort().await {
                executor.multipart_abort(entry, handle).await;
            }
            executor.multipart_emit_file_error(entry, &failure);
            TransferOutcome::Failed(failure)
        }
    }
}

async fn account_outcome(
    progress: &Mutex<BatchProgressSnapshot>,
    sink: &Arc<dyn TransferEventSink>,
    progress_observer: Option<&ProgressObserver>,
    outcome: TransferOutcome,
    success_bytes: u64,
    dec_active: bool,
) {
    let snapshot_clone = {
        let mut snapshot = progress.lock().await;
        if dec_active {
            snapshot.active = snapshot.active.saturating_sub(1);
        }
        match &outcome {
            TransferOutcome::Success => {
                snapshot.completed += 1;
                snapshot.bytes_transferred += success_bytes;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{MultipartHandle, UploadedPart};
    use crate::transfer_dag::TransferSessionPoolHandle;
    use crate::transfer_domain::{
        TransferBatchConfig, TransferEntry, TransferFailure, TransferFailureKind,
    };
    use crate::transfer_multipart::MultipartLayout;
    use crate::TransferEvent;
    use async_trait::async_trait;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    /// (file id, part number) → (failure kind, optional Retry-After secs).
    type TypedPartFailMap =
        std::collections::HashMap<(String, u32), (TransferFailureKind, Option<u64>)>;

    /// Scripted whole-file executor with optional multipart wire log.
    struct MockExecutor {
        fail: HashSet<String>,
        /// Typed whole-file failure overrides (kind + optional Retry-After secs).
        fail_typed: StdMutex<std::collections::HashMap<String, (TransferFailureKind, Option<u64>)>>,
        skip: HashSet<String>,
        executed: StdMutex<Vec<String>>,
        in_flight: AtomicUsize,
        peak: AtomicUsize,
        session_capacity: usize,
        capabilities: TransferCapabilities,
        transfer_calls: AtomicUsize,
        /// When true, implements multipart wire methods.
        multipart_wire: bool,
        begin_calls: AtomicUsize,
        part_calls: AtomicUsize,
        complete_calls: AtomicUsize,
        abort_calls: AtomicUsize,
        part_numbers: StdMutex<Vec<u32>>,
        part_byte_lens: StdMutex<Vec<u64>>,
        part_in_flight: AtomicUsize,
        part_peak: AtomicUsize,
        part_delay: Duration,
        fail_begin_for: StdMutex<HashSet<String>>,
        fail_part: StdMutex<Option<(String, u32)>>,
        /// Typed part failure overrides keyed by (file id, part number).
        fail_part_typed: StdMutex<TypedPartFailMap>,
        fail_complete_for: StdMutex<HashSet<String>>,
        cancelled: AtomicBool,
    }

    impl MockExecutor {
        fn new(session_capacity: usize) -> Self {
            Self {
                fail: HashSet::new(),
                fail_typed: StdMutex::new(std::collections::HashMap::new()),
                skip: HashSet::new(),
                executed: StdMutex::new(Vec::new()),
                in_flight: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                session_capacity,
                capabilities: TransferCapabilities::default(),
                transfer_calls: AtomicUsize::new(0),
                multipart_wire: false,
                begin_calls: AtomicUsize::new(0),
                part_calls: AtomicUsize::new(0),
                complete_calls: AtomicUsize::new(0),
                abort_calls: AtomicUsize::new(0),
                part_numbers: StdMutex::new(Vec::new()),
                part_byte_lens: StdMutex::new(Vec::new()),
                part_in_flight: AtomicUsize::new(0),
                part_peak: AtomicUsize::new(0),
                part_delay: Duration::from_millis(15),
                fail_begin_for: StdMutex::new(HashSet::new()),
                fail_part: StdMutex::new(None),
                fail_part_typed: StdMutex::new(std::collections::HashMap::new()),
                fail_complete_for: StdMutex::new(HashSet::new()),
                cancelled: AtomicBool::new(false),
            }
        }

        fn set_typed_fail(
            &self,
            id: &str,
            kind: TransferFailureKind,
            retry_after_secs: Option<u64>,
        ) {
            self.fail_typed
                .lock()
                .unwrap()
                .insert(id.to_string(), (kind, retry_after_secs));
        }

        fn set_typed_part_fail(
            &self,
            id: &str,
            part: u32,
            kind: TransferFailureKind,
            retry_after_secs: Option<u64>,
        ) {
            self.fail_part_typed
                .lock()
                .unwrap()
                .insert((id.to_string(), part), (kind, retry_after_secs));
        }

        fn with_capabilities(mut self, capabilities: TransferCapabilities) -> Self {
            self.capabilities = capabilities;
            self
        }

        fn with_multipart_wire(mut self) -> Self {
            self.multipart_wire = true;
            self
        }

        fn executed(&self) -> Vec<String> {
            self.executed.lock().unwrap().clone()
        }

        fn peak(&self) -> usize {
            self.peak.load(AtomicOrdering::SeqCst)
        }

        fn transfer_calls(&self) -> usize {
            self.transfer_calls.load(AtomicOrdering::SeqCst)
        }

        fn part_peak(&self) -> usize {
            self.part_peak.load(AtomicOrdering::SeqCst)
        }
    }

    #[async_trait]
    impl TransferExecutor for MockExecutor {
        async fn execute(&self, entry: TransferEntry) -> TransferOutcome {
            self.transfer_calls.fetch_add(1, AtomicOrdering::SeqCst);
            let now = self.in_flight.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            self.peak.fetch_max(now, AtomicOrdering::SeqCst);
            tokio::time::sleep(Duration::from_millis(15)).await;
            self.in_flight.fetch_sub(1, AtomicOrdering::SeqCst);
            self.executed.lock().unwrap().push(entry.id.clone());

            if let Some((kind, retry_after)) =
                self.fail_typed.lock().unwrap().get(&entry.id).copied()
            {
                let mut failure = TransferFailure::new(
                    kind,
                    crate::transfer_domain::user_facing_transfer_failure_message(&kind),
                    true,
                );
                failure.retry_after_secs = retry_after;
                TransferOutcome::Failed(failure)
            } else if self.fail.contains(&entry.id) {
                TransferOutcome::Failed(TransferFailure::new(
                    TransferFailureKind::Unknown,
                    "mock failure",
                    false,
                ))
            } else if self.skip.contains(&entry.id) {
                TransferOutcome::Skipped {
                    reason: "mock skip".to_string(),
                }
            } else {
                TransferOutcome::Success
            }
        }

        fn transfer_capabilities(&self) -> TransferCapabilities {
            self.capabilities.clone()
        }

        fn session_pool(&self, max_concurrent: usize) -> TransferSessionPoolHandle {
            TransferSessionPoolHandle::http_clone(
                "mock",
                self.session_capacity.min(max_concurrent).max(1),
            )
        }

        fn supports_multipart_wire(&self) -> bool {
            self.multipart_wire
        }

        fn is_transfer_cancelled(&self) -> bool {
            self.cancelled.load(Ordering::Relaxed)
        }

        async fn multipart_begin(
            &self,
            entry: &TransferEntry,
            _layout: &MultipartLayout,
        ) -> Result<MultipartHandle, TransferFailure> {
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(cancelled_failure());
            }
            if self.fail_begin_for.lock().unwrap().contains(&entry.id) {
                return Err(TransferFailure::new(
                    TransferFailureKind::RemoteIo,
                    "mock begin failure",
                    false,
                ));
            }
            self.begin_calls.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(MultipartHandle {
                upload_id: format!("up-{}", entry.id),
                remote_path: entry.remote_path.clone(),
            })
        }

        async fn multipart_upload_part(
            &self,
            entry: &TransferEntry,
            _handle: &MultipartHandle,
            part_number: u32,
            data: Vec<u8>,
        ) -> Result<UploadedPart, TransferFailure> {
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(cancelled_failure());
            }
            if let Some((kind, retry_after)) = self
                .fail_part_typed
                .lock()
                .unwrap()
                .get(&(entry.id.clone(), part_number))
                .copied()
            {
                let mut failure = TransferFailure::new(
                    kind,
                    crate::transfer_domain::user_facing_transfer_failure_message(&kind),
                    true,
                );
                failure.retry_after_secs = retry_after;
                return Err(failure);
            }
            if let Some((ref id, n)) = *self.fail_part.lock().unwrap() {
                if id == &entry.id && n == part_number {
                    return Err(TransferFailure::new(
                        TransferFailureKind::RemoteIo,
                        format!("mock part {part_number} failure"),
                        false,
                    ));
                }
            }
            let now = self.part_in_flight.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            self.part_peak.fetch_max(now, AtomicOrdering::SeqCst);
            self.part_calls.fetch_add(1, AtomicOrdering::SeqCst);
            self.part_numbers.lock().unwrap().push(part_number);
            self.part_byte_lens.lock().unwrap().push(data.len() as u64);
            tokio::time::sleep(self.part_delay).await;
            self.part_in_flight.fetch_sub(1, AtomicOrdering::SeqCst);
            // Intentionally return receipt with part_number as-is; complete sorts.
            Ok(UploadedPart {
                part_number,
                etag: format!("etag-{part_number}"),
            })
        }

        async fn multipart_complete(
            &self,
            entry: &TransferEntry,
            _handle: MultipartHandle,
            parts: Vec<UploadedPart>,
        ) -> Result<(), TransferFailure> {
            if self.fail_complete_for.lock().unwrap().contains(&entry.id) {
                return Err(TransferFailure::new(
                    TransferFailureKind::RemoteIo,
                    "mock complete failure",
                    false,
                ));
            }
            // Prove receipts arrive sorted.
            let nums: Vec<u32> = parts.iter().map(|p| p.part_number).collect();
            let mut sorted = nums.clone();
            sorted.sort();
            assert_eq!(nums, sorted, "complete must receive sorted receipts");
            self.complete_calls.fetch_add(1, AtomicOrdering::SeqCst);
            self.executed.lock().unwrap().push(entry.id.clone());
            Ok(())
        }

        async fn multipart_abort(&self, _entry: &TransferEntry, _handle: MultipartHandle) {
            self.abort_calls.fetch_add(1, AtomicOrdering::SeqCst);
        }
    }

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

    fn entry_with_local(id: &str, size: u64, local_path: &str) -> TransferEntry {
        TransferEntry {
            id: id.to_string(),
            display_name: id.to_string(),
            remote_path: format!("/remote/{id}"),
            local_path: local_path.to_string(),
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

    fn upload_batch(entries: Vec<TransferEntry>, max_concurrent: u32) -> TransferBatch {
        TransferBatch {
            id: "batch-up".to_string(),
            display_name: "Upload batch".to_string(),
            direction: DomainTransferDirection::Upload,
            config: TransferBatchConfig {
                max_concurrent,
                max_retries: 0,
                timeout_ms: 30_000,
            },
            entries,
        }
    }

    fn multipart_caps(max_chunk: u16, chunk_size: u64) -> TransferCapabilities {
        TransferCapabilities {
            file_parallel: crate::transfer_dag::Capability::Supported,
            session_pool: crate::transfer_dag::Capability::Supported,
            multipart_upload: crate::transfer_dag::Capability::Supported,
            max_file_slots: Some(8),
            max_chunk_slots: Some(max_chunk),
            preferred_chunk_size: Some(chunk_size),
            multipart_threshold: 0,
            ..TransferCapabilities::default()
        }
    }

    fn write_temp_file(bytes: usize) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("source.bin");
        let data: Vec<u8> = (0..bytes).map(|i| (i % 256) as u8).collect();
        std::fs::write(&path, data).expect("write");
        (dir, path)
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
        assert_eq!(sink.started.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(sink.completed.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(sink.progress.load(AtomicOrdering::SeqCst), 3);
    }

    #[tokio::test]
    async fn batch_dag_failed_file_does_not_abort_remaining() {
        let mut executor = MockExecutor::new(1);
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

    use crate::transfer_dag::session_pool::{
        SessionLeaseKind, SessionPoolCapacity, SessionPoolError, TransferSessionLease,
        TransferSessionPool,
    };

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

    #[test]
    fn default_executor_capabilities_are_conservative_serial() {
        let executor = MockExecutor::new(1);
        let caps = executor.transfer_capabilities();
        assert_eq!(
            caps.file_parallel,
            crate::transfer_dag::Capability::Unsupported
        );
        assert_eq!(caps.max_file_slots, Some(1));
        assert_eq!(
            caps.multipart_upload,
            crate::transfer_dag::Capability::Unsupported
        );
        assert!(!executor.supports_multipart_wire());
    }

    #[tokio::test]
    async fn batch_dag_uses_executor_capability_snapshot_not_defaults() {
        let caps = multipart_caps(4, 5 * 1024 * 1024);
        let file_size = 20 * 1024 * 1024;
        let (_dir, path) = write_temp_file(file_size as usize);
        let executor = Arc::new(
            MockExecutor::new(4)
                .with_capabilities(caps)
                .with_multipart_wire(),
        );
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![entry_with_local("big", file_size, path.to_str().unwrap())],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.completed, 1);
        // Real per-part I/O: 1 begin + N parts + 1 complete, zero whole-file.
        assert_eq!(executor.transfer_calls(), 0);
        assert_eq!(executor.begin_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(executor.part_calls.load(AtomicOrdering::SeqCst), 4);
        assert_eq!(executor.complete_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(executor.abort_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    async fn batch_dag_independent_file_overlap_for_enabled_executor() {
        let caps = TransferCapabilities {
            file_parallel: crate::transfer_dag::Capability::Supported,
            session_pool: crate::transfer_dag::Capability::Supported,
            max_file_slots: Some(4),
            ..TransferCapabilities::default()
        };
        let executor = Arc::new(MockExecutor::new(4).with_capabilities(caps));
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            batch(
                vec![entry("a", 1), entry("b", 1), entry("c", 1), entry("d", 1)],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.completed, 4);
        assert!(
            executor.peak() > 1,
            "independent files must overlap for a pool-enabled executor (peak={})",
            executor.peak()
        );
        assert!(executor.peak() <= 4);
    }

    #[tokio::test]
    async fn batch_dag_peak_bounded_by_min_of_file_slots_and_session_pool() {
        let caps = TransferCapabilities {
            file_parallel: crate::transfer_dag::Capability::Supported,
            session_pool: crate::transfer_dag::Capability::Supported,
            max_file_slots: Some(8),
            ..TransferCapabilities::default()
        };
        let executor = Arc::new(MockExecutor::new(2).with_capabilities(caps));
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            batch(
                vec![entry("a", 1), entry("b", 1), entry("c", 1), entry("d", 1)],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.completed, 4);
        assert_eq!(executor.peak(), 2);
    }

    #[tokio::test]
    async fn batch_dag_session_pool_acquire_failure_is_counted_in_snapshot() {
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
        assert_eq!(result.failed, 3);
        assert_eq!(result.completed, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(sink.progress.load(AtomicOrdering::SeqCst), 3);
        assert_eq!(observed.load(AtomicOrdering::SeqCst), 3);
    }

    // ---------------- DAG-P1-03 multipart wire tests -------------------------

    #[tokio::test]
    async fn multipart_batch_one_begin_n_parts_one_complete_zero_aborts() {
        let chunk = 8 * 1024;
        let file_size = 24 * 1024; // 3 parts
        let (_dir, path) = write_temp_file(file_size);
        let executor = Arc::new(
            MockExecutor::new(4)
                .with_capabilities(multipart_caps(4, chunk as u64))
                .with_multipart_wire(),
        );
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![entry_with_local(
                    "m",
                    file_size as u64,
                    path.to_str().unwrap(),
                )],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.completed, 1);
        assert_eq!(result.failed, 0);
        assert_eq!(executor.begin_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(executor.part_calls.load(AtomicOrdering::SeqCst), 3);
        assert_eq!(executor.complete_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(executor.abort_calls.load(AtomicOrdering::SeqCst), 0);
        let mut nums = executor.part_numbers.lock().unwrap().clone();
        nums.sort();
        assert_eq!(nums, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn multipart_batch_exact_byte_ranges_match_layout_helper() {
        use crate::transfer_dag::multipart_part_byte_len;
        let chunk = 10u64;
        let file_size = 25u64; // parts: 10, 10, 5
        let (_dir, path) = write_temp_file(file_size as usize);
        let executor = Arc::new(
            MockExecutor::new(4)
                .with_capabilities(multipart_caps(4, chunk))
                .with_multipart_wire(),
        );
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![entry_with_local("r", file_size, path.to_str().unwrap())],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.completed, 1);
        let lens = executor.part_byte_lens.lock().unwrap().clone();
        let expected: Vec<u64> = (0..3)
            .map(|i| multipart_part_byte_len(file_size, i, 3, chunk))
            .collect();
        // Order of completion may differ; compare as multisets.
        let mut got = lens;
        let mut exp = expected;
        got.sort();
        exp.sort();
        assert_eq!(got, exp);
        assert_eq!(got.iter().sum::<u64>(), file_size);
    }

    #[tokio::test]
    async fn multipart_batch_cap1_preserves_monotonic_wire_order() {
        let chunk = 8u64;
        let file_size = 24u64;
        let (_dir, path) = write_temp_file(file_size as usize);
        let mut exec = MockExecutor::new(4)
            .with_capabilities(multipart_caps(1, chunk))
            .with_multipart_wire();
        exec.part_delay = Duration::from_millis(5);
        let executor = Arc::new(exec);
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![entry_with_local("s", file_size, path.to_str().unwrap())],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.completed, 1);
        let nums = executor.part_numbers.lock().unwrap().clone();
        assert_eq!(nums, vec![1, 2, 3], "cap=1 must wire parts in order");
        assert_eq!(executor.part_peak(), 1);
    }

    #[tokio::test]
    async fn multipart_batch_cap_gt1_shows_real_part_overlap() {
        let chunk = 8u64;
        let file_size = 32u64; // 4 parts
        let (_dir, path) = write_temp_file(file_size as usize);
        let mut exec = MockExecutor::new(4)
            .with_capabilities(multipart_caps(4, chunk))
            .with_multipart_wire();
        exec.part_delay = Duration::from_millis(40);
        let executor = Arc::new(exec);
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![entry_with_local("o", file_size, path.to_str().unwrap())],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.completed, 1);
        assert!(
            executor.part_peak() > 1,
            "cap>1 must overlap parts (peak={})",
            executor.part_peak()
        );
        assert!(executor.part_peak() <= 4);
    }

    #[tokio::test]
    async fn multipart_batch_budget_exposes_chunk_slots() {
        let config = TransferBatchConfig {
            max_concurrent: 3,
            max_retries: 0,
            timeout_ms: 30_000,
        };
        let caps = multipart_caps(4, 1024);
        let budget = config.transfer_budget_for_capabilities(&caps);
        assert_eq!(budget.file_slots, 3);
        assert_eq!(budget.chunk_slots, 4);
        assert!(budget.disk_read_slots >= 4);
    }

    #[tokio::test]
    async fn multipart_batch_begin_failure_no_part_complete_one_failed() {
        let chunk = 8u64;
        let file_size = 24u64;
        let (_dir, path) = write_temp_file(file_size as usize);
        let exec = MockExecutor::new(4)
            .with_capabilities(multipart_caps(4, chunk))
            .with_multipart_wire();
        exec.fail_begin_for
            .lock()
            .unwrap()
            .insert("bad".to_string());
        let executor = Arc::new(exec);
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![entry_with_local("bad", file_size, path.to_str().unwrap())],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.failed, 1);
        assert_eq!(result.completed, 0);
        assert_eq!(executor.part_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(executor.complete_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(executor.abort_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    async fn multipart_batch_part_failure_aborts_once_and_continues_sibling_file() {
        let chunk = 8u64;
        let file_size = 24u64;
        let (_dir, path_a) = write_temp_file(file_size as usize);
        let (_dir2, path_b) = write_temp_file(file_size as usize);
        let exec = MockExecutor::new(4)
            .with_capabilities(multipart_caps(4, chunk))
            .with_multipart_wire();
        *exec.fail_part.lock().unwrap() = Some(("fail".to_string(), 2));
        let executor = Arc::new(exec);
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![
                    entry_with_local("fail", file_size, path_a.to_str().unwrap()),
                    entry_with_local("ok", file_size, path_b.to_str().unwrap()),
                ],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.failed, 1);
        assert_eq!(result.completed, 1);
        assert_eq!(executor.abort_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(executor.complete_calls.load(AtomicOrdering::SeqCst), 1);
        assert!(executor.executed().contains(&"ok".to_string()));
    }

    #[tokio::test]
    async fn multipart_batch_complete_failure_aborts_once() {
        let chunk = 8u64;
        let file_size = 16u64;
        let (_dir, path) = write_temp_file(file_size as usize);
        let exec = MockExecutor::new(4)
            .with_capabilities(multipart_caps(4, chunk))
            .with_multipart_wire();
        exec.fail_complete_for
            .lock()
            .unwrap()
            .insert("c".to_string());
        let executor = Arc::new(exec);
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![entry_with_local("c", file_size, path.to_str().unwrap())],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.failed, 1);
        assert_eq!(executor.complete_calls.load(AtomicOrdering::SeqCst), 0);
        // complete was attempted (fail before increment) — check abort
        assert_eq!(executor.abort_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(executor.part_calls.load(AtomicOrdering::SeqCst), 2);
    }

    #[tokio::test]
    async fn multipart_batch_pre_cancel_no_begin_part_complete_abort() {
        let chunk = 8u64;
        let file_size = 24u64;
        let (_dir, path) = write_temp_file(file_size as usize);
        let executor = Arc::new(
            MockExecutor::new(4)
                .with_capabilities(multipart_caps(4, chunk))
                .with_multipart_wire(),
        );
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![entry_with_local("x", file_size, path.to_str().unwrap())],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(true)),
            None,
        )
        .await;
        assert!(result.cancelled);
        assert_eq!(result.completed, 0);
        assert_eq!(result.failed, 0);
        assert_eq!(executor.begin_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(executor.part_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(executor.complete_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(executor.abort_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    async fn multipart_batch_progress_once_per_file_bytes_after_complete() {
        let chunk = 8u64;
        let file_size = 24u64;
        let (_dir, path) = write_temp_file(file_size as usize);
        let executor = Arc::new(
            MockExecutor::new(4)
                .with_capabilities(multipart_caps(4, chunk))
                .with_multipart_wire(),
        );
        let sink = Arc::new(CountingSink::default());
        let result = execute_batch_dag(
            Arc::clone(&sink) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![entry_with_local("p", file_size, path.to_str().unwrap())],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.completed, 1);
        // One progress emission for the one file (not per part).
        assert_eq!(sink.progress.load(AtomicOrdering::SeqCst), 1);
        // Bytes credited after complete (via result bookkeeping in snapshot).
        // We don't expose snapshot; completed=1 and size accounted in run.
        assert_eq!(result.failed, 0);
    }

    #[tokio::test]
    async fn multipart_batch_conservative_executor_fails_preflight() {
        // Multipart caps but no wire support → fail closed, no silent default.
        let caps = multipart_caps(4, 8);
        let file_size = 24u64;
        let (_dir, path) = write_temp_file(file_size as usize);
        let executor = Arc::new(MockExecutor::new(4).with_capabilities(caps));
        assert!(!executor.supports_multipart_wire());
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![entry_with_local("c", file_size, path.to_str().unwrap())],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.failed, 1);
        assert_eq!(result.completed, 0);
        assert_eq!(executor.begin_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(executor.part_calls.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(executor.transfer_calls(), 0);
    }

    #[tokio::test]
    async fn multipart_batch_session_lease_bounds_concurrent_files() {
        // Session capacity 1: two multipart files must serialize on lease.
        let chunk = 8u64;
        let file_size = 16u64;
        let (_d1, p1) = write_temp_file(file_size as usize);
        let (_d2, p2) = write_temp_file(file_size as usize);
        let mut exec = MockExecutor::new(1)
            .with_capabilities(multipart_caps(4, chunk))
            .with_multipart_wire();
        exec.part_delay = Duration::from_millis(20);
        let executor = Arc::new(exec);
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![
                    entry_with_local("a", file_size, p1.to_str().unwrap()),
                    entry_with_local("b", file_size, p2.to_str().unwrap()),
                ],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.completed, 2);
        // Two begins, two completes — session released between files.
        assert_eq!(executor.begin_calls.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(executor.complete_calls.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(executor.abort_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn whole_file_dedupe_seam_absent_from_production_batch_runner() {
        // Production code must not keep the whole-file multipart no-op seam.
        // Strip the test module so this assertion itself cannot match.
        let src = include_str!("transfer_dag_batch.rs");
        let prod = src.split("#[cfg(test)]").next().expect("prod section");
        assert!(
            !prod.contains("entry_transferred"),
            "production batch runner must not contain the whole-file dedupe seam"
        );
    }

    // ── DAG-P1-04: typed file failures → AIMD without aborting the batch ──

    #[tokio::test]
    async fn p1_04_whole_file_429_halves_file_target_and_sibling_completes() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let executor = MockExecutor::new(8);
        executor.set_typed_fail("bad", TransferFailureKind::RateLimited, Some(12));
        let executor = Arc::new(executor);
        let sink = Arc::new(CountingSink::default());
        let aimd = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));

        let result = execute_batch_dag_with_aimd(
            Arc::clone(&sink) as Arc<dyn TransferEventSink>,
            batch(vec![entry("bad", 100), entry("good", 200)], 8),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
            Arc::clone(&aimd),
        )
        .await;

        assert_eq!(result.failed, 1, "snapshot counts one failure");
        assert_eq!(result.completed, 1, "independent file completes");
        assert!(!result.cancelled);
        assert_eq!(aimd.target(AdaptiveClass::File), 4, "8 → 4 on typed 429");
        // Exactly one progress event per file (failed + success).
        assert_eq!(sink.progress.load(AtomicOrdering::SeqCst), 2);
        let executed = executor.executed();
        assert!(executed.contains(&"good".to_string()));
        assert!(executed.contains(&"bad".to_string()));
    }

    #[tokio::test]
    async fn p1_04_whole_file_non_congestion_does_not_shrink() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        for kind in [
            TransferFailureKind::Auth,
            TransferFailureKind::NotFound,
            TransferFailureKind::PermissionDenied,
            TransferFailureKind::QuotaExceeded,
            TransferFailureKind::LocalIo,
            TransferFailureKind::RemoteIo,
            TransferFailureKind::Unknown,
        ] {
            let executor = MockExecutor::new(8);
            executor.set_typed_fail("bad", kind, None);
            let executor = Arc::new(executor);
            let aimd = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
            let result = execute_batch_dag_with_aimd(
                Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
                batch(vec![entry("bad", 10), entry("good", 20)], 8),
                Arc::clone(&executor),
                Arc::new(AtomicBool::new(false)),
                None,
                Arc::clone(&aimd),
            )
            .await;
            assert_eq!(result.failed, 1, "{kind:?}");
            assert_eq!(result.completed, 1, "{kind:?}");
            assert_eq!(
                aimd.target(AdaptiveClass::File),
                8,
                "{kind:?} must not shrink"
            );
        }
    }

    #[tokio::test]
    async fn p1_04_session_acquire_failure_visible_not_congestion() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let aimd = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
        let result = execute_batch_dag_with_aimd(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            batch(vec![entry("a", 1), entry("b", 1)], 8),
            Arc::new(FailingPoolExecutor),
            Arc::new(AtomicBool::new(false)),
            None,
            Arc::clone(&aimd),
        )
        .await;
        assert_eq!(result.failed, 2);
        assert_eq!(result.completed, 0);
        assert_eq!(
            aimd.target(AdaptiveClass::File),
            8,
            "session-acquire Unknown is not congestion"
        );
    }

    #[tokio::test]
    async fn p1_04_multipart_part_failure_one_file_decrease_not_two() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        // Two parts; both can fail, but commit is the single file terminal →
        // at most one File-class decrease.
        let caps = multipart_caps(4, 8);
        let file_size = 16u64;
        let (_dir, path) = write_temp_file(file_size as usize);
        let executor = MockExecutor::new(4)
            .with_capabilities(caps)
            .with_multipart_wire();
        // Both sibling parts fail with congestion; first-wins records one
        // RateLimited and commit must apply exactly one File-class decrease.
        executor.set_typed_part_fail("mp", 1, TransferFailureKind::RateLimited, Some(20));
        executor.set_typed_part_fail("mp", 2, TransferFailureKind::RateLimited, Some(20));
        let executor = Arc::new(executor);
        let aimd = Arc::new(AimdController::new(8, 4, 1, 1, AimdConfig::default()));

        let result = execute_batch_dag_with_aimd(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![entry_with_local("mp", file_size, path.to_str().unwrap())],
                8,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
            Arc::clone(&aimd),
        )
        .await;

        assert_eq!(result.failed, 1);
        assert_eq!(result.completed, 0);
        assert_eq!(
            executor.abort_calls.load(AtomicOrdering::SeqCst),
            1,
            "abort once after parts drain"
        );
        assert_eq!(
            executor.complete_calls.load(AtomicOrdering::SeqCst),
            0,
            "complete must not run after part failure"
        );
        // One multiplicative decrease (8→4), not two (would be 8→4→2).
        assert_eq!(
            aimd.target(AdaptiveClass::File),
            4,
            "two failing parts → one file-class decrease at CommitTemp"
        );
    }

    #[tokio::test]
    async fn p1_04_multipart_failure_does_not_stop_independent_plain_file() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let caps = multipart_caps(4, 8);
        let file_size = 16u64;
        let (_dir, path) = write_temp_file(file_size as usize);
        let executor = MockExecutor::new(4)
            .with_capabilities(caps)
            .with_multipart_wire();
        // Congestion on one multipart file; independent sibling multipart continues.
        executor.set_typed_part_fail("bad", 1, TransferFailureKind::RateLimited, None);
        let executor = Arc::new(executor);
        let aimd = Arc::new(AimdController::new(8, 4, 1, 1, AimdConfig::default()));
        let (_dir2, path2) = write_temp_file(file_size as usize);

        let result = execute_batch_dag_with_aimd(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![
                    entry_with_local("bad", file_size, path.to_str().unwrap()),
                    entry_with_local("good", file_size, path2.to_str().unwrap()),
                ],
                8,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
            Arc::clone(&aimd),
        )
        .await;

        assert_eq!(result.failed, 1);
        assert_eq!(result.completed, 1, "independent multipart file completes");
        assert_eq!(aimd.target(AdaptiveClass::File), 4);
        assert!(executor.executed().contains(&"good".to_string()));
    }

    #[tokio::test]
    async fn p1_04_multipart_begin_failure_surfaces_once_at_commit() {
        let caps = multipart_caps(4, 8);
        let file_size = 16u64;
        let (_dir, path) = write_temp_file(file_size as usize);
        let executor = MockExecutor::new(4)
            .with_capabilities(caps)
            .with_multipart_wire();
        executor
            .fail_begin_for
            .lock()
            .unwrap()
            .insert("b".to_string());
        let executor = Arc::new(executor);
        let sink = Arc::new(CountingSink::default());
        let result = execute_batch_dag(
            Arc::clone(&sink) as Arc<dyn TransferEventSink>,
            upload_batch(
                vec![entry_with_local("b", file_size, path.to_str().unwrap())],
                4,
            ),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.failed, 1);
        assert_eq!(sink.progress.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(executor.complete_calls.load(AtomicOrdering::SeqCst), 0);
        // begin attempted; abort may or may not run depending on begun flag.
        assert_eq!(executor.part_calls.load(AtomicOrdering::SeqCst), 0);
    }

    #[tokio::test]
    async fn p1_04_successful_and_skipped_not_reported_as_failures() {
        let mut executor = MockExecutor::new(2);
        executor.skip.insert("s".to_string());
        let executor = Arc::new(executor);
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            batch(vec![entry("ok", 10), entry("s", 20)], 2),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await;
        assert_eq!(result.completed, 1);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.failed, 0);
    }

    #[tokio::test]
    async fn p1_04_mixed_success_failure_cancel_totals() {
        let mut executor = MockExecutor::new(4);
        executor.set_typed_fail("f", TransferFailureKind::Timeout, None);
        executor.skip.insert("s".to_string());
        let executor = Arc::new(executor);
        let cancel = Arc::new(AtomicBool::new(false));
        let result = execute_batch_dag(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            batch(vec![entry("ok", 50), entry("f", 60), entry("s", 70)], 4),
            Arc::clone(&executor),
            Arc::clone(&cancel),
            None,
        )
        .await;
        assert_eq!(result.completed, 1);
        assert_eq!(result.failed, 1);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.total, 3);
        assert!(!result.cancelled);
        // Bytes: only the successful file credits size.
        // Snapshot bytes aren't on result; completed/failed/skipped is the contract.
    }

    #[tokio::test]
    async fn p1_04_cancel_flag_is_not_congestion() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let executor = Arc::new(MockExecutor::new(4));
        let aimd = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
        let result = execute_batch_dag_with_aimd(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            batch(vec![entry("a", 1), entry("b", 1)], 8),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(true)),
            None,
            Arc::clone(&aimd),
        )
        .await;
        assert!(result.cancelled);
        assert_eq!(result.failed, 0);
        assert_eq!(result.completed, 0);
        assert_eq!(aimd.target(AdaptiveClass::File), 8);
    }

    #[tokio::test]
    async fn p1_04_aimd_disabled_leaves_ceiling() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let executor = MockExecutor::new(8);
        executor.set_typed_fail("bad", TransferFailureKind::RateLimited, None);
        let executor = Arc::new(executor);
        let cfg = AimdConfig {
            disabled: true,
            ..AimdConfig::default()
        };
        let aimd = Arc::new(AimdController::new(8, 1, 1, 1, cfg));
        let result = execute_batch_dag_with_aimd(
            Arc::new(CountingSink::default()) as Arc<dyn TransferEventSink>,
            batch(vec![entry("bad", 10), entry("good", 20)], 8),
            Arc::clone(&executor),
            Arc::new(AtomicBool::new(false)),
            None,
            Arc::clone(&aimd),
        )
        .await;
        assert_eq!(result.failed, 1);
        assert_eq!(result.completed, 1);
        assert_eq!(aimd.target(AdaptiveClass::File), 8);
    }

    #[test]
    fn p1_04_node_outcome_mapping_success_skip_fail() {
        let ok = node_outcome_for_file_result(&TransferOutcome::Success);
        assert_eq!(ok, NodeOutcome::Completed);

        let skip = node_outcome_for_file_result(&TransferOutcome::Skipped { reason: "x".into() });
        assert_eq!(skip, NodeOutcome::Completed);

        let failure = TransferFailure::new(
            TransferFailureKind::RateLimited,
            "Transfer rate limit reached",
            true,
        )
        .with_retry_after_secs(30);
        let cont = node_outcome_for_file_result(&TransferOutcome::Failed(failure));
        match cont {
            NodeOutcome::FileFailedButGraphContinues(e) => {
                assert_eq!(e.kind, crate::transfer_dag::TransferErrorKind::RateLimited);
                assert_eq!(e.retry_after, Some(Duration::from_secs(30)));
                assert_eq!(e.scope, crate::transfer_dag::FailureScope::File);
            }
            other => panic!("expected continuing failure, got {other:?}"),
        }
    }
}
